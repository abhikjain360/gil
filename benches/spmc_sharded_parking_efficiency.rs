//! CPU efficiency benchmark for the sharded parking SPMC queue.

use std::{hint::black_box, num::NonZeroUsize, thread, time::Instant};

use gil::spmc::sharded_parking::channel;

fn cpu_time_us() -> u64 {
    unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        let user = usage.ru_utime.tv_sec as u64 * 1_000_000 + usage.ru_utime.tv_usec as u64;
        let sys = usage.ru_stime.tv_sec as u64 * 1_000_000 + usage.ru_stime.tv_usec as u64;
        user + sys
    }
}

fn run_sustained(label: &str, size: NonZeroUsize, count: usize, receiver_count: usize) {
    let (mut tx, rx) = channel::<usize>(NonZeroUsize::new(receiver_count).unwrap(), size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let items_per_receiver = count / receiver_count;

    let mut handles = Vec::new();
    for _ in 0..receiver_count - 1 {
        let mut rx = rx.clone().unwrap();
        handles.push(thread::spawn(move || {
            for _ in 0..items_per_receiver {
                black_box(rx.recv());
            }
        }));
    }

    let mut rx = rx;
    handles.push(thread::spawn(move || {
        for _ in 0..items_per_receiver {
            black_box(rx.recv());
        }
    }));

    for i in 0..count {
        tx.send(black_box(i));
    }

    for h in handles {
        h.join().unwrap();
    }

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
    receiver_count: usize,
) {
    let (mut tx, rx) = channel::<usize>(NonZeroUsize::new(receiver_count).unwrap(), size);

    let cpu_start = cpu_time_us();
    let wall_start = Instant::now();

    let items_per_receiver_per_burst = burst_size / receiver_count;
    let total_per_receiver = bursts * items_per_receiver_per_burst;

    let mut handles = Vec::new();
    for _ in 0..receiver_count - 1 {
        let mut rx = rx.clone().unwrap();
        handles.push(thread::spawn(move || {
            for _ in 0..total_per_receiver {
                black_box(rx.recv());
            }
        }));
    }

    let mut rx = rx;
    handles.push(thread::spawn(move || {
        for _ in 0..total_per_receiver {
            black_box(rx.recv());
        }
    }));

    for b in 0..bursts {
        for i in 0..burst_size {
            tx.send(black_box(b * burst_size + i));
        }
        if b + 1 < bursts {
            thread::sleep(std::time::Duration::from_millis(gap_ms));
        }
    }

    for h in handles {
        h.join().unwrap();
    }

    let wall_us = wall_start.elapsed().as_micros() as u64;
    let cpu_us = cpu_time_us() - cpu_start;

    println!(
        "  {label:<30} wall={wall_us:>8}us  cpu={cpu_us:>8}us  cpu/wall={:.2}x",
        cpu_us as f64 / wall_us as f64
    );
}

fn run_idle_wait(label: &str, size: NonZeroUsize, wait_ms: u64) {
    let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(1).unwrap(), size);

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

    println!("=== SPMC sharded parking ===\n");

    println!("Sustained throughput (1M items):");
    for &rc in &[1, 2, 4] {
        run_sustained(&format!("size_4096/rx_{rc}"), size, 1_000_000, rc);
    }

    println!("\nBursty (100 bursts x 1000 items, 1ms gaps):");
    for &rc in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/1ms_gap/rx_{rc}"),
            size,
            100,
            1000,
            1,
            rc,
        );
    }

    println!("\nBursty (100 bursts x 1000 items, 5ms gaps):");
    for &rc in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/5ms_gap/rx_{rc}"),
            size,
            100,
            1000,
            5,
            rc,
        );
    }

    println!("\nBursty (50 bursts x 1000 items, 10ms gaps):");
    for &rc in &[1, 2, 4] {
        run_bursty(
            &format!("size_4096/10ms_gap/rx_{rc}"),
            size,
            50,
            1000,
            10,
            rc,
        );
    }

    println!("\nIdle wait (consumer waits for delayed producer):");
    run_idle_wait("size_4096/100ms_delay", size, 100);
    run_idle_wait("size_4096/500ms_delay", size, 500);

    println!();
}
