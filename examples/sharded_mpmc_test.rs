use gil::sharded_mpmc::channel;
use std::{hint::black_box, num::NonZeroUsize, thread::spawn, time::SystemTime};

fn main() {
    const SENDERS: usize = 8;
    const RECEIVERS: usize = 8;
    const CAPACITY_PER_SHARD: usize = 4096;
    const MESSAGES: usize = 1_000_000;

    // Use SENDERS as shard count since we have equal senders and receivers in this test
    let (mut tx, mut rx) = channel(
        NonZeroUsize::new(SENDERS).unwrap(),
        NonZeroUsize::new(CAPACITY_PER_SHARD).unwrap(),
    );

    let start = SystemTime::now();

    // Spawn senders
    let mut sender_handles = Vec::with_capacity(SENDERS);
    for _ in 0..SENDERS - 1 {
        let mut tx = tx.clone().expect("too many senders for max_shards");
        sender_handles.push(spawn(move || {
            for i in 0..MESSAGES {
                tx.send(black_box(i));
            }
        }));
    }
    // Last sender uses the original tx
    sender_handles.push(spawn(move || {
        for i in 0..MESSAGES {
            tx.send(black_box(i));
        }
    }));

    // Spawn receivers
    let mut receiver_handles = Vec::with_capacity(RECEIVERS);
    for _ in 0..RECEIVERS - 1 {
        let mut rx = rx.clone().expect("too many receivers for max_shards");
        receiver_handles.push(spawn(move || {
            // Total messages = SENDERS * MESSAGES
            // Each receiver gets roughly (SENDERS * MESSAGES) / RECEIVERS
            for _ in 0..(SENDERS * MESSAGES / RECEIVERS) {
                let x = rx.recv();
                black_box(x);
            }
        }));
    }
    // Last receiver uses the original rx
    receiver_handles.push(spawn(move || {
        for _ in 0..(SENDERS * MESSAGES / RECEIVERS) {
            let x = rx.recv();
            black_box(x);
        }
    }));

    for handle in sender_handles {
        handle.join().unwrap();
    }
    for handle in receiver_handles {
        handle.join().unwrap();
    }

    let time = start.elapsed().unwrap();
    println!("{time:?}");
}
