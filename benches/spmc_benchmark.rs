use std::{
    hint::black_box,
    num::NonZeroUsize,
    sync::{Arc, Barrier},
    thread::spawn,
    time::{Duration, SystemTime},
};

use criterion::{
    BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main, measurement::WallTime,
};
use gil::spmc::channel;

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

    const RECEIVER_COUNTS: [usize; 2] = [2, 8];

    // ==================== LATENCY ====================
    let mut group = make_group(c, "spmc_latency");

    for size in SIZES {
        for &receiver_count in &RECEIVER_COUNTS {
            // u8 payload (1 byte)
            group.bench_function(
                format!("size_{size}/receivers_{receiver_count}/payload_1"),
                |b| {
                    b.iter_custom(|iter| {
                        let iter = iter as usize;
                        let (mut tx, rx) = channel::<u8>(size);

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let messages_per_receiver = iter / receiver_count;

                        let handles: Vec<_> = (0..receiver_count)
                            .map(|_| {
                                let mut rx = rx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for _ in 0..messages_per_receiver {
                                        black_box(rx.recv());
                                    }
                                })
                            })
                            .collect();

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
                        let (mut tx, rx) = channel::<usize>(size);

                        let barrier = Arc::new(Barrier::new(receiver_count + 1));
                        let messages_per_receiver = iter / receiver_count;

                        let handles: Vec<_> = (0..receiver_count)
                            .map(|_| {
                                let mut rx = rx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for _ in 0..messages_per_receiver {
                                        black_box(rx.recv());
                                    }
                                })
                            })
                            .collect();

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
        }
    }
    drop(group);

    // ==================== THROUGHPUT ====================
    const ELEMENTS: usize = 1_000_000;

    // Throughput for usize (8 bytes)
    {
        let mut group = make_group(c, "spmc_throughput/payload_8");
        group.throughput(Throughput::ElementsAndBytes {
            elements: ELEMENTS as u64,
            bytes: (ELEMENTS * std::mem::size_of::<usize>()) as u64,
        });

        for size in SIZES {
            for &receiver_count in &RECEIVER_COUNTS {
                group.bench_function(format!("size_{size}/receivers_{receiver_count}"), |b| {
                    b.iter(|| {
                        let (mut tx, rx) = channel::<usize>(size);
                        let messages_per_receiver = ELEMENTS / receiver_count;

                        let handles: Vec<_> = (0..receiver_count)
                            .map(|_| {
                                let mut rx = rx.clone();
                                spawn(move || {
                                    for _ in 0..messages_per_receiver {
                                        black_box(rx.recv());
                                    }
                                })
                            })
                            .collect();

                        for i in 0..(messages_per_receiver * receiver_count) {
                            tx.send(black_box(i));
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
}

criterion_group! {benches, benchmark}

criterion_main! {benches}
