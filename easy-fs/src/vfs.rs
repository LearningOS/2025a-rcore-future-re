use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Get inode id of this inode
    pub fn inode_id(&self) -> u32 {
        let fs = self.fs.lock();
        fs.get_inode_id_by_pos(self.block_id as u32, self.block_offset)
    }
    /// Whether this inode is a directory
    pub fn is_dir(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }
    /// Count number of directory entries in root pointing to this inode
    pub fn link_count(&self) -> u32 {
        let id = self.inode_id();
        let root = EasyFileSystem::root_inode(&self.fs);
        root.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            let mut n = 0u32;
            for i in 0..file_count {
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &root.block_device),
                    DIRENT_SZ,
                );
                if dirent.name().len() != 0 && dirent.inode_id() == id {
                    n += 1;
                }
            }
            n
        })
    }
    /// Remove a directory entry `name` from this directory.
    /// If after removal, no more links refer to the target inode,
    /// free its data blocks and deallocate its inode id.
    pub fn unlink(&self, name: &str) -> bool {
        // find target index and inode id
        let opt = self.read_disk_inode(|disk_inode| {
            if !disk_inode.is_dir() {
                return None;
            }
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ,
                );
                if dirent.name() == name {
                    return Some((i, dirent.inode_id(), file_count));
                }
            }
            None
        });
        let (idx, inode_id, file_count) = match opt {
            Some(t) => t,
            None => return false,
        };
        // remove entry by swapping with last and emptying last slot
        self.modify_disk_inode(|disk_inode| {
            if idx != file_count - 1 {
                let mut last = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(
                        (file_count - 1) * DIRENT_SZ,
                        last.as_bytes_mut(),
                        &self.block_device,
                    ),
                    DIRENT_SZ,
                );
                disk_inode.write_at(idx * DIRENT_SZ, last.as_bytes(), &self.block_device);
            }
            let empty = DirEntry::empty();
            disk_inode.write_at(
                (file_count - 1) * DIRENT_SZ,
                empty.as_bytes(),
                &self.block_device,
            );
        });
        block_cache_sync_all();

        // check if there are more links to the same inode id
        let still_exists = self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ,
                );
                if dirent.name().len() != 0 && dirent.inode_id() == inode_id {
                    return true;
                }
            }
            false
        });
        if !still_exists {
            // clear file data and deallocate inode id
            let (bid, boff) = {
                let fs = self.fs.lock();
                fs.get_disk_inode_pos(inode_id)
            };
            let target = Inode::new(bid, boff, self.fs.clone(), self.block_device.clone());
            target.clear();
            let mut fs = self.fs.lock();
            fs.dealloc_inode(inode_id);
            block_cache_sync_all();
        }
        true
    }
    /// Create a hard link under current directory that points to `src`
    /// Returns true on success, false if name exists
    pub fn link(&self, name: &str, src: &Arc<Inode>) -> bool {
        // ensure name not exists in this directory
        let mut fs = self.fs.lock();
        if self.read_disk_inode(|root_inode| self.find_inode_id(name, root_inode)).is_some() {
            return false;
        }
        // compute source inode id from its on-disk position
        let src_id = fs.get_inode_id_by_pos(src.block_id as u32, src.block_offset);
        // append new dirent with existing inode id
        self.modify_disk_inode(|root_inode| {
            assert!(root_inode.is_dir());
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            self.increase_size(new_size as u32, root_inode, &mut fs);
            let dirent = DirEntry::new(name, src_id);
            root_inode.write_at(file_count * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
        });
        block_cache_sync_all();
        true
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
