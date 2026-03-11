use gil::mpmc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::SystemTime};

fn main() {
    let (tx, rx) = channel(NonZeroUsize::new(4096).unwrap());

    const SENDERS: usize = 4;
    const RECEIVERS: usize = 4;
    const MESSAGES: usize = 5_000_000;

    let start = SystemTime::now();

    for _ in 0..SENDERS {
        let mut tx = tx.clone();
        spawn(move || {
            for i in 0..MESSAGES {
                tx.send(black_box(i));
            }
        });
    }

    // Drop the original sender since we've cloned it for all threads
    drop(tx);

    let mut handles = Vec::with_capacity(RECEIVERS);
    for _ in 0..RECEIVERS {
        let mut rx = rx.clone();
        handles.push(spawn(move || {
            for _ in 0..(SENDERS * MESSAGES / RECEIVERS) {
                let x = rx.recv();
                black_box(x);
            }
        }));
    }

    // Drop the original receiver since we've cloned it for all threads
    drop(rx);

    for handle in handles {
        handle.join().unwrap();
    }

    let time = start.elapsed().unwrap();
    println!("{time:?}");
}
