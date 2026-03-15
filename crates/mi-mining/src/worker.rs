use crate::hasher::{hash_range_midstate, HashResult};
use mi_core::MiningStats;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Work data shared between stratum and all workers.
#[derive(Clone, Debug)]
pub struct Work {
    pub header: [u8; 80],
    pub target: [u8; 32],
    pub job_id: String,
    pub extranonce: u64,
    pub ntime: String,
}

/// Shared work holder. Stratum updates this; workers poll it.
pub struct SharedWork {
    pub work: Mutex<Option<Work>>,
    pub generation: AtomicU64,
}

impl SharedWork {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            work: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }

    /// Called by stratum when new work arrives.
    pub fn update(&self, work: Work) {
        *self.work.lock().unwrap() = Some(work);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Called by workers to get current work if generation changed.
    pub fn get_if_new(&self, last_gen: u64) -> Option<(u64, Work)> {
        let current_gen = self.generation.load(Ordering::Acquire);
        if current_gen != last_gen {
            let work = self.work.lock().unwrap().clone();
            work.map(|w| (current_gen, w))
        } else {
            None
        }
    }
}

/// Callback when a valid nonce is found.
pub type FoundCallback = Box<dyn Fn(Work, u32, [u8; 32]) + Send + Sync>;

/// Single mining worker thread. Hashes continuously, polling for new work every ~1M nonces.
pub fn mining_worker(
    id: usize,
    num_workers: usize,
    shared_work: Arc<SharedWork>,
    stats: Arc<MiningStats>,
    on_found: Arc<FoundCallback>,
) {
    tracing::info!(worker = id, "Mining worker started");

    let mut last_gen: u64 = 0;
    let check_interval: u32 = 1 << 20; // ~1M nonces between checks

    loop {
        if stats.should_stop.load(Ordering::Relaxed) {
            break;
        }

        // Wait while paused
        while stats.paused.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if stats.should_stop.load(Ordering::Relaxed) {
                break;
            }
        }

        // Wait while this thread is throttled (id >= active count)
        while id >= stats.active_cpu_threads.load(Ordering::Relaxed) as usize {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if stats.should_stop.load(Ordering::Relaxed) {
                break;
            }
            // Break out if new work arrives (so we re-enter outer loop cleanly)
            if shared_work.generation.load(Ordering::Acquire) != last_gen {
                break;
            }
        }

        // Check for new work or wait for first work
        let (gen, work) = match shared_work.get_if_new(last_gen) {
            Some(gw) => gw,
            None => {
                // No new work — sleep briefly and retry
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        };
        last_gen = gen;

        // Divide nonce space among workers
        let total_nonces = u32::MAX as u64;
        let per_worker = total_nonces / num_workers as u64;
        let nonce_start = (id as u64 * per_worker) as u32;
        let nonce_end = if id == num_workers - 1 {
            u32::MAX
        } else {
            ((id as u64 + 1) * per_worker) as u32
        };

        tracing::trace!(worker = id, nonce_start, nonce_end, job = work.job_id, "Mining");

        // Hash continuously within our nonce range, checking for new work every ~1M hashes
        let mut nonce = nonce_start;
        while nonce < nonce_end {
            if stats.should_stop.load(Ordering::Relaxed) {
                break;
            }

            // Check for new work — if generation changed, break out and grab it
            if shared_work.generation.load(Ordering::Acquire) != last_gen {
                break;
            }

            // Check if this thread has been throttled
            if id >= stats.active_cpu_threads.load(Ordering::Relaxed) as usize {
                break;
            }

            // Hash a batch of ~1M nonces
            let batch_end = (nonce as u64 + check_interval as u64).min(nonce_end as u64) as u32;
            let (result, _) = hash_range_midstate(
                &work.header,
                nonce,
                batch_end,
                &work.target,
                &stats.should_stop,
                check_interval,
                Some(&stats),
            );

            match result {
                HashResult::Found { nonce: found_nonce, hash } => {
                    tracing::info!(
                        worker = id,
                        nonce = found_nonce,
                        hash = hex_encode(&hash),
                        "Found valid hash!"
                    );
                    on_found(work.clone(), found_nonce, hash);
                }
                HashResult::Stopped => {
                    break;
                }
                HashResult::Exhausted => {}
            }

            nonce = batch_end;
        }
    }

    tracing::info!(worker = id, "Mining worker exiting");
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a dummy Work for testing.
    fn dummy_work() -> Work {
        Work {
            header: [0u8; 80],
            target: [0xFFu8; 32],
            job_id: "test-job-1".to_string(),
            extranonce: 42,
            ntime: "6050a3b0".to_string(),
        }
    }

    #[test]
    fn shared_work_new_starts_with_no_work_and_generation_zero() {
        let sw = SharedWork::new();
        assert_eq!(sw.generation.load(Ordering::Acquire), 0);
        let work = sw.work.lock().unwrap();
        assert!(work.is_none());
    }

    #[test]
    fn shared_work_update_stores_work_and_increments_generation() {
        let sw = SharedWork::new();
        let work = dummy_work();

        sw.update(work.clone());

        assert_eq!(sw.generation.load(Ordering::Acquire), 1);
        {
            let guard = sw.work.lock().unwrap();
            let stored = guard.as_ref().expect("work should be Some after update");
            assert_eq!(stored.job_id, "test-job-1");
            assert_eq!(stored.extranonce, 42);
            assert_eq!(stored.ntime, "6050a3b0");
        }

        // Update again — generation should increment to 2
        sw.update(dummy_work());
        assert_eq!(sw.generation.load(Ordering::Acquire), 2);
    }

    #[test]
    fn get_if_new_returns_none_when_generation_matches() {
        let sw = SharedWork::new();
        sw.update(dummy_work());

        let current_gen = sw.generation.load(Ordering::Acquire);
        // Passing the current generation should return None (no new work)
        assert!(sw.get_if_new(current_gen).is_none());
    }

    #[test]
    fn get_if_new_returns_some_when_generation_differs() {
        let sw = SharedWork::new();
        sw.update(dummy_work());

        // last_gen=0 but current generation is 1, so there is new work
        let result = sw.get_if_new(0);
        assert!(result.is_some());

        let (gen, work) = result.unwrap();
        assert_eq!(gen, 1);
        assert_eq!(work.job_id, "test-job-1");
        assert_eq!(work.extranonce, 42);
    }

    #[test]
    fn get_if_new_returns_none_when_no_work_exists() {
        let sw = SharedWork::new();

        // Generation is 0 and last_gen is 0, so no new work
        assert!(sw.get_if_new(0).is_none());

        // Even if we somehow ask with a different generation, there is no work
        // to return, so it should still be None.
        // Force generation forward without setting work:
        sw.generation.store(1, Ordering::Release);
        let result = sw.get_if_new(0);
        assert!(result.is_none());
    }

    #[test]
    fn work_struct_can_be_cloned_and_fields_accessed() {
        let work = Work {
            header: [0xABu8; 80],
            target: [0xCDu8; 32],
            job_id: "clone-test".to_string(),
            extranonce: 12345,
            ntime: "deadbeef".to_string(),
        };

        let cloned = work.clone();

        assert_eq!(cloned.header, [0xABu8; 80]);
        assert_eq!(cloned.target, [0xCDu8; 32]);
        assert_eq!(cloned.job_id, "clone-test");
        assert_eq!(cloned.extranonce, 12345);
        assert_eq!(cloned.ntime, "deadbeef");

        // Verify the clone is independent
        assert_eq!(work.job_id, cloned.job_id);
        assert_eq!(work.ntime, cloned.ntime);
    }

    #[test]
    fn shared_work_multiple_updates_track_generation() {
        let sw = SharedWork::new();

        for i in 1..=5 {
            let mut work = dummy_work();
            work.job_id = format!("job-{i}");
            sw.update(work);
            assert_eq!(sw.generation.load(Ordering::Acquire), i);
        }

        // After 5 updates, get_if_new(3) should return the latest work with gen 5
        let result = sw.get_if_new(3);
        assert!(result.is_some());
        let (gen, work) = result.unwrap();
        assert_eq!(gen, 5);
        assert_eq!(work.job_id, "job-5");
    }
}
