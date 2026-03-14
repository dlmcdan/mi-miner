use crate::hasher::{hash_range_midstate, HashResult};
use crossbeam_channel::Receiver;
use mi_core::MiningStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Work unit sent to mining workers.
#[derive(Clone, Debug)]
pub struct Work {
    /// The 80-byte block header (nonce field will be overwritten).
    pub header: [u8; 80],
    /// Target for share/block validity.
    pub target: [u8; 32],
    /// Nonce range start (inclusive).
    pub nonce_start: u32,
    /// Nonce range end (exclusive).
    pub nonce_end: u32,
    /// Job ID from stratum (for submitting shares).
    pub job_id: String,
    /// Extranonce used for this work.
    pub extranonce: u64,
}

/// Callback when a valid nonce is found.
pub type FoundCallback = Box<dyn Fn(Work, u32, [u8; 32]) + Send + Sync>;

/// Single mining worker thread function.
pub fn mining_worker(
    id: usize,
    work_rx: Receiver<Work>,
    stats: Arc<MiningStats>,
    on_found: Arc<FoundCallback>,
) {
    tracing::info!(worker = id, "Mining worker started");

    loop {
        // Try to get new work; block until available
        let work = match work_rx.recv() {
            Ok(w) => w,
            Err(_) => {
                tracing::debug!(worker = id, "Work channel closed, worker exiting");
                break;
            }
        };

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

        tracing::trace!(
            worker = id,
            nonce_start = work.nonce_start,
            nonce_end = work.nonce_end,
            "Processing work"
        );

        let (result, hashes_done) = hash_range_midstate(
            &work.header,
            work.nonce_start,
            work.nonce_end,
            &work.target,
            &stats.should_stop,
            1 << 20, // Check stop every ~1M hashes
        );

        stats.add_cpu_hashes(hashes_done);

        match result {
            HashResult::Found { nonce, hash } => {
                tracing::info!(
                    worker = id,
                    nonce = nonce,
                    hash = hex::encode(hash),
                    "Found valid hash!"
                );
                on_found(work, nonce, hash);
            }
            HashResult::Exhausted => {
                tracing::trace!(worker = id, "Nonce range exhausted");
            }
            HashResult::Stopped => {
                tracing::trace!(worker = id, "Worker stopped");
                if stats.should_stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    tracing::info!(worker = id, "Mining worker exiting");
}

// Need hex for logging
mod hex {
    pub fn encode(data: impl AsRef<[u8]>) -> String {
        data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}
