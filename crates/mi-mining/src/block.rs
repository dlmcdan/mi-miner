use mi_core::bitcoin_util::{build_coinbase_tx, sha256d, BlockHeader};

/// Parameters received from stratum `mining.notify` or getblocktemplate.
#[derive(Debug, Clone)]
pub struct BlockTemplate {
    pub job_id: String,
    pub version: i32,
    pub prev_hash: [u8; 32],
    pub coinbase_1: Vec<u8>,
    pub coinbase_2: Vec<u8>,
    pub merkle_branches: Vec<[u8; 32]>,
    pub bits: u32,
    pub timestamp: u32,
    pub clean_jobs: bool,
    pub extranonce_size: usize,
}

impl BlockTemplate {
    /// Build a block header for mining with the given extranonce.
    pub fn build_header(&self, extranonce: u64) -> (BlockHeader, [u8; 80]) {
        let coinbase_tx = build_coinbase_tx(
            &self.coinbase_1,
            extranonce,
            self.extranonce_size,
            &self.coinbase_2,
        );

        let coinbase_hash = sha256d(&coinbase_tx);
        let merkle_root = self.compute_merkle_root_from_branches(&coinbase_hash);

        let header = BlockHeader {
            version: self.version,
            prev_hash: self.prev_hash,
            merkle_root,
            timestamp: self.timestamp,
            bits: self.bits,
            nonce: 0,
        };

        let serialized = header.serialize();
        (header, serialized)
    }

    /// Compute merkle root by combining coinbase hash with merkle branches.
    fn compute_merkle_root_from_branches(&self, coinbase_hash: &[u8; 32]) -> [u8; 32] {
        let mut current = *coinbase_hash;

        for branch in &self.merkle_branches {
            let mut combined = [0u8; 64];
            combined[0..32].copy_from_slice(&current);
            combined[32..64].copy_from_slice(branch);
            current = sha256d(&combined);
        }

        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_header() {
        let template = BlockTemplate {
            job_id: "test".to_string(),
            version: 0x20000000,
            prev_hash: [0xaa; 32],
            coinbase_1: vec![0x01, 0x02, 0x03],
            coinbase_2: vec![0x04, 0x05, 0x06],
            merkle_branches: vec![],
            bits: 0x1d00ffff,
            timestamp: 1700000000,
            clean_jobs: true,
            extranonce_size: 4,
        };

        let (header, serialized) = template.build_header(0);
        assert_eq!(header.version, 0x20000000);
        assert_eq!(header.bits, 0x1d00ffff);
        assert_eq!(serialized.len(), 80);
    }

    #[test]
    fn test_build_header_preserves_prev_hash() {
        let prev = [0x42; 32];
        let template = BlockTemplate {
            job_id: "t".to_string(),
            version: 1,
            prev_hash: prev,
            coinbase_1: vec![],
            coinbase_2: vec![],
            merkle_branches: vec![],
            bits: 0x1d00ffff,
            timestamp: 1000,
            clean_jobs: false,
            extranonce_size: 4,
        };
        let (header, _) = template.build_header(0);
        assert_eq!(header.prev_hash, prev);
        assert_eq!(header.timestamp, 1000);
    }

    #[test]
    fn test_different_extranonce_different_merkle_root() {
        let template = BlockTemplate {
            job_id: "t".to_string(),
            version: 1,
            prev_hash: [0; 32],
            coinbase_1: vec![0x01],
            coinbase_2: vec![0x02],
            merkle_branches: vec![],
            bits: 0x1d00ffff,
            timestamp: 1000,
            clean_jobs: false,
            extranonce_size: 4,
        };
        let (h1, _) = template.build_header(0);
        let (h2, _) = template.build_header(1);
        // Different extranonce should produce different merkle roots
        assert_ne!(h1.merkle_root, h2.merkle_root);
    }

    #[test]
    fn test_build_header_with_merkle_branches() {
        let branch = [0xBB; 32];
        let template = BlockTemplate {
            job_id: "t".to_string(),
            version: 1,
            prev_hash: [0; 32],
            coinbase_1: vec![0x01],
            coinbase_2: vec![0x02],
            merkle_branches: vec![branch],
            bits: 0x1d00ffff,
            timestamp: 1000,
            clean_jobs: false,
            extranonce_size: 4,
        };
        let (header, bytes) = template.build_header(0);
        assert_eq!(bytes.len(), 80);
        // With a merkle branch, the root should differ from no-branch case
        let (header_no_branch, _) = {
            let mut t = template.clone();
            t.merkle_branches = vec![];
            t.build_header(0)
        };
        assert_ne!(header.merkle_root, header_no_branch.merkle_root);
    }

    #[test]
    fn test_header_nonce_starts_at_zero() {
        let template = BlockTemplate {
            job_id: "t".to_string(),
            version: 1,
            prev_hash: [0; 32],
            coinbase_1: vec![],
            coinbase_2: vec![],
            merkle_branches: vec![],
            bits: 0,
            timestamp: 0,
            clean_jobs: false,
            extranonce_size: 4,
        };
        let (header, _) = template.build_header(0);
        assert_eq!(header.nonce, 0);
    }

    #[test]
    fn test_extranonce_produces_completely_different_headers() {
        let template = BlockTemplate {
            job_id: "en-test".to_string(),
            version: 0x20000000,
            prev_hash: [0xAA; 32],
            coinbase_1: vec![0x01, 0x02, 0x03, 0x04],
            coinbase_2: vec![0x05, 0x06, 0x07, 0x08],
            merkle_branches: vec![],
            bits: 0x1d00ffff,
            timestamp: 1700000000,
            clean_jobs: true,
            extranonce_size: 4,
        };

        let (_h0, bytes0) = template.build_header(0);
        let (_h1, bytes1) = template.build_header(1);

        // Bytes 0-35 (version + prev_hash) should be identical
        assert_eq!(&bytes0[0..36], &bytes1[0..36]);

        // Bytes 36-67 (merkle root) MUST differ — this is the key separation
        assert_ne!(&bytes0[36..68], &bytes1[36..68]);

        // Bytes 68-79 (timestamp + bits + nonce) should be identical
        assert_eq!(&bytes0[68..80], &bytes1[68..80]);

        // Same job metadata
        assert_eq!(_h0.version, _h1.version);
        assert_eq!(_h0.prev_hash, _h1.prev_hash);
        assert_eq!(_h0.timestamp, _h1.timestamp);
        assert_eq!(_h0.bits, _h1.bits);
    }

    #[test]
    fn test_extranonce_hashes_are_independent() {
        // Verify that hashing the same nonce on headers with different extranonce
        // produces different SHA-256d results (independent hash spaces)
        use mi_core::bitcoin_util::sha256d;

        let template = BlockTemplate {
            job_id: "hash-test".to_string(),
            version: 0x20000000,
            prev_hash: [0; 32],
            coinbase_1: vec![0x01],
            coinbase_2: vec![0x02],
            merkle_branches: vec![],
            bits: 0x1d00ffff,
            timestamp: 1700000000,
            clean_jobs: false,
            extranonce_size: 4,
        };

        let (_, mut header0) = template.build_header(0);
        let (_, mut header1) = template.build_header(1);

        // Set the same nonce on both
        let nonce: u32 = 42;
        header0[76..80].copy_from_slice(&nonce.to_le_bytes());
        header1[76..80].copy_from_slice(&nonce.to_le_bytes());

        let hash0 = sha256d(&header0);
        let hash1 = sha256d(&header1);

        // Different extranonce → different merkle root → different hash
        assert_ne!(hash0, hash1, "Same nonce on different extranonces must produce different hashes");
    }
}
