use std::{
    hint::black_box,
    num::NonZeroUsize,
    sync::{Arc, Barrier},
    thread::spawn,
};

use criterion::{BenchmarkGroup, Criterion, Throughput, criterion_group, measurement::WallTime};
use gil::mpmc::channel;

mod support;

fn make_group<'a>(c: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let group = c.benchmark_group(name);
    support::configure_group(group)
}

fn benchmark(c: &mut Criterion) {
    const SIZES: [usize; 2] = [512, 4096];
    const THREAD_PAIRS: [usize; 2] = [1, 4]; // (1 sender, 1 receiver) and (4 senders, 4 receivers)
    const ELEMENTS: usize = 1_000_000;

    let mut group = make_group(c, "mpmc/throughput");
    group.throughput(Throughput::Elements(ELEMENTS as u64));

    for size in SIZES {
        for &pairs in &THREAD_PAIRS {
            let sender_count = pairs;
            let receiver_count = pairs;

            group.bench_function(
                format!("size_{size}/senders_{sender_count}/receivers_{receiver_count}"),
                |b| {
                    b.iter(|| {
                        let (tx, rx) = channel::<usize>(NonZeroUsize::new(size).unwrap());
                        let messages_per_sender = ELEMENTS / sender_count;
                        let messages_per_receiver = ELEMENTS / receiver_count;

                        let barrier = Arc::new(Barrier::new(sender_count + receiver_count));

                        let send_handles: Vec<_> = (0..sender_count)
                            .map(|sender_id| {
                                let mut tx = tx.clone();
                                let barrier = Arc::clone(&barrier);
                                spawn(move || {
                                    barrier.wait();
                                    for i in 0..messages_per_sender {
                                        tx.send(black_box(sender_id + i));
                                    }
                                })
                            })
                            .collect();

                        let recv_handles: Vec<_> = (0..receiver_count)
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

                        for handle in send_handles {
                            handle.join().unwrap();
                        }
                        for handle in recv_handles {
                            handle.join().unwrap();
                        }
                    });
                },
            );
        }
    }
    drop(group);
}

criterion_group!(benches, benchmark);

fn main() {
    support::install_timeout();
    benches();
}
