//! CPU efficiency benchmark for the spinning SPSC queue.
//!
//! Measures wall time vs actual CPU time (via getrusage) to show how much
//! CPU is consumed during idle periods. The spinning variant is expected to
//! burn CPU while waiting.

use std::{hint::black_box, num::NonZeroUsize, thread, time::Instant};

use gil::spsc::channel;

fn cpu_time_us() -> u64 {
    unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        let user = usage.ru_utime.tv_sec as u64 * 1_000_000 + usage.ru_utime.tv_usec as u64;
        let sys = usage.ru_stime.tv_sec as u64 * 1_000_000 + usage.ru_stime.tv_usec as u64;
        user + sys
    }
}

fn run_sustained(label: &str, size: NonZeroUsize, count: usize) {
    let (mut tx, mut rx) = channel::<usize>(size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let h = thread::spawn(move || {
        for _ in 0..count {
            black_box(rx.recv());
        }
    });

    for i in 0..count {
        tx.send(black_box(i));
    }

    h.join().unwrap();

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn run_bursty(label: &str, size: NonZeroUsize, bursts: usize, burst_size: usize, gap_ms: u64) {
    let (mut tx, mut rx) = channel::<usize>(size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let h = thread::spawn(move || {
        for _ in 0..bursts {
            for _ in 0..burst_size {
                black_box(rx.recv());
            }
        }
    });

    for b in 0..bursts {
        for i in 0..burst_size {
            tx.send(black_box(b * burst_size + i));
        }
        if b + 1 < bursts {
            thread::sleep(std::time::Duration::from_millis(gap_ms));
        }
    }

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

    println!("=== SPSC spinning ===\n");

    println!("Sustained throughput (1M items):");
    run_sustained("size_4096", size, 1_000_000);

    println!("\nBursty (100 bursts x 1000 items, 1ms gaps):");
    run_bursty("size_4096/1ms_gap", size, 100, 1000, 1);

    println!("\nBursty (100 bursts x 1000 items, 5ms gaps):");
    run_bursty("size_4096/5ms_gap", size, 100, 1000, 5);

    println!("\nBursty (50 bursts x 1000 items, 10ms gaps):");
    run_bursty("size_4096/10ms_gap", size, 50, 1000, 10);

    println!();
}
