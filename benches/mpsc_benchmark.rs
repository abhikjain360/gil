use std::{
    hint::black_box,
    num::NonZeroUsize,
    sync::{Arc, Barrier},
    thread::spawn,
    time::{Duration, Instant, SystemTime},
};

use criterion::{
    BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main, measurement::WallTime,
};
use gil::mpsc::channel;

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

    const SENDER_COUNTS: [usize; 3] = [1, 2, 8];

    // ==================== PUSH LATENCY ====================
    let mut group = make_group(c, "push_latency");

    for size in SIZES {
        for &sender_count in &SENDER_COUNTS {
            // u8 payload (1 byte)
            group.bench_function(
                format!("size_{size}/senders_{sender_count}/payload_1"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (tx, mut rx) = channel::<u8>(size);

                        let barrier = Arc::new(Barrier::new(sender_count + 1));
                        let messages_per_sender = iter / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|_| {
                                let mut tx = tx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for _ in 0..messages_per_sender {
                                        tx.send(black_box(0u8));
                                    }
                                })
                            })
                            .collect();

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
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
                format!("size_{size}/senders_{sender_count}/payload_8"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (tx, mut rx) = channel::<usize>(size);

                        let barrier = Arc::new(Barrier::new(sender_count + 1));
                        let messages_per_sender = iter / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|_| {
                                let mut tx = tx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for _ in 0..messages_per_sender {
                                        tx.send(black_box(0usize));
                                    }
                                })
                            })
                            .collect();

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
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
                format!("size_{size}/senders_{sender_count}/payload_1024"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (tx, mut rx) = channel::<Payload1024>(size);

                        let barrier = Arc::new(Barrier::new(sender_count + 1));
                        let messages_per_sender = iter / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|_| {
                                let mut tx = tx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for _ in 0..messages_per_sender {
                                        tx.send(black_box(Payload1024::new(0)));
                                    }
                                })
                            })
                            .collect();

                        barrier.wait();
                        let start = SystemTime::now();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
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

    // Throughput for u8 (1 byte)
    {
        let mut group = make_group(c, "throughput/payload_1");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * size_of::<u8>()) as u64,
        });

        for size in SIZES {
            for &sender_count in &SENDER_COUNTS {
                group.bench_function(format!("size_{size}/senders_{sender_count}"), |b| {
                    b.iter(|| {
                        let (tx, mut rx) = channel::<u8>(size);
                        let messages_per_sender = ELEMENTS / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|sender_id| {
                                let mut tx = tx.clone();
                                spawn(move || {
                                    for i in 0..messages_per_sender {
                                        tx.send(black_box(((sender_id + i) & 0xFF) as u8));
                                    }
                                })
                            })
                            .collect();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
                        }

                        for handle in handles {
                            handle.join().unwrap();
                        }
                    });
                });
            }
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
            for &sender_count in &SENDER_COUNTS {
                group.bench_function(format!("size_{size}/senders_{sender_count}"), |b| {
                    b.iter(|| {
                        let (tx, mut rx) = channel::<usize>(size);
                        let messages_per_sender = ELEMENTS / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|sender_id| {
                                let mut tx = tx.clone();
                                spawn(move || {
                                    for i in 0..messages_per_sender {
                                        tx.send(black_box(sender_id + i));
                                    }
                                })
                            })
                            .collect();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
                        }

                        for handle in handles {
                            handle.join().unwrap();
                        }
                    });
                });
            }
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
            for &sender_count in &SENDER_COUNTS {
                group.bench_function(format!("size_{size}/senders_{sender_count}"), |b| {
                    b.iter(|| {
                        let (tx, mut rx) = channel::<Payload1024>(size);
                        let messages_per_sender = LARGE_ELEMENTS / sender_count;

                        let handles: Vec<_> = (0..sender_count)
                            .map(|sender_id| {
                                let mut tx = tx.clone();
                                spawn(move || {
                                    for i in 0..messages_per_sender {
                                        tx.send(black_box(Payload1024::new(
                                            ((sender_id + i) & 0xFF) as u8,
                                        )));
                                    }
                                })
                            })
                            .collect();

                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.recv());
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

fn large_queue_benchmark(c: &mut Criterion) {
    let mut group = make_group(c, "large_queue");

    const ITERS: usize = 5_000_000;
    const SENDER_COUNTS: [usize; 3] = [1, 2, 8];

    group.throughput(Throughput::Elements(ITERS as u64));

    for &sender_count in &SENDER_COUNTS {
        group.bench_function(
            format!("uncontended_throughput/senders_{sender_count}"),
            |b| {
                b.iter_custom(|measurement_iters| {
                    let mut total_duration = Duration::ZERO;

                    for _ in 0..measurement_iters {
                        // "Queue is so long that there is no contention between threads."
                        // Capacity = 2 * ITERS
                        let capacity = NonZeroUsize::new((2 * ITERS).next_power_of_two()).unwrap();
                        let (tx, mut rx) = channel::<u8>(capacity);

                        // Pre-fill
                        let mut tx_prefill = tx.clone();
                        for i in 0..ITERS {
                            tx_prefill.try_send(i as u8).unwrap();
                        }

                        let barrier = Arc::new(Barrier::new(sender_count + 2));
                        let messages_per_sender = ITERS / sender_count;

                        let push_handles: Vec<_> = (0..sender_count)
                            .map(|_| {
                                let mut tx = tx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    let start = Instant::now();
                                    for i in 0..messages_per_sender {
                                        tx.try_send(i as u8).unwrap();
                                    }
                                    let end = Instant::now();
                                    (start, end)
                                })
                            })
                            .collect();

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
                        for _ in 0..(messages_per_sender * sender_count) {
                            black_box(rx.try_recv().unwrap());
                        }
                        let stop_popping = Instant::now();

                        let mut min_start = start_popping;
                        let mut max_end = stop_popping;

                        for handle in push_handles {
                            let (start, end) = handle.join().unwrap();
                            min_start = min_start.min(start);
                            max_end = max_end.max(end);
                        }

                        trigger_thread.join().unwrap();

                        // "The total time is the max end time minus the min start time"
                        let total = max_end.duration_since(min_start);

                        total_duration += total;
                    }
                    total_duration
                })
            },
        );
    }

    group.finish();
}

criterion_group! {benches, benchmark, large_queue_benchmark}

criterion_main! {benches}
