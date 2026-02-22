use std::{
    hint::{black_box, spin_loop},
    num::NonZeroUsize,
    sync::{Arc, Barrier},
    thread::spawn,
    time::{Duration, SystemTime},
};

use criterion::{
    BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main, measurement::WallTime,
};
use gil::spmc::sharded::channel;

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
    let mut group = c.benchmark_group(name);
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));

    group
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
                        let messages_per_receiver = iter / receiver_count;

                        let mut handles = Vec::with_capacity(receiver_count);
                        for _ in 0..receiver_count - 1 {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages_per_receiver {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages_per_receiver {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_receiver * receiver_count) {
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
                        let messages_per_receiver = iter / receiver_count;

                        let mut handles = Vec::with_capacity(receiver_count);
                        for _ in 0..receiver_count - 1 {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages_per_receiver {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages_per_receiver {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_receiver * receiver_count) {
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
                        let (mut tx, rx) =
                            channel::<Payload1024>(NonZeroUsize::new(receiver_count).unwrap(), size);

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let messages_per_receiver = iter / receiver_count;

                        let mut handles = Vec::with_capacity(receiver_count);
                        for _ in 0..receiver_count - 1 {
                            let mut rx_clone = rx.clone().unwrap();
                            let barrier_clone = Arc::clone(&barrier);
                            handles.push(spawn(move || {
                                barrier_clone.wait();
                                for _ in 0..messages_per_receiver {
                                    black_box(rx_clone.recv());
                                }
                            }));
                        }

                        let mut rx = rx;
                        let barrier_clone = Arc::clone(&barrier);
                        handles.push(spawn(move || {
                            barrier_clone.wait();
                            for _ in 0..messages_per_receiver {
                                black_box(rx.recv());
                            }
                        }));

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_receiver * receiver_count) {
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
    const ELEMENTS: usize = 1_000_000;

    // Throughput for usize (8 bytes)
    {
        let mut group = make_group(c, "spmc/sharded/throughput/payload_8");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * size_of::<usize>()) as u64,
        });

        for size in SIZES {
            for &receiver_count in &RECEIVER_COUNTS {
                group.bench_function(format!("size_{size}/receivers_{receiver_count}/direct"), |b| {
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
                });

                group.bench_function(format!("size_{size}/receivers_{receiver_count}/batched"), |b| {
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
                });
            }
        }
    }

    // Throughput for 1024-byte payload
    {
        const LARGE_ELEMENTS: usize = 100_000;
        let mut group = make_group(c, "spmc/sharded/throughput/payload_1024");
        group.throughput(Throughput::ElementsAndBytes {
            elements: LARGE_ELEMENTS as u64,
            bytes: (LARGE_ELEMENTS * size_of::<Payload1024>()) as u64,
        });

        for size in SIZES {
            for &receiver_count in &RECEIVER_COUNTS {
                group.bench_function(format!("size_{size}/receivers_{receiver_count}/direct"), |b| {
                    b.iter(|| {
                        let (mut tx, rx) =
                            channel::<Payload1024>(NonZeroUsize::new(receiver_count).unwrap(), size);
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
                });

                group.bench_function(format!("size_{size}/receivers_{receiver_count}/batched"), |b| {
                    b.iter(|| {
                        let (mut tx, rx) =
                            channel::<Payload1024>(NonZeroUsize::new(receiver_count).unwrap(), size);
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
                });
            }
        }
    }
}

criterion_group! {benches, benchmark}
criterion_main! {benches}
