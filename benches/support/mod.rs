#![allow(dead_code)]

use std::{process, thread, time::Duration};

use criterion::{BenchmarkGroup, measurement::WallTime};

const BENCH_TIMEOUT_SECS: u64 = 120;
const MEASUREMENT_TIME_MS: u64 = 250;
const WARM_UP_TIME_MS: u64 = 100;
const SAMPLE_SIZE: usize = 10;

pub fn install_timeout() {
    thread::Builder::new()
        .name("bench-timeout".to_owned())
        .spawn(|| {
            thread::sleep(Duration::from_secs(BENCH_TIMEOUT_SECS));
            eprintln!("benchmark timed out after {BENCH_TIMEOUT_SECS}s");
            process::exit(124);
        })
        .expect("failed to spawn benchmark timeout thread");
}

pub fn configure_group<'a>(
    mut group: BenchmarkGroup<'a, WallTime>,
) -> BenchmarkGroup<'a, WallTime> {
    group.measurement_time(Duration::from_millis(MEASUREMENT_TIME_MS));
    group.sample_size(SAMPLE_SIZE);
    group.warm_up_time(Duration::from_millis(WARM_UP_TIME_MS));
    group
}

pub fn work_items(total: usize, worker_index: usize, workers: usize) -> usize {
    debug_assert!(worker_index < workers);
    let base = total / workers;
    let remainder = total % workers;
    base + usize::from(worker_index < remainder)
}
