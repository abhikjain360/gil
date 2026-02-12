use std::{
    hint::{black_box, spin_loop},
    num::NonZeroUsize,
    ptr,
    sync::{Arc, Barrier},
    thread::spawn,
    time::{Duration, Instant, SystemTime},
};

use criterion::{
    BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main, measurement::WallTime,
};
use gil::read_guard::BatchReader;
use gil::spsc::channel;

/// A 1024-byte payload for benchmarking large object transfers
#[derive(Clone, Copy)]
#[repr(align(8))]
struct Payload1024 {
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
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(1));

    group
}

fn benchmark(c: &mut Criterion) {
    const SIZES: [NonZeroUsize; 3] = [
        NonZeroUsize::new(512).unwrap(),
        NonZeroUsize::new(4096).unwrap(),
        NonZeroUsize::new(65_536).unwrap(),
    ];

    // ==================== ROUNDTRIP LATENCY ====================
    let mut group = make_group(c, "roundtrip_latency");

    for size in SIZES {
        // u8 payload (1 byte)
        group.bench_function(format!("size_{size}/payload_1"), |b| {
            b.iter_custom(move |iter| {
                let (mut tx1, mut rx1) = channel::<u8>(size);
                let (mut tx2, mut rx2) = channel::<u8>(size);

                let iter = iter as usize;

                spawn(move || {
                    for i in 0..iter {
                        let x = rx1.recv();
                        black_box(x);
                        tx2.send(black_box(i as u8));
                    }
                });

                let start = SystemTime::now();

                for i in 0..iter {
                    tx1.send(black_box(i as u8));
                    let x = rx2.recv();
                    black_box(x);
                }

                start.elapsed().unwrap()
            });
        });

        // usize payload (8 bytes)
        group.bench_function(format!("size_{size}/payload_8"), |b| {
            b.iter_custom(move |iter| {
                let (mut tx1, mut rx1) = channel::<usize>(size);
                let (mut tx2, mut rx2) = channel::<usize>(size);

                let iter = iter as usize;

                spawn(move || {
                    for i in 0..iter {
                        let x = rx1.recv();
                        black_box(x);
                        tx2.send(black_box(i));
                    }
                });

                let start = SystemTime::now();

                for i in 0..iter {
                    tx1.send(black_box(i));
                    let x = rx2.recv();
                    black_box(x);
                }

                start.elapsed().unwrap()
            });
        });

        // 1024-byte payload
        group.bench_function(format!("size_{size}/payload_1024"), |b| {
            b.iter_custom(move |iter| {
                let (mut tx1, mut rx1) = channel::<Payload1024>(size);
                let (mut tx2, mut rx2) = channel::<Payload1024>(size);

                let iter = iter as usize;

                spawn(move || {
                    for i in 0..iter {
                        let x = rx1.recv();
                        black_box(x);
                        tx2.send(black_box(Payload1024::new(i as u8)));
                    }
                });

                let start = SystemTime::now();

                for i in 0..iter {
                    tx1.send(black_box(Payload1024::new(i as u8)));
                    let x = rx2.recv();
                    black_box(x);
                }

                start.elapsed().unwrap()
            });
        });
    }
    drop(group);

    // ==================== PUSH LATENCY ====================
    let mut group = make_group(c, "push_latency");

    for size in SIZES {
        // u8 payload (1 byte)
        group.bench_function(format!("size_{size}/payload_1"), |b| {
            b.iter_custom(|iter| {
                let (mut tx, mut rx) = channel::<u8>(size);
                spawn(move || {
                    for _ in 0..iter {
                        black_box(rx.recv());
                    }
                });
                let start = SystemTime::now();

                for _ in 0..iter {
                    tx.send(black_box(0u8));
                }

                start.elapsed().unwrap()
            });
        });

        // usize payload (8 bytes)
        group.bench_function(format!("size_{size}/payload_8"), |b| {
            b.iter_custom(|iter| {
                let (mut tx, mut rx) = channel::<usize>(size);
                spawn(move || {
                    for _ in 0..iter {
                        black_box(rx.recv());
                    }
                });
                let start = SystemTime::now();

                for _ in 0..iter {
                    tx.send(black_box(0usize));
                }

                start.elapsed().unwrap()
            });
        });

        // 1024-byte payload
        group.bench_function(format!("size_{size}/payload_1024"), |b| {
            b.iter_custom(|iter| {
                let (mut tx, mut rx) = channel::<Payload1024>(size);
                spawn(move || {
                    for _ in 0..iter {
                        black_box(rx.recv());
                    }
                });
                let start = SystemTime::now();

                for _ in 0..iter {
                    tx.send(black_box(Payload1024::new(0)));
                }

                start.elapsed().unwrap()
            });
        });
    }
    drop(group);

    // ==================== THROUGHPUT ====================
    const ELEMENTS: usize = 1_000_000;

    // Throughput for u8 (1 byte)
    {
        let mut group = make_group(c, "throughput/payload_1");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * size_of::<u8>()) as u64,
        });

        for size in SIZES {
            group.bench_function(format!("size_{size}/direct"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<u8>(size);

                    spawn(move || {
                        for i in 0..ELEMENTS {
                            let x = black_box(rx.recv());
                            assert_eq!((i & 0xFF) as u8, x);
                        }
                    });

                    for i in 0..ELEMENTS {
                        tx.send(black_box((i & 0xFF) as u8));
                    }
                });
            });

            group.bench_function(format!("size_{size}/batched"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<u8>(size);

                    spawn(move || {
                        let mut received = 0;
                        while received < ELEMENTS {
                            let buf = rx.read_buffer();
                            let len = buf.len();
                            if len == 0 {
                                spin_loop();
                                continue;
                            }

                            black_box(buf[0]);

                            unsafe { rx.advance(len) };
                            received += len;
                        }
                    });

                    let mut sent = 0;
                    let src_data = vec![1u8; size.get()];

                    while sent < ELEMENTS {
                        let buf = tx.write_buffer();
                        let len = buf.len().min(ELEMENTS - sent).min(src_data.len());
                        if len == 0 {
                            spin_loop();
                            continue;
                        }

                        unsafe {
                            ptr::copy_nonoverlapping(
                                src_data.as_ptr().cast(),
                                buf.as_mut_ptr(),
                                len,
                            );
                        }

                        unsafe { tx.commit(len) };
                        sent += len;
                    }
                });
            });
        }
        drop(group);
    }

    // Throughput for usize (8 bytes)
    {
        let mut group = make_group(c, "throughput/payload_8");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * size_of::<usize>()) as u64,
        });

        for size in SIZES {
            group.bench_function(format!("size_{size}/direct"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<usize>(size);

                    spawn(move || {
                        for i in 0..ELEMENTS {
                            let x = black_box(rx.recv());
                            assert_eq!(i, x);
                        }
                    });

                    for i in 0..ELEMENTS {
                        tx.send(black_box(i));
                    }
                });
            });

            group.bench_function(format!("size_{size}/batched"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<usize>(size);

                    spawn(move || {
                        let mut received = 0;
                        while received < ELEMENTS {
                            let buf = rx.read_buffer();
                            let len = buf.len();
                            if len == 0 {
                                spin_loop();
                                continue;
                            }

                            black_box(buf[0]);

                            unsafe { rx.advance(len) };
                            received += len;
                        }
                    });

                    let mut sent = 0;
                    let src_data = vec![1usize; size.get()];

                    while sent < ELEMENTS {
                        let buf = tx.write_buffer();
                        let len = buf.len().min(ELEMENTS - sent).min(src_data.len());
                        if len == 0 {
                            spin_loop();
                            continue;
                        }

                        unsafe {
                            ptr::copy_nonoverlapping(
                                src_data.as_ptr().cast(),
                                buf.as_mut_ptr(),
                                len,
                            );
                        }

                        unsafe { tx.commit(len) };
                        sent += len;
                    }
                });
            });
        }
        drop(group);
    }

    // Throughput for 1024-byte payload
    {
        let mut group = make_group(c, "throughput/payload_1024");
        // Use fewer elements for large payloads to keep benchmark time reasonable
        const LARGE_ELEMENTS: usize = 100_000;
        group.throughput(Throughput::ElementsAndBytes {
            elements: LARGE_ELEMENTS as u64,
            bytes: (LARGE_ELEMENTS * size_of::<Payload1024>()) as u64,
        });

        for size in SIZES {
            group.bench_function(format!("size_{size}/direct"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<Payload1024>(size);

                    spawn(move || {
                        for i in 0..LARGE_ELEMENTS {
                            let x = black_box(rx.recv());
                            assert_eq!((i & 0xFF) as u8, x.data[0]);
                        }
                    });

                    for i in 0..LARGE_ELEMENTS {
                        tx.send(black_box(Payload1024::new((i & 0xFF) as u8)));
                    }
                });
            });

            group.bench_function(format!("size_{size}/batched"), |b| {
                b.iter(|| {
                    let (mut tx, mut rx) = channel::<Payload1024>(size);

                    spawn(move || {
                        let mut received = 0;
                        while received < LARGE_ELEMENTS {
                            let buf = rx.read_buffer();
                            let len = buf.len();
                            if len == 0 {
                                spin_loop();
                                continue;
                            }

                            black_box(buf[0]);

                            unsafe { rx.advance(len) };
                            received += len;
                        }
                    });

                    let mut sent = 0;
                    let src_data: Vec<Payload1024> = (0..size.get())
                        .map(|i| Payload1024::new((i & 0xFF) as u8))
                        .collect();

                    while sent < LARGE_ELEMENTS {
                        let buf = tx.write_buffer();
                        let len = buf.len().min(LARGE_ELEMENTS - sent).min(src_data.len());
                        if len == 0 {
                            spin_loop();
                            continue;
                        }

                        unsafe {
                            ptr::copy_nonoverlapping(
                                src_data.as_ptr().cast(),
                                buf.as_mut_ptr(),
                                len,
                            );
                        }

                        unsafe { tx.commit(len) };
                        sent += len;
                    }
                });
            });
        }
    }
}

fn large_queue_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_queue");
    group.measurement_time(Duration::from_secs(5));
    group.warm_up_time(Duration::from_secs(1));

    const ITERS: usize = 5_000_000;

    group.throughput(Throughput::Elements(ITERS as u64));

    group.bench_function("uncontended_throughput", |b| {
        b.iter_custom(|measurement_iters| {
            let mut total_duration = Duration::ZERO;

            for _ in 0..measurement_iters {
                // "Queue is so long that there is no contention between threads."
                // Capacity = 2 * ITERS
                let capacity = NonZeroUsize::new((2 * ITERS).next_power_of_two()).unwrap();
                let (mut p, mut c) = channel::<u8>(capacity);

                // Pre-fill
                for i in 0..ITERS {
                    p.try_send(i as u8).unwrap();
                }

                let barrier = Arc::new(Barrier::new(3));

                let push_thread = {
                    let barrier = Arc::clone(&barrier);
                    spawn(move || {
                        barrier.wait();
                        let start_pushing = Instant::now();
                        for i in 0..ITERS {
                            p.try_send(i as u8).unwrap();
                        }
                        let stop_pushing = Instant::now();
                        (start_pushing, stop_pushing)
                    })
                };

                let trigger_thread = {
                    let barrier = Arc::clone(&barrier);
                    spawn(move || {
                        // "Try to force other threads to go to sleep on barrier."
                        std::thread::yield_now();
                        barrier.wait();
                    })
                };

                barrier.wait();
                let start_popping = Instant::now();
                for _ in 0..ITERS {
                    black_box(c.try_recv().unwrap());
                }
                let stop_popping = Instant::now();

                let (start_pushing, stop_pushing) = push_thread.join().unwrap();
                trigger_thread.join().unwrap();

                // "The total time is the max end time minus the min start time"
                let total = stop_pushing
                    .max(stop_popping)
                    .duration_since(start_pushing.min(start_popping));

                total_duration += total;
            }
            total_duration
        })
    });

    group.finish();
}

#[cfg(feature = "async")]
fn benchmark_async(c: &mut Criterion) {
    use criterion::async_executor::FuturesExecutor;

    const SIZES: [NonZeroUsize; 3] = [
        NonZeroUsize::new(512).unwrap(),
        NonZeroUsize::new(4096).unwrap(),
        NonZeroUsize::new(65_536).unwrap(),
    ];

    let mut group = make_group(c, "async_roundtrip_latency");

    for size in SIZES {
        group.bench_function(format!("size_{size}"), |b| {
            b.to_async(FuturesExecutor)
                .iter_custom(move |iter| async move {
                    let (mut tx1, mut rx1) = channel::<usize>(size);
                    let (mut tx2, mut rx2) = channel::<usize>(size);

                    let iter = iter as usize;

                    let handle = spawn(move || {
                        futures::executor::block_on(async move {
                            for i in 0..iter {
                                let x = rx1.recv_async().await;
                                black_box(x);
                                tx2.send_async(black_box(i)).await;
                            }
                        })
                    });

                    let start = SystemTime::now();

                    for i in 0..iter {
                        tx1.send_async(black_box(i)).await;
                        let x = rx2.recv_async().await;
                        black_box(x);
                    }

                    handle.join().unwrap();

                    start.elapsed().unwrap()
                });
        });
    }
    drop(group);

    let mut group = make_group(c, "async_push_latency");

    for size in SIZES {
        group.bench_function(format!("size_{size}"), |b| {
            b.to_async(FuturesExecutor)
                .iter_custom(move |iter| async move {
                    let (mut tx, mut rx) = channel::<usize>(size);

                    let handle = spawn(move || {
                        futures::executor::block_on(async move {
                            for _ in 0..iter {
                                black_box(rx.recv_async().await);
                            }
                        })
                    });

                    let start = SystemTime::now();

                    for _ in 0..iter {
                        tx.send_async(black_box(0)).await;
                    }

                    handle.join().unwrap();

                    start.elapsed().unwrap()
                });
        });
    }
    drop(group);

    let mut group = make_group(c, "async_throughput");
    const ELEMENTS: usize = 1_000_000;

    group.throughput(Throughput::ElementsAndBytes {
        elements: ELEMENTS as u64,
        bytes: (ELEMENTS * size_of::<usize>()) as u64,
    });

    for size in SIZES {
        group.bench_function(format!("size_{size}"), |b| {
            b.to_async(FuturesExecutor).iter(move || async move {
                let (mut tx, mut rx) = channel::<usize>(size);

                let handle = spawn(move || {
                    futures::executor::block_on(async move {
                        for i in 0..ELEMENTS {
                            let x = black_box(rx.recv_async().await);
                            assert_eq!(i, x);
                        }
                    })
                });

                for i in 0..ELEMENTS {
                    tx.send_async(black_box(i)).await;
                }

                handle.join().unwrap();
            });
        });
    }
}

#[cfg(feature = "async")]
criterion_group! {benches, benchmark, large_queue_benchmark, benchmark_async}

#[cfg(not(feature = "async"))]
criterion_group! {benches, benchmark, large_queue_benchmark}

criterion_main! {benches}
