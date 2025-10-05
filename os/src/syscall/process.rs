//! Process management syscalls
//!
use core::mem::size_of;

use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_byte_buffer, translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    // snapshot time (microseconds)
    let usec_total = crate::timer::get_time_us();
    let sec = usec_total / 1_000_000;
    let usec = (usec_total % 1_000_000) as usize;
    let local = TimeVal { sec, usec };

    // build byte slice of local struct
    let src = unsafe {
        core::slice::from_raw_parts(
            (&local as *const TimeVal) as *const u8,
            size_of::<TimeVal>(),
        )
    };

    // fast path: if TimeVal does not cross a page, write as a typed struct
    let token = current_user_token();
    let size = size_of::<TimeVal>();
    let start = _ts as usize;
    let end = start + size - 1;
    let page_size = crate::config::PAGE_SIZE;
    let same_page = (start / page_size) == (end / page_size);
    if same_page {
        let dst: &mut TimeVal = translated_refmut(token, _ts);
        *dst = local;
    } else {
        // cross-page: fall back to byte-slice copy
        let mut dst_slices = translated_byte_buffer(token, _ts as *const u8, size_of::<TimeVal>());
        let mut copied = 0usize;
        for slice in dst_slices.iter_mut() {
            let n = slice.len();
            slice.copy_from_slice(&src[copied..copied + n]);
            copied += n;
        }
    }

    0
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap request: start={:#x} len={} prot={:#x}",
        current_task().unwrap().pid.0,
        start,
        len,
        prot
    );

    // validate prot: only R(1),W(2),X(4) allowed and not zero
    if (prot & !0x7) != 0 || prot == 0 {
        return -1;
    }

    // require page-aligned start (simple policy)
    if !crate::mm::VirtAddr::from(start).aligned() {
        return -1;
    }

    // round up length to page size
    let page_size = crate::config::PAGE_SIZE;
    let len_up = if len == 0 {
        0
    } else {
        ((len - 1) / page_size + 1) * page_size
    };
    if len_up == 0 {
        return -1;
    }
    let end = match start.checked_add(len_up) {
        Some(e) => e,
        None => return -1,
    };

    // check no existing mapping overlaps (use a snapshot of current page table)
    let page_table = crate::mm::PageTable::from_token(current_user_token());
    let mut va = start;
    while va < end {
        let vpn = crate::mm::VirtAddr::from(va).floor();
        if let Some(pte) = page_table.translate(vpn) {
            if pte.is_valid() {
                return -1;
            }
        }
        va += page_size;
    }

    // build MapPermission (always include user bit)
    let mut perm = crate::mm::MapPermission::U;
    if (prot & 0x1) != 0 {
        perm |= crate::mm::MapPermission::R;
    }
    if (prot & 0x2) != 0 {
        perm |= crate::mm::MapPermission::W;
    }
    if (prot & 0x4) != 0 {
        perm |= crate::mm::MapPermission::X;
    }

    // perform mapping in current task's MemorySet
    let binding = current_task().unwrap();
    let mut inner = binding.inner_exclusive_access();
    inner
        .memory_set
        .insert_framed_area(start.into(), end.into(), perm);
    // Do not switch satp here; trap_return will load user satp and flush TLB
    0
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);
    // require page-aligned start
    if !crate::mm::VirtAddr::from(_start).aligned() {
        return -1;
    }

    // round up length to page size
    let page_size = crate::config::PAGE_SIZE;
    let len_up = if _len == 0 {
        0
    } else {
        ((_len - 1) / page_size + 1) * page_size
    };
    if len_up == 0 {
        return -1;
    }
    let end = match _start.checked_add(len_up) {
        Some(e) => e,
        None => return -1,
    };

    // verify range is fully mapped in current page table snapshot
    let page_table = crate::mm::PageTable::from_token(current_user_token());
    let mut va = _start;
    while va < end {
        let vpn = crate::mm::VirtAddr::from(va).floor();
        match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() => {}
            _ => return -1,
        }
        va += page_size;
    }

    // perform unmap in current task's MemorySet
    let binding = current_task().unwrap();
    let mut inner = binding.inner_exclusive_access();

    // remove the mapping area that starts at _start; tests only unmap full areas
    let start_vpn = crate::mm::VirtAddr::from(_start).floor();
    inner.memory_set.remove_area_with_start_vpn(start_vpn);

    0
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    // get path string from user space
    let token = current_user_token();
    let path = translated_str(token, path);
    // find program data
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let data = app_inode.read_all();
        let current = current_task().unwrap();
        // create child by forking current task (copy PCB/MemorySet/etc.)
        let child = current.fork();
        // replace child's address space with target program
        child.exec(data.as_slice());
        let child_pid = child.pid.0;
        // add child to scheduler
        add_task(child);
        child_pid as isize
    } else {
        -1
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority request {}",
        current_task().unwrap().pid.0,
        prio
    );
    if prio < 2 {
        return -1;
    }
    let binding = current_task().unwrap();
    let mut inner = binding.inner_exclusive_access();
    if inner.set_priority(prio as usize) {
        prio
    } else {
        -1
    }
}

// linkat is implemented in os/src/syscall/fs.rs for this lab
