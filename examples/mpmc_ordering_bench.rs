use gil::mpmc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::Instant};

fn run_once() -> std::time::Duration {
    let (tx, rx) = channel(NonZeroUsize::new(4096).unwrap());

    const SENDERS: usize = 6;
    const RECEIVERS: usize = 6;
    const MESSAGES_PER_SENDER: usize = 1_000_000;

    let start = Instant::now();

    for _ in 0..SENDERS {
        let mut tx = tx.clone();
        spawn(move || {
            for i in 0..MESSAGES_PER_SENDER {
                tx.send(black_box(i));
            }
        });
    }
    drop(tx);

    let mut handles = Vec::with_capacity(RECEIVERS);
    for _ in 0..RECEIVERS {
        let mut rx = rx.clone();
        handles.push(spawn(move || {
            for _ in 0..(SENDERS * MESSAGES_PER_SENDER / RECEIVERS) {
                let x = rx.recv();
                black_box(x);
            }
        }));
    }
    drop(rx);

    for handle in handles {
        handle.join().unwrap();
    }

    start.elapsed()
}

fn main() {
    // warmup
    run_once();

    let mut times = Vec::with_capacity(10);
    for i in 0..10 {
        let elapsed = run_once();
        println!("iter {i}: {elapsed:?}");
        times.push(elapsed);
    }

    times.sort();
    let median = times[5];
    let min = times[0];
    let max = times[9];
    println!("\nmin: {min:?}, median: {median:?}, max: {max:?}");
}
