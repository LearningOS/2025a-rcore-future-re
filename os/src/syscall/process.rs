//! Process management syscalls
use crate::mm::{translated_byte_buffer, MapPermission, PageTable, PTEFlags, VirtAddr};
use crate::task::{
    change_program_brk, current_mmap, current_munmap, current_user_token, exit_current_and_run_next,
    suspend_current_and_run_next, get_syscall_times,
};
use crate::timer::get_time_us;
use crate::config::PAGE_SIZE;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    // get current time in microseconds
    let us = get_time_us();
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    // write TimeVal into user space, considering page split
    let ptr = ts as *const u8;
    let size = core::mem::size_of::<TimeVal>();
    let mut remain = size;
    let mut written = 0usize;
    let buffers = translated_byte_buffer(current_user_token(), ptr, size);
    for buf in buffers {
        let src = unsafe {
            core::slice::from_raw_parts((&tv as *const TimeVal as *const u8).add(written),
                                        remain.min(buf.len()))
        };
        buf[..src.len()].copy_from_slice(src);
        written += src.len();
        remain -= src.len();
        if remain == 0 { break; }
    }
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        // Read a byte from user addr `id` if readable; else -1
        0 => {
            let token = current_user_token();
            let va = VirtAddr::from(id);
            let vpn = va.floor();
            let page_table = PageTable::from_token(token);
            if let Some(pte) = page_table.translate(vpn) {
                let flags = pte.flags();
                if pte.is_valid() && flags.contains(PTEFlags::U) && pte.readable() {
                    let ppn = pte.ppn();
                    let offset = va.page_offset();
                    let byte = ppn.get_bytes_array()[offset];
                    return byte as isize;
                }
            }
            -1
        }
        // Write a byte `data` to user addr `id` if writable; else -1
        1 => {
            let token = current_user_token();
            let va = VirtAddr::from(id);
            let vpn = va.floor();
            let page_table = PageTable::from_token(token);
            if let Some(pte) = page_table.translate(vpn) {
                let flags = pte.flags();
                if pte.is_valid() && flags.contains(PTEFlags::U) && pte.writable() {
                    let ppn = pte.ppn();
                    let offset = va.page_offset();
                    ppn.get_bytes_array()[offset] = (data & 0xff) as u8;
                    return 0;
                }
            }
            -1
        }
        // Return syscall count for id
        2 => get_syscall_times(id) as isize,
        _ => -1,
    }
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    // validate start alignment
    if !VirtAddr::from(start).aligned() {
        return -1;
    }
    // validate prot: only lower 3 bits and not zero
    if (prot & !0x7) != 0 || (prot & 0x7) == 0 {
        return -1;
    }
    // round up len by page size
    let len_up = if len == 0 { 0 } else { ((len - 1) / PAGE_SIZE + 1) * PAGE_SIZE };
    let end = match start.checked_add(len_up) {
        Some(e) => e,
        None => return -1,
    };
    if len_up == 0 {
        return 0;
    }
    // check no overlap with existing mapped pages
    let page_table = PageTable::from_token(current_user_token());
    let mut va = VirtAddr::from(start);
    while va.0 < end {
        let vpn = va.floor();
        if let Some(pte) = page_table.translate(vpn) {
            if pte.is_valid() {
                return -1;
            }
        }
        va.0 += PAGE_SIZE;
    }
    // build permission
    let mut perm = MapPermission::U;
    if (prot & 0x1) != 0 { perm |= MapPermission::R; }
    if (prot & 0x2) != 0 { perm |= MapPermission::W; }
    if (prot & 0x4) != 0 { perm |= MapPermission::X; }
    // perform mapping in current address space
    current_mmap(start, end, perm);
    0
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    // validate alignment
    if !VirtAddr::from(start).aligned() {
        return -1;
    }
    // round up len by page size
    let len_up = if len == 0 { 0 } else { ((len - 1) / PAGE_SIZE + 1) * PAGE_SIZE };
    let end = match start.checked_add(len_up) {
        Some(e) => e,
        None => return -1,
    };
    if len_up == 0 {
        return 0;
    }
    // ensure entire range is currently mapped
    let page_table = PageTable::from_token(current_user_token());
    let mut va = VirtAddr::from(start);
    while va.0 < end {
        let vpn = va.floor();
        match page_table.translate(vpn) {
            Some(pte) if pte.is_valid() => {}
            _ => return -1,
        }
        va.0 += PAGE_SIZE;
    }
    if current_munmap(start, end) { 0 } else { -1 }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
