use super::messages::MiningNotify;
use mi_core::bitcoin_util::nbits_to_target;
use mi_mining::block::BlockTemplate;

/// Maintains Stratum session state and converts notifications to mining work.
pub struct StratumSession {
    pub extranonce1: Vec<u8>,
    pub extranonce2_size: usize,
    pub current_difficulty: f64,
    pub current_job: Option<MiningNotify>,
    worker: String,
}

impl StratumSession {
    pub fn new(worker: String) -> Self {
        Self {
            extranonce1: Vec::new(),
            extranonce2_size: 4,
            current_difficulty: 1.0,
            current_job: None,
            worker,
        }
    }

    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Set extranonce from subscribe result.
    pub fn set_extranonce(&mut self, extranonce1_hex: &str, extranonce2_size: usize) {
        self.extranonce1 = hex_decode(extranonce1_hex);
        self.extranonce2_size = extranonce2_size;
    }

    /// Process a mining.notify and produce a BlockTemplate.
    pub fn process_notify(&mut self, notify: MiningNotify) -> Result<BlockTemplate, String> {
        let prev_hash = decode_prev_hash(&notify.prev_hash)?;
        let coinbase_1 = hex_decode(&notify.coinbase_1);
        let coinbase_2 = hex_decode(&notify.coinbase_2);

        let mut merkle_branches = Vec::new();
        for branch_hex in &notify.merkle_branches {
            let bytes = hex_decode(branch_hex);
            if bytes.len() != 32 {
                return Err(format!("Invalid merkle branch length: {}", bytes.len()));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            merkle_branches.push(arr);
        }

        let version = i32::from_str_radix(&notify.version, 16)
            .map_err(|e| format!("Invalid version: {e}"))?;
        let bits =
            u32::from_str_radix(&notify.nbits, 16).map_err(|e| format!("Invalid nbits: {e}"))?;
        let timestamp =
            u32::from_str_radix(&notify.ntime, 16).map_err(|e| format!("Invalid ntime: {e}"))?;

        // Build coinbase with extranonce1 prepended
        let mut full_coinbase_1 = coinbase_1;
        full_coinbase_1.extend_from_slice(&self.extranonce1);

        let template = BlockTemplate {
            job_id: notify.job_id.clone(),
            version,
            prev_hash,
            coinbase_1: full_coinbase_1,
            coinbase_2,
            merkle_branches,
            bits,
            timestamp,
            clean_jobs: notify.clean_jobs,
            extranonce_size: self.extranonce2_size,
        };

        self.current_job = Some(notify);
        Ok(template)
    }

    /// Compute the share target from current difficulty.
    pub fn share_target(&self) -> [u8; 32] {
        difficulty_to_target(self.current_difficulty)
    }
}

/// Decode stratum prev_hash (which is in a weird 8-char chunk reversed order).
fn decode_prev_hash(hex: &str) -> Result<[u8; 32], String> {
    let bytes = hex_decode(hex);
    if bytes.len() != 32 {
        return Err(format!(
            "Invalid prev_hash length: {} (expected 32)",
            bytes.len()
        ));
    }

    // Stratum sends prev_hash as 8 groups of 4 bytes, each group byte-reversed
    let mut result = [0u8; 32];
    for i in 0..8 {
        for j in 0..4 {
            result[i * 4 + j] = bytes[i * 4 + (3 - j)];
        }
    }
    Ok(result)
}

/// Convert difficulty to target bytes (big-endian 256-bit).
/// target = diff1_target / difficulty, computed via byte-level long division.
fn difficulty_to_target(difficulty: f64) -> [u8; 32] {
    let diff1_target = nbits_to_target(0x1d00ffff);

    if difficulty <= 0.0 || difficulty == 1.0 {
        return diff1_target;
    }

    // Long division: diff1_target (256-bit big-endian) / difficulty
    // Use u32 intermediates to handle quotients > 255 (when difficulty < 1)
    let mut wide = [0u32; 32];
    let mut remainder = 0.0f64;

    for i in 0..32 {
        let val = remainder * 256.0 + diff1_target[i] as f64;
        let quotient = (val / difficulty).floor();
        remainder = val - quotient * difficulty;
        wide[i] = quotient.min(u32::MAX as f64) as u32;
    }

    // Propagate carries from right to left
    let mut target = [0u8; 32];
    let mut carry = 0u32;
    for i in (0..32).rev() {
        let val = wide[i] + carry;
        target[i] = (val & 0xFF) as u8;
        carry = val >> 8;
    }

    target
}

fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_new() {
        let s = StratumSession::new("worker1".to_string());
        assert_eq!(s.worker(), "worker1");
        assert_eq!(s.extranonce2_size, 4);
        assert_eq!(s.current_difficulty, 1.0);
        assert!(s.current_job.is_none());
    }

    #[test]
    fn test_set_extranonce() {
        let mut s = StratumSession::new("w".to_string());
        s.set_extranonce("aabbccdd", 8);
        assert_eq!(s.extranonce1, vec![0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(s.extranonce2_size, 8);
    }

    #[test]
    fn test_hex_decode() {
        assert_eq!(hex_decode("aabbcc"), vec![0xaa, 0xbb, 0xcc]);
        assert_eq!(hex_decode("00ff"), vec![0x00, 0xff]);
        assert_eq!(hex_decode(""), Vec::<u8>::new());
    }

    #[test]
    fn test_decode_prev_hash_reverses_groups() {
        // 8 groups of 4 bytes each, each group reversed
        let hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let result = decode_prev_hash(hex).unwrap();
        // First group 01020304 reversed -> 04030201
        assert_eq!(&result[0..4], &[0x04, 0x03, 0x02, 0x01]);
        // Second group 05060708 reversed -> 08070605
        assert_eq!(&result[4..8], &[0x08, 0x07, 0x06, 0x05]);
    }

    #[test]
    fn test_decode_prev_hash_wrong_length() {
        let result = decode_prev_hash("aabb");
        assert!(result.is_err());
    }

    #[test]
    fn test_difficulty_to_target_diff1() {
        let target = difficulty_to_target(1.0);
        let diff1 = nbits_to_target(0x1d00ffff);
        assert_eq!(target, diff1);
    }

    #[test]
    fn test_difficulty_to_target_zero() {
        let target = difficulty_to_target(0.0);
        let diff1 = nbits_to_target(0x1d00ffff);
        assert_eq!(target, diff1); // falls back to diff1
    }

    #[test]
    fn test_difficulty_to_target_higher_diff_is_harder() {
        let t1 = difficulty_to_target(1.0);
        let t10 = difficulty_to_target(10.0);
        // Higher difficulty = smaller target (harder to meet)
        // Compare as big-endian: t10 should be <= t1
        assert!(t10 <= t1);
    }

    #[test]
    fn test_difficulty_to_target_diff2() {
        let target = difficulty_to_target(2.0);
        // diff1 = 0x00000000FFFF0000...
        // diff1/2 = 0x000000007FFF8000...
        assert_eq!(target[4], 0x7F);
        assert_eq!(target[5], 0xFF);
        assert_eq!(target[6], 0x80);
    }

    #[test]
    fn test_difficulty_to_target_fractional() {
        let t_half = difficulty_to_target(0.5);
        let t1 = difficulty_to_target(1.0);
        // diff < 1 means easier (larger target)
        assert!(t_half > t1);
        // diff1/0.5 = diff1*2 = 0x00000001FFFE0000...
        assert_eq!(t_half[3], 0x01);
        assert_eq!(t_half[4], 0xFF);
        assert_eq!(t_half[5], 0xFE);
    }

    #[test]
    fn test_process_notify() {
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aabb", 4);

        let notify = MiningNotify {
            job_id: "j1".to_string(),
            prev_hash: "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".to_string(),
            coinbase_1: "01020304".to_string(),
            coinbase_2: "05060708".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: true,
        };

        let template = session.process_notify(notify).unwrap();
        assert_eq!(template.job_id, "j1");
        assert_eq!(template.version, 0x20000000);
        assert_eq!(template.bits, 0x1d00ffff);
        assert!(template.clean_jobs);
        assert!(session.current_job.is_some());
    }

    #[test]
    fn test_process_notify_invalid_version() {
        let mut session = StratumSession::new("w".to_string());
        let notify = MiningNotify {
            job_id: "j".to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            coinbase_1: "".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec![],
            version: "ZZZZZZZZ".to_string(), // invalid hex
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: false,
        };
        let result = session.process_notify(notify);
        assert!(result.is_err());
    }

    #[test]
    fn test_share_target_at_diff1() {
        let session = StratumSession::new("w".to_string());
        let target = session.share_target();
        assert_eq!(target, nbits_to_target(0x1d00ffff));
    }

    #[test]
    fn test_difficulty_to_target_large_difficulty_1000() {
        let target = difficulty_to_target(1000.0);
        let diff1 = nbits_to_target(0x1d00ffff);
        // Must be strictly smaller than diff1
        assert!(target < diff1);
        // Must be 32 bytes
        assert_eq!(target.len(), 32);
        // Leading bytes should be zero (higher difficulty = smaller target)
        assert_eq!(target[0], 0);
        assert_eq!(target[1], 0);
        assert_eq!(target[2], 0);
        assert_eq!(target[3], 0);
    }

    #[test]
    fn test_difficulty_to_target_large_difficulty_65535() {
        let target = difficulty_to_target(65535.0);
        assert_eq!(target.len(), 32);
        let diff1 = nbits_to_target(0x1d00ffff);
        assert!(target < diff1);
        // At diff 65535, target should be very small
        let t1000 = difficulty_to_target(1000.0);
        assert!(target < t1000);
    }

    #[test]
    fn test_difficulty_to_target_always_32_bytes() {
        let difficulties = [0.001, 0.1, 0.5, 1.0, 2.0, 100.0, 1000.0, 65535.0, 1_000_000.0];
        for diff in difficulties {
            let target = difficulty_to_target(diff);
            assert_eq!(target.len(), 32, "target for diff {} was not 32 bytes", diff);
        }
    }

    #[test]
    fn test_difficulty_to_target_negative_returns_diff1() {
        let target = difficulty_to_target(-5.0);
        let diff1 = nbits_to_target(0x1d00ffff);
        assert_eq!(target, diff1);
    }

    #[test]
    fn test_difficulty_to_target_ordering_is_monotonic() {
        // Higher difficulty must always produce a smaller (or equal) target
        let diffs = [0.5, 1.0, 2.0, 10.0, 100.0, 1000.0, 65535.0];
        for i in 0..diffs.len() - 1 {
            let t_low = difficulty_to_target(diffs[i]);
            let t_high = difficulty_to_target(diffs[i + 1]);
            assert!(
                t_high <= t_low,
                "target for diff {} should be <= target for diff {}",
                diffs[i + 1],
                diffs[i]
            );
        }
    }

    #[test]
    fn test_process_notify_invalid_nbits_returns_error() {
        let mut session = StratumSession::new("w".to_string());
        let notify = MiningNotify {
            job_id: "j".to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            coinbase_1: "".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "ZZZZZZZZ".to_string(), // invalid hex for nbits
            ntime: "65a5e300".to_string(),
            clean_jobs: false,
        };
        let result = session.process_notify(notify);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid nbits"));
    }

    #[test]
    fn test_process_notify_invalid_ntime_returns_error() {
        let mut session = StratumSession::new("w".to_string());
        let notify = MiningNotify {
            job_id: "j".to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            coinbase_1: "".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "XXXXXXXX".to_string(), // invalid hex for ntime
            clean_jobs: false,
        };
        let result = session.process_notify(notify);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid ntime"));
    }

    #[test]
    fn test_process_notify_sets_current_job() {
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aabb", 4);
        assert!(session.current_job.is_none());

        let notify = MiningNotify {
            job_id: "myjob42".to_string(),
            prev_hash: "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
                .to_string(),
            coinbase_1: "aabbccdd".to_string(),
            coinbase_2: "11223344".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: true,
        };

        let template = session.process_notify(notify).unwrap();
        assert_eq!(template.job_id, "myjob42");
        assert!(template.clean_jobs);

        // Verify current_job is set
        let job = session.current_job.as_ref().unwrap();
        assert_eq!(job.job_id, "myjob42");
        assert!(job.clean_jobs);
    }

    #[test]
    fn test_process_notify_replaces_current_job() {
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aa", 4);

        let make_notify = |id: &str| MiningNotify {
            job_id: id.to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            coinbase_1: "".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: false,
        };

        session.process_notify(make_notify("first")).unwrap();
        assert_eq!(session.current_job.as_ref().unwrap().job_id, "first");

        session.process_notify(make_notify("second")).unwrap();
        assert_eq!(session.current_job.as_ref().unwrap().job_id, "second");
    }

    #[test]
    fn test_process_notify_invalid_merkle_branch_length() {
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("aa", 4);

        let notify = MiningNotify {
            job_id: "j".to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            coinbase_1: "".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec!["aabb".to_string()], // only 2 bytes, need 32
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: false,
        };

        let result = session.process_notify(notify);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid merkle branch length"));
    }

    #[test]
    fn test_process_notify_coinbase_includes_extranonce1() {
        let mut session = StratumSession::new("w".to_string());
        session.set_extranonce("deadbeef", 4);

        let notify = MiningNotify {
            job_id: "j".to_string(),
            prev_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            coinbase_1: "01020304".to_string(),
            coinbase_2: "".to_string(),
            merkle_branches: vec![],
            version: "20000000".to_string(),
            nbits: "1d00ffff".to_string(),
            ntime: "65a5e300".to_string(),
            clean_jobs: false,
        };

        let template = session.process_notify(notify).unwrap();
        // coinbase_1 should be original bytes + extranonce1
        assert_eq!(
            template.coinbase_1,
            vec![0x01, 0x02, 0x03, 0x04, 0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn test_share_target_changes_with_difficulty() {
        let mut session = StratumSession::new("w".to_string());

        let target_diff1 = session.share_target();

        session.current_difficulty = 100.0;
        let target_diff100 = session.share_target();

        assert!(target_diff100 < target_diff1);
    }
}
