//! CPU efficiency benchmark for the sharded parking MPSC queue.

use std::{hint::black_box, num::NonZeroUsize, thread, time::Instant};

use gil::mpsc::sharded_parking::channel;

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
    let (tx, mut rx) =
        channel::<usize>(NonZeroUsize::new(sender_count).unwrap(), size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let per_sender = count / sender_count;

    let recv_handle = thread::spawn(move || {
        for _ in 0..count {
            black_box(rx.recv());
        }
    });

    let mut sender_handles = Vec::new();
    for _ in 0..sender_count - 1 {
        let mut tx_clone = tx.clone().unwrap();
        sender_handles.push(thread::spawn(move || {
            for i in 0..per_sender {
                tx_clone.send(black_box(i));
            }
        }));
    }

    let mut tx = tx;
    sender_handles.push(thread::spawn(move || {
        for i in 0..per_sender {
            tx.send(black_box(i));
        }
    }));

    for h in sender_handles {
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
    let (tx, mut rx) =
        channel::<usize>(NonZeroUsize::new(sender_count).unwrap(), size);

    let total = bursts * burst_size;
    let per_sender = burst_size / sender_count;

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let recv_handle = thread::spawn(move || {
        for _ in 0..total {
            black_box(rx.recv());
        }
    });

    for b in 0..bursts {
        let mut sender_handles = Vec::new();
        for _ in 0..sender_count - 1 {
            let mut tx_clone = tx.clone().unwrap();
            sender_handles.push(thread::spawn(move || {
                for i in 0..per_sender {
                    tx_clone.send(black_box(b * burst_size + i));
                }
            }));
        }

        {
            let mut tx_clone = tx.clone().unwrap();
            sender_handles.push(thread::spawn(move || {
                for i in 0..per_sender {
                    tx_clone.send(black_box(b * burst_size + i));
                }
            }));
        }

        for h in sender_handles {
            h.join().unwrap();
        }

        if b + 1 < bursts {
            thread::sleep(std::time::Duration::from_millis(gap_ms));
        }
    }

    drop(tx);
    recv_handle.join().unwrap();

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn run_idle_wait(label: &str, size: NonZeroUsize, wait_ms: u64) {
    let (mut tx, mut rx) =
        channel::<usize>(NonZeroUsize::new(1).unwrap(), size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let h = thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(wait_ms));
        tx.send(black_box(42));
    });

    black_box(rx.recv());

    h.join().unwrap();

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn main() {
    let size = NonZeroUsize::new(4096).unwrap();
    let sender_counts = [1, 2, 4];

    println!("=== MPSC sharded parking ===\n");

    println!("Sustained throughput (1M items):");
    for &sc in &sender_counts {
        run_sustained(&format!("size_4096/{sc}_senders"), size, 1_000_000, sc);
    }

    println!("\nBursty (100 bursts x 1000 items, 1ms gaps):");
    for &sc in &sender_counts {
        run_bursty(&format!("size_4096/1ms_gap/{sc}_senders"), size, 100, 1000, 1, sc);
    }

    println!("\nBursty (100 bursts x 1000 items, 5ms gaps):");
    for &sc in &sender_counts {
        run_bursty(&format!("size_4096/5ms_gap/{sc}_senders"), size, 100, 1000, 5, sc);
    }

    println!("\nBursty (50 bursts x 1000 items, 10ms gaps):");
    for &sc in &sender_counts {
        run_bursty(&format!("size_4096/10ms_gap/{sc}_senders"), size, 50, 1000, 10, sc);
    }

    println!("\nIdle wait (consumer waits for delayed producer):");
    run_idle_wait("size_4096/100ms_delay", size, 100);
    run_idle_wait("size_4096/500ms_delay", size, 500);

    println!();
}
