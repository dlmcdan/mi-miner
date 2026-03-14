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
fn difficulty_to_target(difficulty: f64) -> [u8; 32] {
    // Difficulty 1 target = 0x00000000FFFF0000...0000 (26 trailing zero bytes)
    let diff1_target = nbits_to_target(0x1d00ffff);

    if difficulty <= 0.0 || difficulty == 1.0 {
        return diff1_target;
    }

    // target = diff1_target / difficulty
    // Use f64 arithmetic on the significant portion.
    // diff1 ≈ 0xFFFF * 2^(26*8) = 0xFFFF * 2^208
    // We compute in log space then reconstruct.
    let diff1_mantissa: f64 = 0xFFFF as f64; // significant part
    let target_mantissa = diff1_mantissa / difficulty;

    // diff1 target has 0xFFFF at bytes [4..6] (big-endian), rest zeros.
    // After dividing by difficulty, we scale and place appropriately.
    let mut target = [0u8; 32];

    // Place the 2-byte mantissa region scaled by difficulty
    // For difficulty >= 1, the result fits in the same byte range or earlier
    if target_mantissa >= 1.0 {
        let val = target_mantissa as u32;
        // Place at bytes 4-5 (same position as diff1 target's 0xFFFF)
        target[4] = ((val >> 8) & 0xFF) as u8;
        target[5] = (val & 0xFF) as u8;
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
}
