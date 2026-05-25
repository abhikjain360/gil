use static_assertions::{assert_impl_all, assert_not_impl_any};
use std::{cell::RefCell, rc::Rc};

type NonSend = Rc<RefCell<usize>>;

assert_impl_all!(gil::mpmc::Sender<usize>: Send);
assert_impl_all!(gil::mpmc::Receiver<usize>: Send);
assert_impl_all!(gil::mpmc::sharded::Sender<usize>: Send);
assert_impl_all!(gil::mpmc::sharded::Receiver<usize>: Send);

assert_impl_all!(gil::mpsc::Sender<usize>: Send);
assert_impl_all!(gil::mpsc::Receiver<usize>: Send);
assert_impl_all!(gil::mpsc::sharded::Sender<usize>: Send);
assert_impl_all!(gil::mpsc::sharded::Receiver<usize>: Send);

assert_impl_all!(gil::spmc::Sender<usize>: Send);
assert_impl_all!(gil::spmc::Receiver<usize>: Send);
assert_impl_all!(gil::spmc::sharded::Sender<usize>: Send);
assert_impl_all!(gil::spmc::sharded::Receiver<usize>: Send);

assert_impl_all!(gil::spsc::Sender<usize>: Send);
assert_impl_all!(gil::spsc::Receiver<usize>: Send);

#[cfg(feature = "std")]
assert_impl_all!(gil::mpsc::sharded_parking::Sender<usize>: Send);
#[cfg(feature = "std")]
assert_impl_all!(gil::mpsc::sharded_parking::Receiver<usize>: Send);
#[cfg(feature = "std")]
assert_impl_all!(gil::spmc::sharded_parking::Sender<usize>: Send);
#[cfg(feature = "std")]
assert_impl_all!(gil::spmc::sharded_parking::Receiver<usize>: Send);
#[cfg(feature = "std")]
assert_impl_all!(gil::spsc::parking::Sender<usize>: Send);
#[cfg(feature = "std")]
assert_impl_all!(gil::spsc::parking::Receiver<usize>: Send);

assert_not_impl_any!(gil::mpmc::Sender<NonSend>: Send);
assert_not_impl_any!(gil::mpmc::Receiver<NonSend>: Send);
assert_not_impl_any!(gil::mpmc::sharded::Sender<NonSend>: Send);
assert_not_impl_any!(gil::mpmc::sharded::Receiver<NonSend>: Send);

assert_not_impl_any!(gil::mpsc::Sender<NonSend>: Send);
assert_not_impl_any!(gil::mpsc::Receiver<NonSend>: Send);
assert_not_impl_any!(gil::mpsc::sharded::Sender<NonSend>: Send);
assert_not_impl_any!(gil::mpsc::sharded::Receiver<NonSend>: Send);

assert_not_impl_any!(gil::spmc::Sender<NonSend>: Send);
assert_not_impl_any!(gil::spmc::Receiver<NonSend>: Send);
assert_not_impl_any!(gil::spmc::sharded::Sender<NonSend>: Send);
assert_not_impl_any!(gil::spmc::sharded::Receiver<NonSend>: Send);

assert_not_impl_any!(gil::spsc::Sender<NonSend>: Send);
assert_not_impl_any!(gil::spsc::Receiver<NonSend>: Send);

#[cfg(feature = "std")]
assert_not_impl_any!(gil::mpsc::sharded_parking::Sender<NonSend>: Send);
#[cfg(feature = "std")]
assert_not_impl_any!(gil::mpsc::sharded_parking::Receiver<NonSend>: Send);
#[cfg(feature = "std")]
assert_not_impl_any!(gil::spmc::sharded_parking::Sender<NonSend>: Send);
#[cfg(feature = "std")]
assert_not_impl_any!(gil::spmc::sharded_parking::Receiver<NonSend>: Send);
#[cfg(feature = "std")]
assert_not_impl_any!(gil::spsc::parking::Sender<NonSend>: Send);
#[cfg(feature = "std")]
assert_not_impl_any!(gil::spsc::parking::Receiver<NonSend>: Send);
