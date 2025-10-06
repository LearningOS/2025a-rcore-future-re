use core::mem::size_of;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_byte_buffer, translated_ref, translated_refmut, translated_str},
    task::{
        current_process, current_task, current_user_token, exit_current_and_run_next, pid2process,
        suspend_current_and_run_next, SignalFlags,
    },
};
use alloc::{string::String, sync::Arc, vec::Vec};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// exit syscall
///
/// exit the current task and run the next task in task list
pub fn sys_exit(exit_code: i32) -> ! {
    trace!(
        "kernel:pid[{}] sys_exit",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}
/// yield syscall
pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}
/// getpid syscall
pub fn sys_getpid() -> isize {
    trace!(
        "kernel: sys_getpid pid:{}",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    current_task().unwrap().process.upgrade().unwrap().getpid() as isize
}
/// fork child process syscall
pub fn sys_fork() -> isize {
    trace!(
        "kernel:pid[{}] sys_fork",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let current_process = current_process();
    let new_process = current_process.fork();
    let new_pid = new_process.getpid();
    // modify trap context of new_task, because it returns immediately after switching
    let new_process_inner = new_process.inner_exclusive_access();
    let task = new_process_inner.tasks[0].as_ref().unwrap();
    let trap_cx = task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    new_pid as isize
}
/// exec syscall
pub fn sys_exec(path: *const u8, mut args: *const usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_exec",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let path = translated_str(token, path);
    let mut args_vec: Vec<String> = Vec::new();
    loop {
        let arg_str_ptr = *translated_ref(token, args);
        if arg_str_ptr == 0 {
            break;
        }
        args_vec.push(translated_str(token, arg_str_ptr as *const u8));
        unsafe {
            args = args.add(1);
        }
    }
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let process = current_process();
        let argc = args_vec.len();
        process.exec(all_data.as_slice(), args_vec);
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        -1
    }
}

/// waitpid syscall
///
/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let process = current_process();
    // find a child process

    let mut inner = process.inner_exclusive_access();
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
        p.inner_exclusive_access().is_zombie && (pid == -1 || pid as usize == p.getpid())
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

/// kill syscall
pub fn sys_kill(pid: usize, signal: u32) -> isize {
    trace!(
        "kernel:pid[{}] sys_kill",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    if let Some(process) = pid2process(pid) {
        if let Some(flag) = SignalFlags::from_bits(signal) {
            process.inner_exclusive_access().signals |= flag;
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

/// get_time syscall
///
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

/// mmap syscall
///
/// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_process().getpid());
    // validate _port: only R(1),W(2),X(4) allowed and not zero
    if (_port & !0x7) != 0 || _port == 0 {
        return -1;
    }

    // require page-aligned start (simple policy)
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

    // check no existing mapping overlaps (use a snapshot of current page table)
    let page_table = crate::mm::PageTable::from_token(current_user_token());
    let mut va = _start;
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
    if (_port & 0x1) != 0 {
        perm |= crate::mm::MapPermission::R;
    }
    if (_port & 0x2) != 0 {
        perm |= crate::mm::MapPermission::W;
    }
    if (_port & 0x4) != 0 {
        perm |= crate::mm::MapPermission::X;
    }

    // perform mapping in current process's MemorySet
    let process = current_process();
    let mut proc_inner = process.inner_exclusive_access();
    proc_inner
        .memory_set
        .insert_framed_area(_start.into(), end.into(), perm);
    // Do not switch satp here; trap_return will load user satp and flush TLB
    0
}

/// munmap syscall
///
/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
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

    // perform unmap in current process's MemorySet
    let process = current_process();
    let mut proc_inner = process.inner_exclusive_access();

    // remove the mapping area that starts at _start; tests only unmap full areas
    let start_vpn = crate::mm::VirtAddr::from(_start).floor();
    proc_inner.memory_set.remove_area_with_start_vpn(start_vpn);

    0
}

/// change data segment size
// pub fn sys_sbrk(size: i32) -> isize {
//     trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().process.upgrade().unwrap().getpid());
//     if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
//         old_brk as isize
//     } else {
//     -1
// }

/// spawn syscall
/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_process().getpid());
    // get path string from user space
    let token = current_user_token();
    let path = translated_str(token, path);
    // find program data
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let data = app_inode.read_all();
        let parent = current_process();
        // create child by forking current process (copy PCB/MemorySet/etc.)
        let child = parent.fork();
        // replace child's address space with target program
        child.exec(data.as_slice(), Vec::new());
        child.getpid() as isize
    } else {
        -1
    }
}

/// set priority syscall
///
/// YOUR JOB: Set task priority
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority request {}",
        current_process().getpid(),
        prio
    );
    if prio < 2 {
        return -1;
    }
    let process = current_process();
    process.inner_exclusive_access().priority = prio as usize;
    prio
}
