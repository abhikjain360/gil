use std::{
    hint::{black_box, spin_loop},
    num::NonZeroUsize,
    sync::{Arc, Barrier},
    thread::spawn,
    time::SystemTime,
};

use criterion::{BenchmarkGroup, Criterion, Throughput, criterion_group, measurement::WallTime};
use gil::spmc::sharded::channel;

mod support;

/// A 1024-byte payload for benchmarking large object transfers
#[derive(Clone, Copy)]
#[repr(align(8))]
struct Payload1024 {
    #[expect(dead_code)]
    data: [u8; 1024],
}

impl Payload1024 {
    fn new(val: u8) -> Self {
        Self { data: [val; 1024] }
    }
}

fn make_group<'a>(c: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let group = c.benchmark_group(name);
    support::configure_group(group)
}

fn benchmark(c: &mut Criterion) {
    const SIZES: [NonZeroUsize; 2] = [
        NonZeroUsize::new(512).unwrap(),
        NonZeroUsize::new(4096).unwrap(),
    ];

    const RECEIVER_COUNTS: [usize; 3] = [1, 2, 4];

    // ==================== PUSH LATENCY ====================
    let mut group = make_group(c, "spmc/sharded/push_latency");

    for size in SIZES {
        for &receiver_count in &RECEIVER_COUNTS {
            // u8 payload (1 byte)
            group.bench_function(
                format!("size_{size}/receivers_{receiver_count}/payload_1"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (mut tx, rx) =
                            channel::<u8>(NonZeroUsize::new(receiver_count).unwrap(), size);

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let mut handles = Vec::with_capacity(receiver_count);
                        for receiver_id in 1..receiver_count {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            let messages = support::work_items(iter, receiver_id, receiver_count);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        let messages = support::work_items(iter, 0, receiver_count);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..iter {
                            tx.send(black_box(0u8));
                        }

                        let duration = start.elapsed().unwrap();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        duration
                    });
                },
            );

            // usize payload (8 bytes)
            group.bench_function(
                format!("size_{size}/receivers_{receiver_count}/payload_8"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (mut tx, rx) =
                            channel::<usize>(NonZeroUsize::new(receiver_count).unwrap(), size);

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let mut handles = Vec::with_capacity(receiver_count);
                        for receiver_id in 1..receiver_count {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            let messages = support::work_items(iter, receiver_id, receiver_count);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        let messages = support::work_items(iter, 0, receiver_count);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..iter {
                            tx.send(black_box(0usize));
                        }

                        let duration = start.elapsed().unwrap();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        duration
                    });
                },
            );

            // 1024-byte payload
            group.bench_function(
                format!("size_{size}/receivers_{receiver_count}/payload_1024"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (mut tx, rx) = channel::<Payload1024>(
                            NonZeroUsize::new(receiver_count).unwrap(),
                            size,
                        );

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let mut handles = Vec::with_capacity(receiver_count);
                        for receiver_id in 1..receiver_count {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            let messages = support::work_items(iter, receiver_id, receiver_count);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        let messages = support::work_items(iter, 0, receiver_count);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..iter {
                            tx.send(black_box(Payload1024::new(0)));
                        }

                        let duration = start.elapsed().unwrap();

                        for handle in handles {
                            handle.join().unwrap();
                        }

                        duration
                    });
                },
            );
        }
    }
    drop(group);

    // ==================== THROUGHPUT ====================
    const ELEMENTS: usize = 262_144;

    // Throughput for usize (8 bytes)
    {
        let mut group = make_group(c, "spmc/sharded/throughput/payload_8");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * size_of::<usize>()) as u64,
        });

        for size in SIZES {
            for &receiver_count in &RECEIVER_COUNTS {
                group.bench_function(
                    format!("size_{size}/receivers_{receiver_count}/direct"),
                    |b| {
                        b.iter(|| {
                            let (mut tx, rx) =
                                channel::<usize>(NonZeroUsize::new(receiver_count).unwrap(), size);
                            let messages_per_receiver = ELEMENTS / receiver_count;

                            let mut handles = Vec::with_capacity(receiver_count);
                            for _ in 0..receiver_count - 1 {
                                let mut rx_clone = rx.clone().unwrap();
                                handles.push(spawn(move || {
                                    for _ in 0..messages_per_receiver {
                                        black_box(rx_clone.recv());
                                    }
                                }));
                            }

                            let mut rx = rx;
                            handles.push(spawn(move || {
                                for _ in 0..messages_per_receiver {
                                    black_box(rx.recv());
                                }
                            }));

                            for i in 0..(messages_per_receiver * receiver_count) {
                                tx.send(black_box(i));
                            }

                            for handle in handles {
                                handle.join().unwrap();
                            }
                        });
                    },
                );

                group.bench_function(
                    format!("size_{size}/receivers_{receiver_count}/batched"),
                    |b| {
                        b.iter(|| {
                            let (mut tx, rx) =
                                channel::<usize>(NonZeroUsize::new(receiver_count).unwrap(), size);
                            let messages_per_receiver = ELEMENTS / receiver_count;
                            let total = messages_per_receiver * receiver_count;

                            let mut handles = Vec::with_capacity(receiver_count);
                            for _ in 0..receiver_count - 1 {
                                let mut rx_clone = rx.clone().unwrap();
                                handles.push(spawn(move || {
                                    let mut received = 0;
                                    while received < messages_per_receiver {
                                        let mut guard = rx_clone.read_guard();
                                        let len = guard.len();
                                        if len == 0 {
                                            spin_loop();
                                            continue;
                                        }

                                        black_box(guard.as_slice()[0]);

                                        guard.advance(len);
                                        received += len;
                                    }
                                }));
                            }

                            let mut rx = rx;
                            handles.push(spawn(move || {
                                let mut received = 0;
                                while received < messages_per_receiver {
                                    let mut guard = rx.read_guard();
                                    let len = guard.len();
                                    if len == 0 {
                                        spin_loop();
                                        continue;
                                    }

                                    black_box(guard.as_slice()[0]);

                                    guard.advance(len);
                                    received += len;
                                }
                            }));

                            let mut sent = 0;
                            while sent < total {
                                let buf = tx.write_buffer();
                                let len = buf.len().min(total - sent);
                                if len == 0 {
                                    spin_loop();
                                    continue;
                                }

                                for (i, item) in buf.iter_mut().enumerate().take(len) {
                                    item.write(black_box(sent + i));
                                }

                                unsafe { tx.commit(len) };
                                sent += len;
                            }

                            for handle in handles {
                                handle.join().unwrap();
                            }
                        });
                    },
                );
            }
        }
    }

    // Throughput for 1024-byte payload
    {
        const LARGE_ELEMENTS: usize = 65_536;
        let mut group = make_group(c, "spmc/sharded/throughput/payload_1024");
        group.throughput(Throughput::ElementsAndBytes {
            elements: LARGE_ELEMENTS as u64,
            bytes: (LARGE_ELEMENTS * size_of::<Payload1024>()) as u64,
        });

        for size in SIZES {
            for &receiver_count in &RECEIVER_COUNTS {
                group.bench_function(
                    format!("size_{size}/receivers_{receiver_count}/direct"),
                    |b| {
                        b.iter(|| {
                            let (mut tx, rx) = channel::<Payload1024>(
                                NonZeroUsize::new(receiver_count).unwrap(),
                                size,
                            );
                            let messages_per_receiver = LARGE_ELEMENTS / receiver_count;

                            let mut handles = Vec::with_capacity(receiver_count);
                            for _ in 0..receiver_count - 1 {
                                let mut rx_clone = rx.clone().unwrap();
                                handles.push(spawn(move || {
                                    for _ in 0..messages_per_receiver {
                                        black_box(rx_clone.recv());
                                    }
                                }));
                            }

                            let mut rx = rx;
                            handles.push(spawn(move || {
                                for _ in 0..messages_per_receiver {
                                    black_box(rx.recv());
                                }
                            }));

                            for i in 0..(messages_per_receiver * receiver_count) {
                                tx.send(black_box(Payload1024::new(i as u8)));
                            }

                            for handle in handles {
                                handle.join().unwrap();
                            }
                        });
                    },
                );

                group.bench_function(
                    format!("size_{size}/receivers_{receiver_count}/batched"),
                    |b| {
                        b.iter(|| {
                            let (mut tx, rx) = channel::<Payload1024>(
                                NonZeroUsize::new(receiver_count).unwrap(),
                                size,
                            );
                            let messages_per_receiver = LARGE_ELEMENTS / receiver_count;
                            let total = messages_per_receiver * receiver_count;

                            let mut handles = Vec::with_capacity(receiver_count);
                            for _ in 0..receiver_count - 1 {
                                let mut rx_clone = rx.clone().unwrap();
                                handles.push(spawn(move || {
                                    let mut received = 0;
                                    while received < messages_per_receiver {
                                        let mut guard = rx_clone.read_guard();
                                        let len = guard.len();
                                        if len == 0 {
                                            spin_loop();
                                            continue;
                                        }

                                        black_box(guard.as_slice()[0]);

                                        guard.advance(len);
                                        received += len;
                                    }
                                }));
                            }

                            let mut rx = rx;
                            handles.push(spawn(move || {
                                let mut received = 0;
                                while received < messages_per_receiver {
                                    let mut guard = rx.read_guard();
                                    let len = guard.len();
                                    if len == 0 {
                                        spin_loop();
                                        continue;
                                    }

                                    black_box(guard.as_slice()[0]);

                                    guard.advance(len);
                                    received += len;
                                }
                            }));

                            let mut sent = 0;
                            while sent < total {
                                let buf = tx.write_buffer();
                                let len = buf.len().min(total - sent);
                                if len == 0 {
                                    spin_loop();
                                    continue;
                                }

                                for item in buf.iter_mut().take(len) {
                                    item.write(black_box(Payload1024::new(0)));
                                }

                                unsafe { tx.commit(len) };
                                sent += len;
                            }

                            for handle in handles {
                                handle.join().unwrap();
                            }
                        });
                    },
                );
            }
        }
    }
}

criterion_group! {benches, benchmark}

fn main() {
    support::install_timeout();
    benches();
}
