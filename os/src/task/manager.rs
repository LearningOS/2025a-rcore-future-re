//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// A simple FIFO scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        // 如果队列为空直接返回
        if self.ready_queue.is_empty() {
            return None;
        }
        // 找到拥有最小 stride 的索引
        let mut min_idx = 0usize;
        let mut min_stride = {
            let t = &self.ready_queue[0];
            t.inner_exclusive_access().stride
        };
        for (i, task) in self.ready_queue.iter().enumerate().skip(1) {
            let s = task.inner_exclusive_access().stride;
            if s < min_stride {
                min_stride = s;
                min_idx = i;
            }
        }
        // 从队列中移除选中的任务
        let task = self.ready_queue.remove(min_idx).unwrap();
        // 更新选中任务的 stride = stride + pass（防溢出）
        {
            let mut inner = task.inner_exclusive_access();
            inner.stride = inner.stride.saturating_add(inner.pass);
        }
        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
