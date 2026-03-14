use crate::hasher::{hash_range_midstate, hash_range_simple};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run CPU benchmark and print results.
pub fn run_benchmark(duration_secs: u64, threads: usize, full: bool) {
    println!("=== mi-miner CPU Benchmark ===\n");

    // Create a test header
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&0x20000000i32.to_le_bytes());
    header[4..36].fill(0xaa);
    header[36..68].fill(0xbb);
    header[68..72].copy_from_slice(&1700000000u32.to_le_bytes());
    header[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());

    // Use an impossible target so we always exhaust nonces
    let target = [0u8; 32];

    if full {
        run_full_benchmark(&header, &target, duration_secs);
    } else {
        run_simple_benchmark(&header, &target, duration_secs, threads);
    }
}

fn run_simple_benchmark(header: &[u8; 80], target: &[u8; 32], duration_secs: u64, threads: usize) {
    // Single-threaded midstate benchmark
    println!("Single-core midstate benchmark ({duration_secs}s)...");
    let rate = bench_single_core(header, target, duration_secs, true);
    println!("  Hashrate: {}", format_hashrate(rate));

    println!();

    // Multi-threaded benchmark
    if threads > 1 {
        println!("Multi-core benchmark ({threads} threads, {duration_secs}s)...");
        let rate = bench_multi_core(header, target, duration_secs, threads);
        println!("  Hashrate: {}", format_hashrate(rate));
    }

    println!("\nBenchmark complete.");
}

fn run_full_benchmark(header: &[u8; 80], target: &[u8; 32], duration_secs: u64) {
    let bench_duration = duration_secs.min(5); // Use shorter duration for individual tests in full mode

    println!("--- CPU Baseline: Single core, no midstate ---");
    let baseline = bench_single_core(header, target, bench_duration, false);
    println!("  Hashrate: {}\n", format_hashrate(baseline));

    println!("--- CPU Midstate: Single core, with midstate ---");
    let midstate = bench_single_core(header, target, bench_duration, true);
    let improvement = if baseline > 0.0 {
        (midstate / baseline - 1.0) * 100.0
    } else {
        0.0
    };
    println!(
        "  Hashrate: {} (+{:.1}% over baseline)\n",
        format_hashrate(midstate),
        improvement
    );

    println!("--- CPU Scaling: 1 to {} cores ---", num_cpus::get());
    let max_cores = num_cpus::get();
    let mut results = Vec::new();

    for n in [1, 2, 4, 6, 8, 10, 12, 14, 16].iter().copied().filter(|&n| n <= max_cores) {
        let rate = bench_multi_core(header, target, bench_duration, n);
        let per_core = rate / n as f64;
        println!(
            "  {n:2} cores: {} total ({}/core)",
            format_hashrate(rate),
            format_hashrate(per_core)
        );
        results.push((n, rate));
    }

    println!("\n--- Summary ---");
    println!(
        "{:<30} | {:>12} | Notes",
        "Configuration", "Hashrate"
    );
    println!("{:-<30}-+-{:-<12}-+------", "", "");
    println!(
        "{:<30} | {:>12} | baseline",
        "CPU 1-core no midstate",
        format_hashrate(baseline)
    );
    println!(
        "{:<30} | {:>12} | +{:.0}%",
        "CPU 1-core midstate",
        format_hashrate(midstate),
        improvement
    );
    for (n, rate) in &results {
        println!(
            "CPU {n}-core midstate            | {:>12} |",
            format_hashrate(*rate)
        );
    }

    // Save results
    save_benchmark_results(baseline, midstate, &results);
}

fn bench_single_core(header: &[u8; 80], target: &[u8; 32], duration_secs: u64, use_midstate: bool) -> f64 {
    let stop = AtomicBool::new(false);
    let start = Instant::now();
    let deadline = Duration::from_secs(duration_secs);
    let mut total_hashes: u64 = 0;
    let mut nonce: u32 = 0;
    let chunk: u32 = 1 << 20; // 1M nonces per iteration

    while start.elapsed() < deadline {
        let end = nonce.saturating_add(chunk);
        let (_, hashes) = if use_midstate {
            hash_range_midstate(header, nonce, end, target, &stop, chunk, None)
        } else {
            hash_range_simple(header, nonce, end, target, &stop, chunk)
        };
        total_hashes += hashes;
        nonce = end;
        if nonce == 0 {
            break; // wrapped
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    total_hashes as f64 / elapsed
}

fn bench_multi_core(header: &[u8; 80], target: &[u8; 32], duration_secs: u64, threads: usize) -> f64 {
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let header = *header;
            let target = *target;
            let stop = stop.clone();
            let nonce_offset = (u32::MAX / threads as u32) * i as u32;

            std::thread::spawn(move || {
                let mut total_hashes: u64 = 0;
                let mut nonce = nonce_offset;
                let chunk: u32 = 1 << 20;
                let deadline = Duration::from_secs(duration_secs);
                let start = Instant::now();

                while start.elapsed() < deadline && !stop.load(Ordering::Relaxed) {
                    let end = nonce.saturating_add(chunk);
                    let (_, hashes) =
                        hash_range_midstate(&header, nonce, end, &target, &stop, chunk, None);
                    total_hashes += hashes;
                    nonce = end;
                }

                total_hashes
            })
        })
        .collect();

    // Wait for duration then signal stop
    std::thread::sleep(Duration::from_secs(duration_secs));
    stop.store(true, Ordering::Relaxed);

    let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let elapsed = start.elapsed().as_secs_f64();
    total as f64 / elapsed
}

fn save_benchmark_results(baseline: f64, midstate: f64, scaling: &[(usize, f64)]) {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(&home)
        .join(".mi-miner")
        .join("benchmarks");

    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("Warning: Could not create benchmark directory");
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut content = format!(
        "# Benchmark Results - {timestamp}\n\n\
         baseline_no_midstate_hps = {baseline:.0}\n\
         midstate_hps = {midstate:.0}\n\n\
         [scaling]\n"
    );

    for (cores, rate) in scaling {
        content.push_str(&format!("cores_{cores} = {rate:.0}\n"));
    }

    let path = dir.join(format!("bench_{timestamp}.toml"));
    if let Err(e) = std::fs::write(&path, content) {
        eprintln!("Warning: Could not save benchmark results: {e}");
    } else {
        println!("\nResults saved to {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hashrate_hps() {
        assert_eq!(format_hashrate(500.0), "500 H/s");
    }

    #[test]
    fn test_format_hashrate_khps() {
        assert_eq!(format_hashrate(1_500.0), "1.50 KH/s");
    }

    #[test]
    fn test_format_hashrate_mhps() {
        assert_eq!(format_hashrate(5_000_000.0), "5.00 MH/s");
    }

    #[test]
    fn test_format_hashrate_ghps() {
        assert_eq!(format_hashrate(2_500_000_000.0), "2.50 GH/s");
    }

    #[test]
    fn test_format_hashrate_zero() {
        assert_eq!(format_hashrate(0.0), "0 H/s");
    }
}

pub fn format_hashrate(hps: f64) -> String {
    if hps >= 1_000_000_000.0 {
        format!("{:.2} GH/s", hps / 1_000_000_000.0)
    } else if hps >= 1_000_000.0 {
        format!("{:.2} MH/s", hps / 1_000_000.0)
    } else if hps >= 1_000.0 {
        format!("{:.2} KH/s", hps / 1_000.0)
    } else {
        format!("{:.0} H/s", hps)
    }
}
