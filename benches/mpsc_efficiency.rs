//! CPU efficiency benchmark for the default (non-sharded) MPSC channel.
//!
//! Measures wall time vs actual CPU time (via getrusage) to show how much
//! CPU is consumed under sustained and bursty workloads with varying sender
//! counts.

use std::{hint::black_box, num::NonZeroUsize, thread, time::Instant};

use gil::mpsc::channel;

fn cpu_time_us() -> u64 {
    unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        let user = usage.ru_utime.tv_sec as u64 * 1_000_000 + usage.ru_utime.tv_usec as u64;
        let sys = usage.ru_stime.tv_sec as u64 * 1_000_000 + usage.ru_stime.tv_usec as u64;
        user + sys
    }
}

fn run_sustained(label: &str, size: NonZeroUsize, count: usize, sender_count: usize) {
    let (tx, mut rx) = channel::<usize>(size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    // Spawn receiver
    let recv_handle = thread::spawn(move || {
        for _ in 0..count {
            black_box(rx.recv());
        }
    });

    // Spawn sender threads
    let per_sender = count / sender_count;
    let mut handles = Vec::with_capacity(sender_count);

    for s in 0..sender_count {
        let mut tx_clone = if s < sender_count - 1 {
            tx.clone()
        } else {
            break;
        };
        let items = per_sender;
        handles.push(thread::spawn(move || {
            for i in 0..items {
                tx_clone.send(black_box(i));
            }
        }));
    }

    // Last sender gets the original tx
    let last_items = count - per_sender * (sender_count - 1);
    let mut tx = tx;
    handles.push(thread::spawn(move || {
        for i in 0..last_items {
            tx.send(black_box(i));
        }
    }));

    for h in handles {
        h.join().unwrap();
    }
    recv_handle.join().unwrap();

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn run_bursty(
    label: &str,
    size: NonZeroUsize,
    bursts: usize,
    burst_size: usize,
    gap_ms: u64,
    sender_count: usize,
) {
    let (tx, mut rx) = channel::<usize>(size);

    let total = bursts * burst_size;
    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    // Spawn receiver
    let recv_handle = thread::spawn(move || {
        for _ in 0..total {
            black_box(rx.recv());
        }
    });

    // Spawn sender threads
    let per_sender = burst_size / sender_count;
    let mut handles = Vec::with_capacity(sender_count);

    for s in 0..sender_count {
        let mut tx_clone = if s < sender_count - 1 {
            tx.clone()
        } else {
            break;
        };
        let items_per_burst = per_sender;
        handles.push(thread::spawn(move || {
            for b in 0..bursts {
                for i in 0..items_per_burst {
                    tx_clone.send(black_box(b * items_per_burst + i));
                }
                if b + 1 < bursts {
                    thread::sleep(std::time::Duration::from_millis(gap_ms));
                }
            }
        }));
    }

    // Last sender gets the original tx
    let last_items_per_burst = burst_size - per_sender * (sender_count - 1);
    let mut tx = tx;
    handles.push(thread::spawn(move || {
        for b in 0..bursts {
            for i in 0..last_items_per_burst {
                tx.send(black_box(b * last_items_per_burst + i));
            }
            if b + 1 < bursts {
                thread::sleep(std::time::Duration::from_millis(gap_ms));
            }
        }
    }));

    for h in handles {
        h.join().unwrap();
    }
    recv_handle.join().unwrap();

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn main() {
    let size = NonZeroUsize::new(4096).unwrap();

    println!("=== MPSC default ===\n");

    println!("Sustained throughput (1M items):");
    for &senders in &[1, 2, 4] {
        run_sustained(
            &format!("size_4096/{senders}tx"),
            size,
            1_000_000,
            senders,
        );
    }

    println!("\nBursty (100 bursts x 1000 items, 1ms gaps):");
    for &senders in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/{senders}tx/1ms_gap"),
            size,
            100,
            1000,
            1,
            senders,
        );
    }

    println!("\nBursty (100 bursts x 1000 items, 5ms gaps):");
    for &senders in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/{senders}tx/5ms_gap"),
            size,
            100,
            1000,
            5,
            senders,
        );
    }

    println!("\nBursty (50 bursts x 1000 items, 10ms gaps):");
    for &senders in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/{senders}tx/10ms_gap"),
            size,
            50,
            1000,
            10,
            senders,
        );
    }

    println!();
}
