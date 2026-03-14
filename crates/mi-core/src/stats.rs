use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug)]
pub struct MiningStats {
    // Hash counts
    pub cpu_hashes: AtomicU64,
    pub gpu_hashes: AtomicU64,

    // Share tracking
    pub shares_submitted: AtomicU64,
    pub shares_accepted: AtomicU64,
    pub shares_rejected: AtomicU64,
    pub blocks_found: AtomicU64,

    // Current state
    pub cpu_threads: AtomicU64,
    pub gpu_intensity_pct: AtomicU64, // stored as 0-100
    pub is_user_active: AtomicBool,
    pub idle_secs: AtomicU64,

    // Hashrate snapshots (hashes per second, stored as integer)
    pub cpu_hashrate: AtomicU64,
    pub gpu_hashrate: AtomicU64,

    // Control
    pub should_stop: AtomicBool,
    pub paused: AtomicBool,

    // Start time (not atomic, set once)
    pub start_time: Instant,
}

impl MiningStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cpu_hashes: AtomicU64::new(0),
            gpu_hashes: AtomicU64::new(0),
            shares_submitted: AtomicU64::new(0),
            shares_accepted: AtomicU64::new(0),
            shares_rejected: AtomicU64::new(0),
            blocks_found: AtomicU64::new(0),
            cpu_threads: AtomicU64::new(0),
            gpu_intensity_pct: AtomicU64::new(100),
            is_user_active: AtomicBool::new(false),
            idle_secs: AtomicU64::new(0),
            cpu_hashrate: AtomicU64::new(0),
            gpu_hashrate: AtomicU64::new(0),
            should_stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            start_time: Instant::now(),
        })
    }

    pub fn add_cpu_hashes(&self, count: u64) {
        self.cpu_hashes.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_gpu_hashes(&self, count: u64) {
        self.gpu_hashes.fetch_add(count, Ordering::Relaxed);
    }

    pub fn total_hashes(&self) -> u64 {
        self.cpu_hashes.load(Ordering::Relaxed) + self.gpu_hashes.load(Ordering::Relaxed)
    }

    pub fn combined_hashrate(&self) -> u64 {
        self.cpu_hashrate.load(Ordering::Relaxed) + self.gpu_hashrate.load(Ordering::Relaxed)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            cpu_hashrate: self.cpu_hashrate.load(Ordering::Relaxed),
            gpu_hashrate: self.gpu_hashrate.load(Ordering::Relaxed),
            combined_hashrate: self.combined_hashrate(),
            total_hashes: self.total_hashes(),
            cpu_hashes: self.cpu_hashes.load(Ordering::Relaxed),
            gpu_hashes: self.gpu_hashes.load(Ordering::Relaxed),
            shares_submitted: self.shares_submitted.load(Ordering::Relaxed),
            shares_accepted: self.shares_accepted.load(Ordering::Relaxed),
            shares_rejected: self.shares_rejected.load(Ordering::Relaxed),
            blocks_found: self.blocks_found.load(Ordering::Relaxed),
            cpu_threads: self.cpu_threads.load(Ordering::Relaxed),
            gpu_intensity_pct: self.gpu_intensity_pct.load(Ordering::Relaxed),
            is_user_active: self.is_user_active.load(Ordering::Relaxed),
            idle_secs: self.idle_secs.load(Ordering::Relaxed),
            uptime_secs: self.uptime_secs(),
            paused: self.paused.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsSnapshot {
    pub cpu_hashrate: u64,
    pub gpu_hashrate: u64,
    pub combined_hashrate: u64,
    pub total_hashes: u64,
    pub cpu_hashes: u64,
    pub gpu_hashes: u64,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub blocks_found: u64,
    pub cpu_threads: u64,
    pub gpu_intensity_pct: u64,
    pub is_user_active: bool,
    pub idle_secs: u64,
    pub uptime_secs: u64,
    pub paused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_stats_defaults() {
        let stats = MiningStats::new();
        assert_eq!(stats.cpu_hashes.load(Ordering::Relaxed), 0);
        assert_eq!(stats.gpu_hashes.load(Ordering::Relaxed), 0);
        assert_eq!(stats.shares_submitted.load(Ordering::Relaxed), 0);
        assert_eq!(stats.blocks_found.load(Ordering::Relaxed), 0);
        assert!(!stats.should_stop.load(Ordering::Relaxed));
        assert!(!stats.paused.load(Ordering::Relaxed));
        assert_eq!(stats.gpu_intensity_pct.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_add_hashes() {
        let stats = MiningStats::new();
        stats.add_cpu_hashes(100);
        stats.add_cpu_hashes(200);
        stats.add_gpu_hashes(500);
        assert_eq!(stats.cpu_hashes.load(Ordering::Relaxed), 300);
        assert_eq!(stats.gpu_hashes.load(Ordering::Relaxed), 500);
        assert_eq!(stats.total_hashes(), 800);
    }

    #[test]
    fn test_combined_hashrate() {
        let stats = MiningStats::new();
        stats.cpu_hashrate.store(1_000_000, Ordering::Relaxed);
        stats.gpu_hashrate.store(5_000_000, Ordering::Relaxed);
        assert_eq!(stats.combined_hashrate(), 6_000_000);
    }

    #[test]
    fn test_snapshot_captures_state() {
        let stats = MiningStats::new();
        stats.add_cpu_hashes(42);
        stats.add_gpu_hashes(99);
        stats.shares_submitted.store(3, Ordering::Relaxed);
        stats.shares_accepted.store(2, Ordering::Relaxed);
        stats.shares_rejected.store(1, Ordering::Relaxed);
        stats.blocks_found.store(0, Ordering::Relaxed);
        stats.cpu_threads.store(8, Ordering::Relaxed);
        stats.paused.store(true, Ordering::Relaxed);
        stats.is_user_active.store(true, Ordering::Relaxed);

        let snap = stats.snapshot();
        assert_eq!(snap.cpu_hashes, 42);
        assert_eq!(snap.gpu_hashes, 99);
        assert_eq!(snap.total_hashes, 141);
        assert_eq!(snap.shares_submitted, 3);
        assert_eq!(snap.shares_accepted, 2);
        assert_eq!(snap.shares_rejected, 1);
        assert_eq!(snap.blocks_found, 0);
        assert_eq!(snap.cpu_threads, 8);
        assert!(snap.paused);
        assert!(snap.is_user_active);
    }

    #[test]
    fn test_snapshot_serializes_to_json() {
        let stats = MiningStats::new();
        stats.add_cpu_hashes(1000);
        let snap = stats.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"cpu_hashes\":1000"));
        assert!(json.contains("\"paused\":false"));
    }

    #[test]
    fn test_uptime_increases() {
        let stats = MiningStats::new();
        // uptime should be 0 or very small
        assert!(stats.uptime_secs() < 2);
    }

    #[test]
    fn test_concurrent_hash_counting() {
        let stats = MiningStats::new();
        let mut handles = vec![];

        for _ in 0..4 {
            let s = stats.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    s.add_cpu_hashes(1);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(stats.cpu_hashes.load(Ordering::Relaxed), 4000);
    }
}
