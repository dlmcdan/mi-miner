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
    on_found: Arc<FoundCallback>,
    current_thread_count: usize,
}

impl MiningPool {
    pub fn new(
        thread_count: usize,
        stats: Arc<MiningStats>,
        on_found: FoundCallback,
    ) -> Self {
        let shared_work = SharedWork::new();
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

        MiningPool {
            workers,
            shared_work,
            stats,
            on_found,
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
