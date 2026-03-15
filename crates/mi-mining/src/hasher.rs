use mi_core::MiningStats;
use sha2::{compress256, digest::generic_array::GenericArray, Digest, Sha256};
use std::sync::Arc;

const SHA256_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
    0x5be0cd19,
];

/// Result of attempting to find a valid nonce.
pub enum HashResult {
    /// Found a hash that meets the target.
    Found { nonce: u32, hash: [u8; 32] },
    /// Exhausted the nonce range without finding a valid hash.
    Exhausted,
    /// Stopped early (e.g., new work arrived or shutdown).
    Stopped,
}

/// Hash a nonce range using raw SHA-256 compression with ARM SHA2 hardware acceleration.
/// Uses midstate optimization: pre-compress first 64 bytes, then only compress remaining
/// 16 bytes (with padding) per nonce. Reports hashes incrementally to stats.
pub fn hash_range_midstate(
    header_bytes: &[u8; 80],
    nonce_start: u32,
    nonce_end: u32,
    target: &[u8; 32],
    should_stop: &std::sync::atomic::AtomicBool,
    check_interval: u32,
    stats: Option<&Arc<MiningStats>>,
) -> (HashResult, u64) {
    // Compute midstate: SHA-256 state after compressing the first 64-byte block
    let mut midstate = SHA256_INIT;
    compress256(
        &mut midstate,
        std::slice::from_ref(GenericArray::from_slice(&header_bytes[0..64])),
    );

    // Pre-build second block for first SHA-256 (the block after midstate):
    // [tail_12_bytes][nonce_4][0x80][zeros_37][bit_length_8]
    // Only bytes 12-15 (nonce) change per iteration.
    let mut block_a = [0u8; 64];
    block_a[0..12].copy_from_slice(&header_bytes[64..76]);
    // block_a[12..16] = nonce, set per iteration
    block_a[16] = 0x80;
    // block_a[17..56] = zeros (already zero)
    block_a[56..64].copy_from_slice(&640u64.to_be_bytes()); // 80 * 8 = 640 bits

    // Pre-build block for second SHA-256 (hash-of-hash):
    // [first_hash_32][0x80][zeros_23][bit_length_8]
    // Only bytes 0-31 (first hash output) change per iteration.
    let mut block_b = [0u8; 64];
    // block_b[0..32] = first hash, set per iteration
    block_b[32] = 0x80;
    // block_b[33..56] = zeros (already zero)
    block_b[56..64].copy_from_slice(&256u64.to_be_bytes()); // 32 * 8 = 256 bits

    // Pre-compute target's most significant word for fast early-exit comparison.
    // Both hash and target are big-endian, so state[0] = bytes[0..3] = MSW.
    let target_w0 = u32::from_be_bytes([target[0], target[1], target[2], target[3]]);

    let mut hashes_done: u64 = 0;
    let mut hashes_since_report: u64 = 0;
    let mut counter: u32 = check_interval;
    let mut nonce = nonce_start;

    while nonce < nonce_end {
        // Countdown-based check (avoids modulo per iteration)
        counter -= 1;
        if counter == 0 {
            counter = check_interval;
            if let Some(s) = stats {
                s.add_cpu_hashes(hashes_since_report);
                hashes_since_report = 0;
            }
            if should_stop.load(std::sync::atomic::Ordering::Relaxed) {
                return (HashResult::Stopped, hashes_done);
            }
        }

        // Set nonce in the pre-built block
        block_a[12..16].copy_from_slice(&nonce.to_le_bytes());

        // First SHA-256: compress midstate with the tail block
        let mut state = midstate;
        compress256(
            &mut state,
            std::slice::from_ref(GenericArray::from_slice(&block_a)),
        );

        // Write first hash output into second block (big-endian state words)
        for i in 0..8 {
            block_b[i * 4..(i + 1) * 4].copy_from_slice(&state[i].to_be_bytes());
        }

        // Second SHA-256: compress initial state with the first hash
        let mut state2 = SHA256_INIT;
        compress256(
            &mut state2,
            std::slice::from_ref(GenericArray::from_slice(&block_b)),
        );

        // Early exit: compare the most significant word of the hash against the
        // target. For real Bitcoin difficulty, target_w0 == 0 so any state2[0] != 0
        // means the hash is too large (filters ~99.99999977% of hashes).
        // For easy targets (testing), target_w0 == 0xFFFFFFFF so we always proceed.
        if state2[0] <= target_w0 {
            let mut hash = [0u8; 32];
            for i in 0..8 {
                hash[i * 4..(i + 1) * 4].copy_from_slice(&state2[i].to_be_bytes());
            }

            if meets_target_be(&hash, target) {
                if let Some(s) = stats {
                    s.add_cpu_hashes(hashes_since_report + 1);
                }
                return (HashResult::Found { nonce, hash }, hashes_done + 1);
            }
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

        if meets_target_be(&hash, target) {
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

/// Check if a SHA-256d hash meets the target. Both are in big-endian byte order
/// (byte[0] is most significant). Returns true if hash <= target.
#[inline(always)]
fn meets_target_be(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in 0..32 {
        if hash[i] < target[i] {
            return true;
        }
        if hash[i] > target[i] {
            return false;
        }
    }
    true // equal
}

/// Validate hasher correctness by hashing the Bitcoin genesis block header
/// and verifying the output matches the known hash. Returns Ok(()) if correct,
/// or Err with details if the hash doesn't match.
pub fn validate_hasher() -> Result<(), String> {
    let header = mi_core::bitcoin_util::BlockHeader {
        version: 1,
        prev_hash: [0u8; 32],
        merkle_root: [
            0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e,
            0x67, 0x76, 0x8f, 0x61, 0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32,
            0x3a, 0x9f, 0xb8, 0xaa, 0x4b, 0x1e, 0x5e, 0x4a,
        ],
        timestamp: 1231006505,
        bits: 0x1d00ffff,
        nonce: 2083236893,
    };

    let header_bytes = header.serialize();
    let target = [0xFF; 32];
    let stop = std::sync::atomic::AtomicBool::new(false);
    let genesis_nonce = 2083236893u32;

    let (result, _) = hash_range_midstate(
        &header_bytes,
        genesis_nonce,
        genesis_nonce.wrapping_add(1),
        &target,
        &stop,
        1 << 20,
        None,
    );

    match result {
        HashResult::Found { hash, .. } => {
            // Genesis block hash in internal byte order — first 4 bytes of
            // display order (reversed) should be 0x00000000, byte 5 = 0x00, byte 6 = 0x19
            let mut display = hash;
            display.reverse();
            if display[0..6] == [0x00, 0x00, 0x00, 0x00, 0x00, 0x19] {
                Ok(())
            } else {
                Err(format!(
                    "Genesis block hash mismatch: got {}",
                    hex::encode(display)
                ))
            }
        }
        _ => Err("Failed to hash genesis block (nonce not found with easy target)".to_string()),
    }
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

    /// Reference SHA-256d using the Digest API for correctness verification.
    fn sha256d_reference(data: &[u8]) -> [u8; 32] {
        let first = Sha256::digest(data);
        Sha256::digest(first).into()
    }

    #[test]
    fn test_hash_range_midstate_matches_simple() {
        let header = test_header();
        let target = [0xff; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);

        let (r1, _) = hash_range_midstate(&header, 0, 10, &target, &stop, 1 << 20, None);
        let (r2, _) = hash_range_simple(&header, 0, 10, &target, &stop, 1 << 20);

        match (r1, r2) {
            (
                HashResult::Found {
                    nonce: n1,
                    hash: h1,
                },
                HashResult::Found {
                    nonce: n2,
                    hash: h2,
                },
            ) => {
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
        let (result, hashes) =
            hash_range_midstate(&header, 0, 100, &target, &stop, 1 << 20, None);
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
        let (result, hashes) =
            hash_range_midstate(&header, 0, 10, &target, &stop, 1 << 20, None);
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
            (
                HashResult::Found {
                    nonce: n1,
                    hash: h1,
                },
                HashResult::Found {
                    nonce: n2,
                    hash: h2,
                },
            ) => {
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
        let (_, hashes) =
            hash_range_midstate(&header, 0, 500, &target, &stop, check, Some(&stats));
        assert_eq!(hashes, 500);

        let reported = stats.cpu_hashes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reported, 500);
    }

    /// Verify that our raw-compress midstate produces identical hashes to the Digest API
    /// across many nonces and different header contents.
    #[test]
    fn test_raw_compress_matches_digest_api() {
        let header = test_header();

        for nonce in [0u32, 1, 42, 255, 256, 1000, 65535, 0xDEADBEEF, u32::MAX - 1] {
            let mut h = header;
            h[76..80].copy_from_slice(&nonce.to_le_bytes());

            let reference = sha256d_reference(&h);

            // Use our optimized path with an easy target to always trigger the full comparison
            let target = [0xFF; 32];
            let stop = std::sync::atomic::AtomicBool::new(false);
            let (result, _) =
                hash_range_midstate(&h, nonce, nonce.wrapping_add(1), &target, &stop, 1 << 20, None);

            match result {
                HashResult::Found { hash, nonce: n, .. } => {
                    assert_eq!(n, nonce);
                    assert_eq!(hash, reference, "Hash mismatch at nonce {nonce}");
                }
                _ => panic!("Should have found nonce {nonce} with easy target"),
            }
        }
    }

    /// Verify correctness with the Bitcoin genesis block.
    #[test]
    fn test_genesis_block_via_compress() {
        let header = mi_core::bitcoin_util::BlockHeader {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [
                0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e,
                0x67, 0x76, 0x8f, 0x61, 0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32,
                0x3a, 0x9f, 0xb8, 0xaa, 0x4b, 0x1e, 0x5e, 0x4a,
            ],
            timestamp: 1231006505,
            bits: 0x1d00ffff,
            nonce: 2083236893,
        };

        let header_bytes = header.serialize();
        let reference = sha256d_reference(&header_bytes);

        // Use optimized path
        let target = [0xFF; 32];
        let stop = std::sync::atomic::AtomicBool::new(false);
        let genesis_nonce = 2083236893u32;
        let (result, _) = hash_range_midstate(
            &header_bytes,
            genesis_nonce,
            genesis_nonce.wrapping_add(1),
            &target,
            &stop,
            1 << 20,
            None,
        );

        match result {
            HashResult::Found { hash, .. } => {
                assert_eq!(hash, reference);
                // Genesis block hash has leading zeros when displayed (reversed)
                let mut display = hash;
                display.reverse();
                // First 4 bytes of display hash should be zero
                assert_eq!(&display[0..4], &[0x00, 0x00, 0x00, 0x00]);
                // Byte 5 should be 0x00, byte 6 should be 0x19
                assert_eq!(display[4], 0x00);
                assert_eq!(display[5], 0x19);
            }
            _ => panic!("Should find genesis nonce with easy target"),
        }
    }

    #[test]
    fn test_validate_hasher_passes() {
        validate_hasher().expect("Hasher self-check should pass");
    }

    /// Verify meets_target_be handles edge cases correctly.
    #[test]
    fn test_meets_target_be_all_zero_hash() {
        let hash = [0u8; 32];
        let target = [0xFF; 32];
        assert!(meets_target_be(&hash, &target));
    }

    #[test]
    fn test_meets_target_be_all_ones_hash() {
        let hash = [0xFF; 32];
        let target = [0u8; 32];
        assert!(!meets_target_be(&hash, &target));
    }

    #[test]
    fn test_meets_target_be_equal() {
        let val = [0x42; 32];
        assert!(meets_target_be(&val, &val));
    }

    #[test]
    fn test_meets_target_be_first_byte_decides() {
        let mut hash = [0u8; 32];
        let mut target = [0u8; 32];
        hash[0] = 0x01;
        target[0] = 0x02;
        assert!(meets_target_be(&hash, &target));

        hash[0] = 0x03;
        assert!(!meets_target_be(&hash, &target));
    }

    /// Stress test: verify midstate and simple produce identical results over a large range.
    #[test]
    fn test_midstate_vs_simple_large_range() {
        let header = test_header();
        let target = [0u8; 32]; // impossible target — both should exhaust
        let stop = std::sync::atomic::AtomicBool::new(false);

        let (r1, h1) = hash_range_midstate(&header, 0, 10_000, &target, &stop, 1 << 20, None);
        let (r2, h2) = hash_range_simple(&header, 0, 10_000, &target, &stop, 1 << 20);

        assert!(matches!(r1, HashResult::Exhausted));
        assert!(matches!(r2, HashResult::Exhausted));
        assert_eq!(h1, h2);
    }

    /// Verify that different header contents produce different hashes.
    #[test]
    fn test_different_headers_different_hashes() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let target = [0xFF; 32];

        let h1 = test_header();
        let mut h2 = test_header();
        h2[40] = 0xCC; // change one byte in merkle root

        let (r1, _) = hash_range_midstate(&h1, 0, 1, &target, &stop, 1 << 20, None);
        let (r2, _) = hash_range_midstate(&h2, 0, 1, &target, &stop, 1 << 20, None);

        match (r1, r2) {
            (HashResult::Found { hash: a, .. }, HashResult::Found { hash: b, .. }) => {
                assert_ne!(a, b, "Different headers should produce different hashes");
            }
            _ => panic!("Both should find with easy target"),
        }
    }
}
