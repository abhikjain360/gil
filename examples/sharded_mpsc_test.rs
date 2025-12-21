use gil::sharded_mpsc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::SystemTime};

fn main() {
    const SENDERS: usize = 8;
    const CAPACITY_PER_SHARD: usize = 4096;
    const MESSAGES: usize = 10_000_000;

    let (mut tx, mut rx) = channel(
        NonZeroUsize::new(SENDERS).unwrap(),
        NonZeroUsize::new(CAPACITY_PER_SHARD).unwrap(),
    );

    let start = SystemTime::now();

    for _ in 0..SENDERS - 1 {
        let mut tx = tx.clone().expect("too many senders for max_shards");
        spawn(move || {
            for i in 0..MESSAGES {
                tx.send(black_box(i));
            }
        });
    }
    spawn(move || {
        for i in 0..MESSAGES {
            tx.send(black_box(i));
        }
    });

    for _ in 0..(SENDERS * MESSAGES) {
        let x = rx.recv();
        black_box(x);
    }

    let time = start.elapsed().unwrap();
    println!("{time:?}");
}
