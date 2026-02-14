# gil

**Get In Line** - A collection of high-performance, lock-free concurrent queues with sync and async support.

> ⚠️ WIP: things **WILL** change a lot without warnings even in minor updates until v1, use at your own risk.

## Usage

### Single-Producer Single-Consumer (SPSC)

The most optimized queue for 1-to-1 thread communication.

```rust
use std::thread;
use core::num::NonZeroUsize;
use gil::spsc::channel;

const COUNT: usize = 100_000;

let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(COUNT).unwrap());

let handle = thread::spawn(move || {
    for i in 0..COUNT {
        tx.send(i);
    }
});

for i in 0..COUNT {
    let value = rx.recv();
    assert_eq!(value, i);
}

handle.join().unwrap();
```

### Multi-Producer Single-Consumer (MPSC)

Useful when multiple threads need to send data to a single worker thread.

```rust
use std::thread;
use core::num::NonZeroUsize;
use gil::mpsc::channel;

let (tx, mut rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
let mut handles = vec![];

for i in 0..10 {
    let mut tx_clone = tx.clone();
    handles.push(thread::spawn(move || {
        tx_clone.send(i);
    }));
}

for _ in 0..10 {
    let _ = rx.recv();
}

for handle in handles {
    handle.join().unwrap();
}
```

### Multi-Producer Multi-Consumer (MPMC)

The most flexible queue, allowing multiple senders and multiple receivers.

```rust
use std::thread;
use core::num::NonZeroUsize;
use gil::mpmc::channel;

let (tx, rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
let mut handles = vec![];

// Spawn multiple producers
for i in 0..5 {
    let mut tx_clone = tx.clone();
    handles.push(thread::spawn(move || {
        tx_clone.send(i);
    }));
}

// Spawn multiple consumers
for _ in 0..5 {
    let mut rx_clone = rx.clone();
    handles.push(thread::spawn(move || {
        let _ = rx_clone.recv();
    }));
}

for handle in handles {
    handle.join().unwrap();
}
```

### Sharded MPMC/MPSC

For high-throughput scenarios where multiple threads access the queue concurrently, sharded versions can significantly reduce contention. These use multiple SPSC queues internally and distribute load across them.

**Note:** The sharded channels use a "bounded" number of shards. This means the number of concurrent senders (and receivers for MPMC) is limited to the number of shards. Cloning a sender/receiver will fail (return `None`) if all shards are occupied.

```rust
use std::thread;
use core::num::NonZeroUsize;
use gil::mpmc::sharded::channel;

let max_shards = NonZeroUsize::new(8).unwrap();
let capacity_per_shard = NonZeroUsize::new(128).unwrap();
let (tx, mut rx) = channel::<usize>(max_shards, capacity_per_shard);

// Clone sender to use different shards
// Note: This returns Option<Sender>, returning None if all shards are busy.
if let Some(mut tx2) = tx.try_clone() {
    thread::spawn(move || {
        tx2.send(42);
    });
}

let value = rx.recv();
assert_eq!(value, 42);
```

### Async Example

To use async features, enable the `async` feature in your `Cargo.toml`.

> **Note:** Async support (`send_async`/`recv_async`) is currently only available for SPSC queues.

```toml
[dependencies]
gil = { version = "0.5", features = ["async"] }
```

```rust,ignore
use gil::spsc::channel;
use core::num::NonZeroUsize;

const COUNT: usize = 100_000;

let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(COUNT).unwrap());

let handle = tokio::spawn(async move {
    for i in 0..COUNT {
        // Await until send completes
        tx.send_async(i).await;
    }
});

for i in 0..COUNT {
    // Await until recv completes
    let value = rx.recv_async().await;
    assert_eq!(value, i);
}

handle.await.unwrap();
```

### Non-blocking Operations

```rust
use gil::spsc::channel;
use core::num::NonZeroUsize;

let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(10).unwrap());

// Try to send without blocking
match tx.try_send(42) {
    Ok(()) => println!("Sent successfully"),
    Err(val) => println!("Queue full, value {} returned", val),
}

// Try to receive without blocking
match rx.try_recv() {
    Some(val) => println!("Received: {}", val),
    None => println!("Queue empty"),
}
```

### Batch Operations (Zero-copy)

For maximum performance, you can directly access the internal buffer. This allows you to write or read multiple items at once, bypassing the per-item synchronization overhead.

```rust
use gil::spsc::channel;
use core::ptr;
use core::num::NonZeroUsize;

let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());

// Zero-copy write
let data = [1usize, 2, 3, 4, 5];
let slice = tx.write_buffer();
let count = data.len().min(slice.len());

unsafe {
    ptr::copy_nonoverlapping(
        data.as_ptr(),
        slice.as_mut_ptr().cast(),
        count
    );
    // Commit the written items to make them visible to the consumer
    tx.commit(count);
}

// Zero-copy read via ReadGuard
let mut guard = rx.read_guard();
for &value in guard.as_slice() {
    println!("Value: {}", value);
}
// Mark all items as consumed; head is published on drop
guard.advance(guard.len());
```

## Performance

The queue achieves high throughput through several optimizations:

- **Cache-line alignment**: Head and tail pointers are on separate cache lines to prevent false sharing
- **Local caching**: Each side caches the other side's position to reduce atomic operations
- **Batch operations**: Amortize atomic operation costs across multiple items
- **Zero-copy API**: Direct buffer access eliminates memory copies

### Large Objects

For large objects, consider using `Box<T>` to avoid the cost of copying the entire object into the queue. This way, only the pointer (8 bytes) is copied:

```rust
use gil::spsc::channel;
use core::num::NonZeroUsize;

struct LargeStruct {
    data: [u8; 1024],
}

let (mut tx, mut rx) = channel::<Box<LargeStruct>>(NonZeroUsize::new(100).unwrap());

// Only the Box pointer is copied, not the 1024 bytes
tx.send(Box::new(LargeStruct { data: [0; 1024] }));
let value = rx.recv();
```

## Safety

The code has been verified using:
- [loom](https://github.com/tokio-rs/loom) - Concurrency testing
- [miri](https://github.com/rust-lang/miri) - Undefined behavior detection

## License

MIT License - see [LICENSE](https://github.com/abhikjain360/spsc/blob/main/LICENSE) file for details.

## Acknowledgements

- **SPSC** was inspired by the `ProducerConsumerQueue` in the [Facebook Folly](https://github.com/facebook/folly/blob/main/folly/ProducerConsumerQueue.h) library.
- **MPMC/MPSC** are based on the bounded queue algorithm developed by [Dmitry Vyukov](http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue).

For more details on third-party licenses, see the [LICENSE-THIRD-PARTY](LICENSE-THIRD-PARTY) file.
