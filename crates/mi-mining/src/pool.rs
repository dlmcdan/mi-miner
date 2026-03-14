use crate::worker::{mining_worker, FoundCallback, Work};
use crossbeam_channel::{bounded, Sender};
use mi_core::MiningStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Thread pool for CPU mining workers.
pub struct MiningPool {
    workers: Vec<JoinHandle<()>>,
    work_tx: Sender<Work>,
    stats: Arc<MiningStats>,
    on_found: Arc<FoundCallback>,
    current_thread_count: usize,
}

impl MiningPool {
    /// Create a new mining pool with the given number of threads.
    pub fn new(
        thread_count: usize,
        stats: Arc<MiningStats>,
        on_found: FoundCallback,
    ) -> Self {
        let (work_tx, work_rx) = bounded::<Work>(thread_count * 2);
        let on_found = Arc::new(on_found);

        let mut workers = Vec::with_capacity(thread_count);

        for id in 0..thread_count {
            let rx = work_rx.clone();
            let stats = stats.clone();
            let on_found = on_found.clone();

            let handle = std::thread::Builder::new()
                .name(format!("miner-{id}"))
                .spawn(move || {
                    mining_worker(id, rx, stats, on_found);
                })
                .expect("Failed to spawn mining thread");

            workers.push(handle);
        }

        stats.cpu_threads.store(thread_count as u64, Ordering::Relaxed);

        MiningPool {
            workers,
            work_tx,
            stats,
            on_found,
            current_thread_count: thread_count,
        }
    }

    /// Submit work to the pool. Distributes nonce ranges across workers.
    pub fn submit_work(&self, work: Work) {
        let total_nonces = work
            .nonce_end
            .saturating_sub(work.nonce_start)
            .max(1) as u64;
        let chunk_size = (total_nonces / self.current_thread_count as u64).max(1);

        for i in 0..self.current_thread_count {
            let start = work.nonce_start + (i as u64 * chunk_size) as u32;
            let end = if i == self.current_thread_count - 1 {
                work.nonce_end
            } else {
                (start as u64 + chunk_size) as u32
            };

            if start >= end {
                break;
            }

            let chunk_work = Work {
                header: work.header,
                target: work.target,
                nonce_start: start,
                nonce_end: end,
                job_id: work.job_id.clone(),
                extranonce: work.extranonce,
            };

            if self.work_tx.send(chunk_work).is_err() {
                tracing::warn!("Failed to send work to pool (channel closed)");
                break;
            }
        }
    }

    /// Dynamically adjust thread count.
    pub fn set_thread_count(&mut self, count: usize) {
        if count == self.current_thread_count {
            return;
        }

        tracing::info!(
            from = self.current_thread_count,
            to = count,
            "Adjusting CPU mining threads"
        );

        // For simplicity, we recreate the pool.
        // Drop old sender to close channels and let workers exit.
        let (new_tx, new_rx) = bounded::<Work>(count * 2);

        // Signal old workers to stop by dropping the old sender
        // (they'll get a RecvError when channel drains)
        let old_tx = std::mem::replace(&mut self.work_tx, new_tx);
        drop(old_tx);

        // Wait for old workers
        let old_workers = std::mem::take(&mut self.workers);
        for handle in old_workers {
            let _ = handle.join();
        }

        // Spawn new workers
        let mut new_workers = Vec::with_capacity(count);
        for id in 0..count {
            let rx = new_rx.clone();
            let stats = self.stats.clone();
            let on_found = self.on_found.clone();

            let handle = std::thread::Builder::new()
                .name(format!("miner-{id}"))
                .spawn(move || {
                    mining_worker(id, rx, stats, on_found);
                })
                .expect("Failed to spawn mining thread");

            new_workers.push(handle);
        }

        self.workers = new_workers;
        self.current_thread_count = count;
        self.stats.cpu_threads.store(count as u64, Ordering::Relaxed);
    }

    /// Get current thread count.
    pub fn thread_count(&self) -> usize {
        self.current_thread_count
    }

    /// Shut down the pool gracefully.
    pub fn shutdown(self) {
        drop(self.work_tx);
        for handle in self.workers {
            let _ = handle.join();
        }
    }
}
