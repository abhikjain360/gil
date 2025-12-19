use crate::{atomic::Ordering, hint, spmc::queue::QueuePtr, thread};

pub struct Sender<T> {
    ptr: QueuePtr<T>,
    local_tail: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>) -> Self {
        Self {
            ptr: queue_ptr,
            local_tail: 0,
        }
    }

    pub fn send(&mut self, value: T) {
        let cell = self.ptr.at(self.local_tail);
        let mut spin_count = 0;
        while cell.epoch().load(Ordering::Acquire) != self.local_tail {
            if spin_count < 128 {
                hint::spin_loop();
                spin_count += 1;
            } else {
                spin_count = 0;
                thread::yield_now();
            }
        }

        let next = self.local_tail.wrapping_add(1);
        cell.set(value);
        cell.epoch().store(next, Ordering::Release);
        self.local_tail = next;
    }

    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let cell = self.ptr.at(self.local_tail);
        if cell.epoch().load(Ordering::Acquire) != self.local_tail {
            return Err(value);
        }

        let next = self.local_tail.wrapping_add(1);
        cell.set(value);
        cell.epoch().store(next, Ordering::Release);
        self.local_tail = next;

        Ok(())
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
