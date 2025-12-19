use crate::{atomic::Ordering, hint, mpsc::queue::QueuePtr, thread};

/// The producer end of the MPSC queue.
///
/// This struct is `Clone` and `Send`. It can be shared across threads by cloning it.
#[derive(Clone)]
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

    /// Sends a value into the queue, blocking if necessary.
    ///
    /// This method uses a spin loop to wait for available space in the queue.
    /// For a non-blocking alternative, use [`Sender::try_send`].
    pub fn send(&mut self, value: T) {
        // fetch_add means we are the only ones who can access the cell at this idx
        let tail = self.ptr.tail().fetch_add(1, Ordering::Relaxed);
        let next = tail.wrapping_add(1);

        let cell = self.ptr.at(tail);
        let mut spin_count = 0;
        while cell.epoch().load(Ordering::Acquire) != tail {
            if spin_count < 128 {
                hint::spin_loop();
                spin_count += 1;
            } else {
                spin_count = 0;
                thread::yield_now();
            }
        }

        cell.set(value);
        cell.epoch().store(next, Ordering::Release);
        self.local_tail = next;
    }

    /// Attempts to send a value into the queue without blocking.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the value was successfully sent.
    /// * `Err(value)` if the queue is full, returning the original value.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        use std::cmp::Ordering as Cmp;

        let mut spin_count = 0;

        let cell = loop {
            let cell = self.ptr.at(self.local_tail);
            let epoch = cell.epoch().load(Ordering::Acquire);

            match epoch.cmp(&self.local_tail) {
                // consumer hasn't read the value
                Cmp::Less => return Err(value),

                // consumer has read the value, cell is free
                Cmp::Equal => {
                    let next = self.local_tail.wrapping_add(1);
                    match self.ptr.tail().compare_exchange_weak(
                        self.local_tail,
                        next,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            self.local_tail = next;
                            break cell;
                        }
                        Err(cur_tail) => self.local_tail = cur_tail,
                    }
                }

                // some other producer has written to this cell before us
                Cmp::Greater => self.local_tail = self.ptr.tail().load(Ordering::Relaxed),
            };

            if spin_count < 16 {
                hint::spin_loop();
                spin_count += 1;
            } else {
                spin_count = 0;
                thread::yield_now();
            }
        };

        cell.set(value);
        cell.epoch().store(self.local_tail, Ordering::Release);

        return Ok(());
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
