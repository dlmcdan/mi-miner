use crate::worker::{mining_worker, FoundCallback, SharedWork, Work};
use mi_core::MiningStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Thread pool for CPU mining workers.
pub struct MiningPool {
    workers: Vec<JoinHandle<()>>,
    shared_work: Arc<SharedWork>,
    stats: Arc<MiningStats>,
    current_thread_count: usize,
}

impl MiningPool {
    pub fn new(
        thread_count: usize,
        stats: Arc<MiningStats>,
        on_found: FoundCallback,
    ) -> Self {
        Self::with_shared_work(thread_count, stats, on_found, SharedWork::new())
    }

    /// Create a mining pool that uses an externally-provided SharedWork.
    /// This allows the caller to control work distribution (e.g., separate CPU/GPU work).
    pub fn with_shared_work(
        thread_count: usize,
        stats: Arc<MiningStats>,
        on_found: FoundCallback,
        shared_work: Arc<SharedWork>,
    ) -> Self {
        let shared_work = shared_work;
        let on_found = Arc::new(on_found);

        let mut workers = Vec::with_capacity(thread_count);

        for id in 0..thread_count {
            let sw = shared_work.clone();
            let stats = stats.clone();
            let on_found = on_found.clone();

            let handle = std::thread::Builder::new()
                .name(format!("miner-{id}"))
                .spawn(move || {
                    mining_worker(id, thread_count, sw, stats, on_found);
                })
                .expect("Failed to spawn mining thread");

            workers.push(handle);
        }

        stats.cpu_threads.store(thread_count as u64, Ordering::Relaxed);
        stats.active_cpu_threads.store(thread_count as u64, Ordering::Relaxed);

        MiningPool {
            workers,
            shared_work,
            stats,
            current_thread_count: thread_count,
        }
    }

    /// Submit new work — all workers will pick it up on their next check (~1M hashes).
    pub fn submit_work(&self, work: Work) {
        self.shared_work.update(work);
    }

    /// Get a reference to the shared work for external updates.
    pub fn shared_work(&self) -> Arc<SharedWork> {
        self.shared_work.clone()
    }

    /// Get current thread count.
    pub fn thread_count(&self) -> usize {
        self.current_thread_count
    }

    /// Shut down the pool gracefully.
    pub fn shutdown(self) {
        self.stats.should_stop.store(true, Ordering::Relaxed);
        for handle in self.workers {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_callback() -> FoundCallback {
        Box::new(|_work, _nonce, _hash| {})
    }

    #[test]
    fn thread_count_returns_configured_count() {
        let stats = MiningStats::new();
        let pool = MiningPool::new(2, stats, noop_callback());

        assert_eq!(pool.thread_count(), 2);

        pool.shutdown();
    }

    #[test]
    fn shared_work_returns_arc() {
        let stats = MiningStats::new();
        let pool = MiningPool::new(1, stats, noop_callback());

        let sw = pool.shared_work();
        // Verify it's a valid SharedWork — generation starts at 0
        assert_eq!(sw.generation.load(Ordering::Acquire), 0);
        assert!(sw.work.lock().unwrap().is_none());

        pool.shutdown();
    }

    #[test]
    fn pool_creation_sets_stats_and_shuts_down_cleanly() {
        let stats = MiningStats::new();
        let stats_clone = stats.clone();
        let pool = MiningPool::new(2, stats, noop_callback());

        // Verify stats were set correctly by the constructor
        assert_eq!(stats_clone.cpu_threads.load(Ordering::Relaxed), 2);
        assert_eq!(stats_clone.active_cpu_threads.load(Ordering::Relaxed), 2);

        // Verify pool reports correct thread count
        assert_eq!(pool.thread_count(), 2);

        // Shutdown should complete without hanging
        pool.shutdown();

        // After shutdown, should_stop should be true
        assert!(stats_clone.should_stop.load(Ordering::Relaxed));
    }

    #[test]
    fn active_cpu_threads_initialized_correctly_in_stats() {
        let stats = MiningStats::new();
        let stats_clone = stats.clone();

        // Before pool creation, active_cpu_threads defaults to 0
        assert_eq!(stats_clone.active_cpu_threads.load(Ordering::Relaxed), 0);

        let pool = MiningPool::new(1, stats, noop_callback());

        // After pool creation, active_cpu_threads should match thread_count
        assert_eq!(stats_clone.active_cpu_threads.load(Ordering::Relaxed), 1);
        assert_eq!(stats_clone.cpu_threads.load(Ordering::Relaxed), 1);

        pool.shutdown();
    }

    #[test]
    fn with_shared_work_uses_external_shared_work() {
        let stats = MiningStats::new();
        let external_sw = SharedWork::new();

        // Pre-populate the external shared work
        let work = Work {
            header: [0xABu8; 80],
            target: [0xFFu8; 32],
            job_id: "external-work".to_string(),
            extranonce: 42,
            ntime: "aabb".to_string(),
        };
        external_sw.update(work);
        assert_eq!(external_sw.generation.load(Ordering::Acquire), 1);

        let pool = MiningPool::with_shared_work(1, stats, noop_callback(), external_sw.clone());

        // Pool's shared_work should be the same Arc
        let pool_sw = pool.shared_work();
        assert_eq!(pool_sw.generation.load(Ordering::Acquire), 1);
        let stored = pool_sw.work.lock().unwrap();
        assert_eq!(stored.as_ref().unwrap().job_id, "external-work");
        assert_eq!(stored.as_ref().unwrap().extranonce, 42);
        drop(stored);

        // Updating external_sw should be visible through pool_sw
        let work2 = Work {
            header: [0u8; 80],
            target: [0u8; 32],
            job_id: "updated".to_string(),
            extranonce: 99,
            ntime: "ccdd".to_string(),
        };
        external_sw.update(work2);
        assert_eq!(pool_sw.generation.load(Ordering::Acquire), 2);
        let stored2 = pool_sw.work.lock().unwrap();
        assert_eq!(stored2.as_ref().unwrap().job_id, "updated");
        drop(stored2);

        pool.shutdown();
    }

    #[test]
    fn submit_work_updates_shared_work() {
        let stats = MiningStats::new();
        let pool = MiningPool::new(1, stats, noop_callback());

        let sw = pool.shared_work();
        assert_eq!(sw.generation.load(Ordering::Acquire), 0);

        let work = Work {
            header: [0u8; 80],
            target: [0xFFu8; 32],
            job_id: "submit-test".to_string(),
            extranonce: 7,
            ntime: "aabbccdd".to_string(),
        };
        pool.submit_work(work);

        assert_eq!(sw.generation.load(Ordering::Acquire), 1);
        let stored = sw.work.lock().unwrap();
        assert_eq!(stored.as_ref().unwrap().job_id, "submit-test");

        drop(stored);
        pool.shutdown();
    }
}
