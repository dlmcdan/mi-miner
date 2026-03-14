use mi_core::MiningStats;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Result of attempting to find a valid nonce.
pub enum HashResult {
    /// Found a hash that meets the target.
    Found { nonce: u32, hash: [u8; 32] },
    /// Exhausted the nonce range without finding a valid hash.
    Exhausted,
    /// Stopped early (e.g., new work arrived or shutdown).
    Stopped,
}

/// Hash a nonce range using the sha2 crate with hardware acceleration (ARM SHA2 on M4 Max).
/// Uses midstate optimization: pre-hash first 64 bytes, then only hash remaining 16 bytes per nonce.
/// Reports hashes incrementally to stats every `check_interval` hashes.
pub fn hash_range_midstate(
    header_bytes: &[u8; 80],
    nonce_start: u32,
    nonce_end: u32,
    target: &[u8; 32],
    should_stop: &std::sync::atomic::AtomicBool,
    check_interval: u32,
    stats: Option<&Arc<MiningStats>>,
) -> (HashResult, u64) {
    let mut midstate_hasher = Sha256::new();
    midstate_hasher.update(&header_bytes[0..64]);

    let mut tail = [0u8; 16];
    tail.copy_from_slice(&header_bytes[64..80]);

    let mut hashes_done: u64 = 0;
    let mut hashes_since_report: u64 = 0;
    let mut nonce = nonce_start;

    while nonce < nonce_end {
        if hashes_done % check_interval as u64 == 0 && hashes_done > 0 {
            // Report hashes incrementally
            if let Some(s) = stats {
                s.add_cpu_hashes(hashes_since_report);
                hashes_since_report = 0;
            }
            if should_stop.load(std::sync::atomic::Ordering::Relaxed) {
                return (HashResult::Stopped, hashes_done);
            }
        }

        tail[12..16].copy_from_slice(&nonce.to_le_bytes());

        let mut h1 = midstate_hasher.clone();
        h1.update(&tail);
        let first_hash = h1.finalize();

        let hash: [u8; 32] = Sha256::digest(first_hash).into();

        if meets_target_le(&hash, target) {
            if let Some(s) = stats {
                s.add_cpu_hashes(hashes_since_report + 1);
            }
            return (HashResult::Found { nonce, hash }, hashes_done + 1);
        }

        hashes_done += 1;
        hashes_since_report += 1;
        nonce = nonce.wrapping_add(1);
        if nonce == nonce_start && nonce_start != 0 {
            break;
        }
    }

    // Report any remaining hashes
    if let Some(s) = stats {
        s.add_cpu_hashes(hashes_since_report);
    }

    (HashResult::Exhausted, hashes_done)
}

/// Hash a nonce range without midstate optimization (for benchmarking comparison).
pub fn hash_range_simple(
    header_bytes: &[u8; 80],
    nonce_start: u32,
    nonce_end: u32,
    target: &[u8; 32],
    should_stop: &std::sync::atomic::AtomicBool,
    check_interval: u32,
) -> (HashResult, u64) {
    let mut buf = *header_bytes;
    let mut hashes_done: u64 = 0;
    let mut nonce = nonce_start;

    while nonce < nonce_end {
        if hashes_done % check_interval as u64 == 0
            && hashes_done > 0
            && should_stop.load(std::sync::atomic::Ordering::Relaxed)
        {
            return (HashResult::Stopped, hashes_done);
        }

        buf[76..80].copy_from_slice(&nonce.to_le_bytes());

        let first = Sha256::digest(&buf);
        let hash: [u8; 32] = Sha256::digest(first).into();

        if meets_target_le(&hash, target) {
            return (HashResult::Found { nonce, hash }, hashes_done + 1);
        }

        hashes_done += 1;
        nonce = nonce.wrapping_add(1);
        if nonce == nonce_start && nonce_start != 0 {
            break;
        }
    }

    (HashResult::Exhausted, hashes_done)
}

/// Check if hash meets target.
fn meets_target_le(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        let h = hash[31 - i];
        let t = target[31 - i];
        if h < t {
            return true;
        }
        if h > t {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_header() -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&1i32.to_le_bytes());
        h[4..36].fill(0xaa);
        h[36..68].fill(0xbb);
        h[68..72].copy_from_slice(&1700000000u32.to_le_bytes());
        h[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());
        h
    }

    #[test]
    fn test_hash_range_midstate_matches_simple() {
        let header = test_header();
        let target = [0xff; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);

        let (r1, _) = hash_range_midstate(&header, 0, 10, &target, &stop, 1 << 20, None);
        let (r2, _) = hash_range_simple(&header, 0, 10, &target, &stop, 1 << 20);

        match (r1, r2) {
            (HashResult::Found { nonce: n1, hash: h1 }, HashResult::Found { nonce: n2, hash: h2 }) => {
                assert_eq!(n1, n2);
                assert_eq!(h1, h2);
            }
            _ => panic!("Expected both to find a valid nonce"),
        }
    }

    #[test]
    fn test_hash_range_exhausted() {
        let header = test_header();
        let target = [0u8; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (result, hashes) = hash_range_midstate(&header, 0, 100, &target, &stop, 1 << 20, None);
        assert!(matches!(result, HashResult::Exhausted));
        assert_eq!(hashes, 100);
    }

    #[test]
    fn test_hash_range_stop_signal() {
        let header = test_header();
        let target = [0u8; 32];
        let stop = std::sync::atomic::AtomicBool::new(true);
        let (result, _) = hash_range_midstate(&header, 0, 1_000_000, &target, &stop, 1, None);
        assert!(matches!(result, HashResult::Stopped));
    }

    #[test]
    fn test_hash_range_finds_easy_target() {
        let header = test_header();
        let target = [0xFF; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (result, hashes) = hash_range_midstate(&header, 0, 10, &target, &stop, 1 << 20, None);
        match result {
            HashResult::Found { nonce, .. } => assert_eq!(nonce, 0),
            _ => panic!("Should have found nonce 0 with easy target"),
        }
        assert_eq!(hashes, 1);
    }

    #[test]
    fn test_hash_range_simple_finds_same_nonce() {
        let header = test_header();
        let target = [0xFF; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (r1, _) = hash_range_midstate(&header, 5, 10, &target, &stop, 1 << 20, None);
        let (r2, _) = hash_range_simple(&header, 5, 10, &target, &stop, 1 << 20);
        match (r1, r2) {
            (HashResult::Found { nonce: n1, hash: h1 }, HashResult::Found { nonce: n2, hash: h2 }) => {
                assert_eq!(n1, n2);
                assert_eq!(h1, h2);
            }
            _ => panic!("Both should find a nonce"),
        }
    }

    #[test]
    fn test_hash_count_accuracy() {
        let header = test_header();
        let target = [0u8; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (_, hashes) = hash_range_midstate(&header, 10, 20, &target, &stop, 1 << 20, None);
        assert_eq!(hashes, 10);
    }

    #[test]
    fn test_simple_hash_count_accuracy() {
        let header = test_header();
        let target = [0u8; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (_, hashes) = hash_range_simple(&header, 0, 50, &target, &stop, 1 << 20);
        assert_eq!(hashes, 50);
    }

    #[test]
    fn test_incremental_reporting() {
        let header = test_header();
        let target = [0u8; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let stats = MiningStats::new();
        let check = 100u32;

        // Hash 500 nonces with check_interval=100 — should report ~5 times
        let (_, hashes) = hash_range_midstate(&header, 0, 500, &target, &stop, check, Some(&stats));
        assert_eq!(hashes, 500);

        let reported = stats.cpu_hashes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reported, 500);
    }
}
