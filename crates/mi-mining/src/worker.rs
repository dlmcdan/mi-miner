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
