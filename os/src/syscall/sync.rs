use crate::sync::{Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::vec;
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        // ensure tracking vector sized
        if process_inner.mutex_holder.len() <= id {
            process_inner.mutex_holder.resize(id + 1, None);
        }
        process_inner.mutex_holder[id] = None;
        id as isize
    } else {
        process_inner.mutex_list.push(mutex);
        // push corresponding holder slot
        process_inner.mutex_holder.push(None);
        process_inner.mutex_list.len() as isize - 1
    }
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;
    let mut process_inner = process.inner_exclusive_access();
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    // deadlock detection for mutex
    if process_inner.deadlock_detect {
        if process_inner.mutex_holder.len() <= mutex_id {
            process_inner.mutex_holder.resize(mutex_id + 1, None);
        }
        // build waits-for adjacency
        let mut adj: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (wtid, &mid) in process_inner.mutex_waiting.iter() {
            if mid < process_inner.mutex_holder.len() {
                if let Some(h) = process_inner.mutex_holder[mid] {
                    adj.entry(*wtid).or_default().push(h);
                }
            }
        }
        if let Some(h) = process_inner.mutex_holder[mutex_id] {
            adj.entry(tid).or_default().push(h);
        }
        // DFS from tid to detect cycle
        let _stack: Vec<usize> = Vec::new();
        let mut onstack: BTreeMap<usize, bool> = BTreeMap::new();
        let mut visited: BTreeMap<usize, bool> = BTreeMap::new();
        fn dfs(
            u: usize,
            adj: &BTreeMap<usize, Vec<usize>>,
            visited: &mut BTreeMap<usize, bool>,
            onstack: &mut BTreeMap<usize, bool>,
        ) -> bool {
            visited.insert(u, true);
            onstack.insert(u, true);
            if let Some(neis) = adj.get(&u) {
                for &v in neis {
                    if !visited.get(&v).copied().unwrap_or(false) {
                        if dfs(v, adj, visited, onstack) {
                            return true;
                        }
                    } else if onstack.get(&v).copied().unwrap_or(false) {
                        return true;
                    }
                }
            }
            onstack.insert(u, false);
            false
        }
        if dfs(tid, &adj, &mut visited, &mut onstack) {
            return -(0xDEAD as isize);
        }
        // mark as waiting while we attempt to lock
        process_inner.mutex_waiting.insert(tid, mutex_id);
    }
    drop(process_inner);
    drop(process);
    mutex.lock();
    // after lock returns, record ownership
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    if process_inner.deadlock_detect {
        process_inner.mutex_waiting.remove(&tid);
        if process_inner.mutex_holder.len() <= mutex_id {
            process_inner.mutex_holder.resize(mutex_id + 1, None);
        }
        process_inner.mutex_holder[mutex_id] = Some(tid);
    }
    0
}
/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    if process_inner.deadlock_detect {
        if process_inner.mutex_holder.len() > mutex_id {
            process_inner.mutex_holder[mutex_id] = None;
        }
    }
    drop(process_inner);
    drop(process);
    mutex.unlock();
    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        if process_inner.sem_holders.len() <= id {
            process_inner.sem_holders.resize_with(id + 1, BTreeMap::new);
        }
        process_inner.sem_holders[id].clear();
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        // push holders map slot
        process_inner.sem_holders.push(BTreeMap::new());
        process_inner.semaphore_list.len() - 1
    };
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;
    let process_inner = process.inner_exclusive_access();
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    drop(process_inner);
    sem.up();
    // update holder count after release
    let binding = current_process();
    let mut process_inner = binding.inner_exclusive_access();
    if process_inner.sem_holders.len() > sem_id {
        if let Some(entry) = process_inner.sem_holders[sem_id].get_mut(&tid) {
            if *entry > 0 {
                *entry -= 1;
            }
            if *entry == 0 {
                process_inner.sem_holders[sem_id].remove(&tid);
            }
        }
    }
    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;
    let mut process_inner = process.inner_exclusive_access();
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    // deadlock detection for semaphore
    if process_inner.deadlock_detect {
        // read current count snapshot
        let would_block = {
            let inner = sem.inner.exclusive_access();
            inner.count <= 0
        };
        if would_block {
            // Build Banker's safety check across all semaphores in this process
            // Clone state we need and drop inner borrow to avoid nested borrows
            let sem_list = process_inner.semaphore_list.clone();
            let holders = process_inner.sem_holders.clone();
            let mut waiting = process_inner.sem_waiting.clone();
            waiting.insert(tid, sem_id);
            let m = sem_list.len();
            // collect tids
            let mut tids_vec: Vec<usize> = Vec::new();
            for hm in holders.iter() {
                for (&t, _) in hm.iter() {
                    if !tids_vec.contains(&t) {
                        tids_vec.push(t);
                    }
                }
            }
            for (&t, _) in waiting.iter() {
                if !tids_vec.contains(&t) {
                    tids_vec.push(t);
                }
            }
            // map tid -> idx
            let mut tid2idx: BTreeMap<usize, usize> = BTreeMap::new();
            for (i, &t) in tids_vec.iter().enumerate() {
                tid2idx.insert(t, i);
            }
            let n = tids_vec.len();
            // Available
            let mut work: Vec<usize> = vec![0; m];
            for j in 0..m {
                if let Some(Some(s)) = sem_list.get(j) {
                    let c = s.inner.exclusive_access().count;
                    work[j] = if c > 0 { c as usize } else { 0 };
                }
            }
            // Allocation
            let mut alloc: Vec<Vec<usize>> = vec![vec![0; m]; n];
            for j in 0..m {
                if j < holders.len() {
                    for (&t, &c) in holders[j].iter() {
                        if let Some(&i) = tid2idx.get(&t) {
                            alloc[i][j] = c;
                        }
                    }
                }
            }
            // Need (0/1 vector): waiting thread needs 1 unit of the sem it awaits
            let mut need: Vec<Vec<usize>> = vec![vec![0; m]; n];
            for (&t, &s) in waiting.iter() {
                if let Some(&i) = tid2idx.get(&t) {
                    if s < m {
                        need[i][s] = 1;
                    }
                }
            }
            // Safety check
            let mut finish: Vec<bool> = vec![false; n];
            let mut progress = true;
            while progress {
                progress = false;
                for i in 0..n {
                    if finish[i] {
                        continue;
                    }
                    let mut ok = true;
                    for j in 0..m {
                        if need[i][j] > work[j] {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        // this thread can finish and release its allocation
                        for j in 0..m {
                            work[j] += alloc[i][j];
                        }
                        finish[i] = true;
                        progress = true;
                    }
                }
            }
            let safe = finish.iter().all(|&f| f);
            if !safe {
                return -(0xDEAD as isize);
            }
            process_inner.sem_waiting.insert(tid, sem_id);
        }
    }
    drop(process_inner);
    sem.down();
    // after successful down, record ownership and clear waiting mark
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    if process_inner.deadlock_detect {
        process_inner.sem_waiting.remove(&tid);
        if process_inner.sem_holders.len() <= sem_id {
            process_inner
                .sem_holders
                .resize_with(sem_id + 1, BTreeMap::new);
        }
        *process_inner.sem_holders[sem_id].entry(tid).or_insert(0) += 1;
    }
    0
}
/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// enable deadlock detection syscall
///
/// YOUR JOB: Implement deadlock detection, but might not all in this syscall
pub fn sys_enable_deadlock_detect(_enabled: usize) -> isize {
    trace!("kernel: sys_enable_deadlock_detect");
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    match _enabled {
        0 => {
            inner.deadlock_detect = false;
            0
        }
        1 => {
            inner.deadlock_detect = true;
            0
        }
        _ => -1,
    }
}
