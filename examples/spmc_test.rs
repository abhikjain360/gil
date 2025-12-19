use gil::spmc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::SystemTime};

fn main() {
    let (mut tx, rx) = channel(NonZeroUsize::new(4096).unwrap());

    const RECEIVERS: usize = 8;
    const MESSAGES: usize = 1_000_000;

    let start = SystemTime::now();

    let mut handles = Vec::with_capacity(RECEIVERS);
    for _ in 0..RECEIVERS {
        let mut rx = rx.clone();
        handles.push(spawn(move || {
            for _ in 0..MESSAGES {
                let x = rx.recv();
                black_box(x);
            }
        }));
    }

    // Drop the original receiver since we've cloned it for all threads
    drop(rx);

    for i in 0..(RECEIVERS * MESSAGES) {
        tx.send(black_box(i));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let time = start.elapsed().unwrap();
    println!("{time:?}");
}
