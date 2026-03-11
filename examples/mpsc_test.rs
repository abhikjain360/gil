use gil::mpsc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::SystemTime};

fn main() {
    let (tx, mut rx) = channel(NonZeroUsize::new(4096).unwrap());

    const SENDERS: usize = 4;
    const MESSAGES: usize = 5_000_000;

    let start = SystemTime::now();

    for _ in 0..SENDERS {
        let mut tx = tx.clone();
        spawn(move || {
            for i in 0..MESSAGES {
                while tx.try_send(black_box(i)).is_err() {}
            }
        });
    }

    // Drop the original sender since we've cloned it for all threads
    drop(tx);

    for _ in 0..(SENDERS * MESSAGES) {
        loop {
            let Some(x) = rx.try_recv() else {
                continue;
            };
            black_box(x);
            break;
        }
    }

    let time = start.elapsed().unwrap();
    println!(
        "time={time:?}\nmil/s={throughput:.2}",
        throughput = MESSAGES as f64 / 1_000_000.0 / time.as_secs_f64()
    );
}
