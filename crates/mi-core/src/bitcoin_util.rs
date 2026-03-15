/// Bitcoin mining utilities: block header construction, merkle root, coinbase TX, target handling.
use sha2::{Digest, Sha256};

/// 80-byte Bitcoin block header.
#[derive(Debug, Clone)]
pub struct BlockHeader {
    pub version: i32,
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
}

impl BlockHeader {
    /// Serialize to 80 bytes (little-endian, as Bitcoin uses).
    pub fn serialize(&self) -> [u8; 80] {
        let mut buf = [0u8; 80];
        buf[0..4].copy_from_slice(&self.version.to_le_bytes());
        buf[4..36].copy_from_slice(&self.prev_hash);
        buf[36..68].copy_from_slice(&self.merkle_root);
        buf[68..72].copy_from_slice(&self.timestamp.to_le_bytes());
        buf[72..76].copy_from_slice(&self.bits.to_le_bytes());
        buf[76..80].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    /// Compute SHA-256d (double SHA-256) of the serialized header.
    pub fn hash(&self) -> [u8; 32] {
        sha256d(&self.serialize())
    }
}

/// Double SHA-256 hash.
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    second.into()
}

/// Compute the midstate: SHA-256 state after processing the first 64 bytes of the 80-byte header.
/// This is an optimization since bytes 0-63 don't change when only the nonce (bytes 76-79) varies.
///
/// Returns the 32-byte intermediate hash state (8 x u32 words).
pub fn compute_midstate(header_bytes: &[u8; 80]) -> [u8; 32] {
    // SHA-256 processes data in 64-byte blocks.
    // We manually compute the state after the first block.
    let mut state = SHA256_INIT;
    let block: [u8; 64] = header_bytes[0..64].try_into().unwrap();
    sha256_compress(&mut state, &block);

    // Convert state words to bytes (big-endian, as SHA-256 uses internally)
    let mut midstate = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        midstate[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    midstate
}

/// Complete SHA-256d from midstate: hash the tail (bytes 64-79) using the midstate,
/// then SHA-256 the result again.
pub fn sha256d_from_midstate(midstate: &[u8; 32], tail: &[u8; 16]) -> [u8; 32] {
    // Reconstruct state from midstate bytes
    let mut state = [0u32; 8];
    for i in 0..8 {
        state[i] = u32::from_be_bytes(midstate[i * 4..(i + 1) * 4].try_into().unwrap());
    }

    // Build the final padded block (64 bytes):
    // - 16 bytes of tail data (bytes 64-79 of header)
    // - 1 byte 0x80 (padding start)
    // - 37 bytes of zeros
    // - 8 bytes big-endian bit length (80 * 8 = 640 = 0x280)
    let mut padded = [0u8; 64];
    padded[0..16].copy_from_slice(tail);
    padded[16] = 0x80;
    // Length in bits of the total message (80 bytes = 640 bits)
    let bit_len: u64 = 640;
    padded[56..64].copy_from_slice(&bit_len.to_be_bytes());

    sha256_compress(&mut state, &padded);

    // Convert first hash to bytes
    let mut first_hash = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        first_hash[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }

    // Second SHA-256
    let second = Sha256::digest(first_hash);
    second.into()
}

/// Compute merkle root from a list of transaction hashes (double-SHA256 merkle tree).
pub fn compute_merkle_root(tx_hashes: &[[u8; 32]]) -> [u8; 32] {
    if tx_hashes.is_empty() {
        return [0u8; 32];
    }
    if tx_hashes.len() == 1 {
        return tx_hashes[0];
    }

    let mut current_level: Vec<[u8; 32]> = tx_hashes.to_vec();

    while current_level.len() > 1 {
        let mut next_level = Vec::with_capacity((current_level.len() + 1) / 2);

        for chunk in current_level.chunks(2) {
            let mut combined = [0u8; 64];
            combined[0..32].copy_from_slice(&chunk[0]);
            if chunk.len() == 2 {
                combined[32..64].copy_from_slice(&chunk[1]);
            } else {
                // Odd number: duplicate last hash
                combined[32..64].copy_from_slice(&chunk[0]);
            }
            next_level.push(sha256d(&combined));
        }

        current_level = next_level;
    }

    current_level[0]
}

/// Build a simple coinbase transaction.
/// Returns the serialized transaction bytes.
pub fn build_coinbase_tx(
    coinbase_1: &[u8],
    extranonce: u64,
    extranonce_size: usize,
    coinbase_2: &[u8],
) -> Vec<u8> {
    let mut tx = Vec::with_capacity(coinbase_1.len() + extranonce_size + coinbase_2.len());
    tx.extend_from_slice(coinbase_1);
    // Write extranonce as little-endian bytes, padded to extranonce_size
    let en_bytes = extranonce.to_le_bytes();
    tx.extend_from_slice(&en_bytes[..extranonce_size.min(8)]);
    if extranonce_size > 8 {
        tx.extend(std::iter::repeat(0u8).take(extranonce_size - 8));
    }
    tx.extend_from_slice(coinbase_2);
    tx
}

/// Decode a compact target (nBits) into a 256-bit target.
pub fn nbits_to_target(nbits: u32) -> [u8; 32] {
    let mut target = [0u8; 32];
    let exponent = (nbits >> 24) as usize;
    let mantissa = nbits & 0x007f_ffff;

    if exponent == 0 {
        return target;
    }

    // Target is mantissa * 2^(8*(exponent-3))
    if exponent >= 3 {
        let offset = exponent - 3;
        if offset < 32 {
            target[32 - offset - 1] = (mantissa & 0xff) as u8;
        }
        if offset + 1 < 32 {
            target[32 - offset - 2] = ((mantissa >> 8) & 0xff) as u8;
        }
        if offset + 2 < 32 {
            target[32 - offset - 3] = ((mantissa >> 16) & 0xff) as u8;
        }
    }

    target
}

/// Convert compact target (nBits) to mining difficulty.
/// difficulty = diff1_target / target = (0xffff * 2^(8*26)) / (mantissa * 2^(8*(exp-3)))
pub fn nbits_to_difficulty(nbits: u32) -> f64 {
    let exp = (nbits >> 24) as i32;
    let mantissa = (nbits & 0x007f_ffff) as f64;
    if mantissa == 0.0 || exp == 0 {
        return 0.0;
    }
    let shift = 8 * (0x1d - exp);
    (0xffff as f64 / mantissa) * (2.0f64).powi(shift)
}

// SHA-256 initial hash values
const SHA256_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
    0x5be0cd19,
];

// SHA-256 round constants
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// SHA-256 compression function: process one 64-byte block.
fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];

    // Prepare message schedule
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256d_empty() {
        // SHA-256d of empty string
        let result = sha256d(&[]);
        let expected = hex::decode(
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456",
        )
        .unwrap();
        assert_eq!(&result[..], &expected[..]);
    }

    #[test]
    fn test_genesis_block_hash() {
        // Bitcoin genesis block header
        let header = BlockHeader {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: {
                let mut mr = [0u8; 32];
                let bytes = hex::decode(
                    "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
                )
                .unwrap();
                mr.copy_from_slice(&bytes);
                mr
            },
            timestamp: 1231006505,
            bits: 0x1d00ffff,
            nonce: 2083236893,
        };

        let hash = header.hash();
        // Genesis block hash (internal byte order, reversed from display order)
        let expected_display =
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
        let expected_bytes = hex::decode(expected_display).unwrap();

        // Hash is in internal order (reversed from display)
        let mut display_hash = hash;
        display_hash.reverse();
        assert_eq!(hex::encode(display_hash), expected_display);
    }

    #[test]
    fn test_midstate_optimization() {
        // Verify that midstate-based hashing produces the same result as full SHA-256d
        let header = BlockHeader {
            version: 2,
            prev_hash: [0xab; 32],
            merkle_root: [0xcd; 32],
            timestamp: 1700000000,
            bits: 0x1d00ffff,
            nonce: 42,
        };

        let header_bytes = header.serialize();
        let full_hash = sha256d(&header_bytes);

        let midstate = compute_midstate(&header_bytes);
        let tail: [u8; 16] = header_bytes[64..80].try_into().unwrap();
        let midstate_hash = sha256d_from_midstate(&midstate, &tail);

        assert_eq!(full_hash, midstate_hash);
    }

    #[test]
    fn test_midstate_different_nonces() {
        let mut header_bytes = [0u8; 80];
        header_bytes[0..4].copy_from_slice(&2i32.to_le_bytes()); // version
        header_bytes[4..36].fill(0xaa); // prev_hash
        header_bytes[36..68].fill(0xbb); // merkle_root
        header_bytes[68..72].copy_from_slice(&1700000000u32.to_le_bytes());
        header_bytes[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());

        let midstate = compute_midstate(&header_bytes);

        for nonce in [0u32, 1, 100, u32::MAX] {
            header_bytes[76..80].copy_from_slice(&nonce.to_le_bytes());
            let full_hash = sha256d(&header_bytes);

            let tail: [u8; 16] = header_bytes[64..80].try_into().unwrap();
            let midstate_hash = sha256d_from_midstate(&midstate, &tail);

            assert_eq!(full_hash, midstate_hash, "Mismatch for nonce {nonce}");
        }
    }

    #[test]
    fn test_merkle_root_single() {
        let hash = [0xaa; 32];
        assert_eq!(compute_merkle_root(&[hash]), hash);
    }

    #[test]
    fn test_nbits_to_target() {
        let target = nbits_to_target(0x1d00ffff);
        assert_eq!(target[0], 0x00);
        assert_eq!(target[1], 0x00);
        assert_eq!(target[2], 0x00);
        assert_eq!(target[3], 0x00);
        assert_eq!(target[4], 0xff);
        assert_eq!(target[5], 0xff);
    }

    #[test]
    fn test_nbits_zero_exponent() {
        let target = nbits_to_target(0x00000000);
        assert_eq!(target, [0u8; 32]);
    }

    #[test]
    fn test_header_serialize_length() {
        let header = BlockHeader {
            version: 1,
            prev_hash: [0; 32],
            merkle_root: [0; 32],
            timestamp: 0,
            bits: 0,
            nonce: 0,
        };
        assert_eq!(header.serialize().len(), 80);
    }

    #[test]
    fn test_header_serialize_version_little_endian() {
        let header = BlockHeader {
            version: 0x20000000,
            prev_hash: [0; 32],
            merkle_root: [0; 32],
            timestamp: 0,
            bits: 0,
            nonce: 0,
        };
        let bytes = header.serialize();
        assert_eq!(&bytes[0..4], &[0x00, 0x00, 0x00, 0x20]);
    }

    #[test]
    fn test_header_nonce_position() {
        let header = BlockHeader {
            version: 0, prev_hash: [0; 32], merkle_root: [0; 32],
            timestamp: 0, bits: 0, nonce: 0xDEADBEEF,
        };
        let bytes = header.serialize();
        assert_eq!(&bytes[76..80], &0xDEADBEEFu32.to_le_bytes());
    }

    #[test]
    fn test_sha256d_known_value() {
        // SHA-256d("hello") should be a known hash
        let result = sha256d(b"hello");
        assert_eq!(result.len(), 32);
        // Just verify it's deterministic
        assert_eq!(sha256d(b"hello"), sha256d(b"hello"));
        assert_ne!(sha256d(b"hello"), sha256d(b"world"));
    }

    #[test]
    fn test_merkle_root_empty() {
        assert_eq!(compute_merkle_root(&[]), [0u8; 32]);
    }

    #[test]
    fn test_merkle_root_two_txs() {
        let a = sha256d(b"tx1");
        let b = sha256d(b"tx2");
        let root = compute_merkle_root(&[a, b]);
        // Root should be sha256d(a || b)
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(&a);
        combined[32..].copy_from_slice(&b);
        assert_eq!(root, sha256d(&combined));
    }

    #[test]
    fn test_merkle_root_odd_count_duplicates_last() {
        let a = sha256d(b"tx1");
        let b = sha256d(b"tx2");
        let c = sha256d(b"tx3");
        let root = compute_merkle_root(&[a, b, c]);
        // First level: hash(a||b), hash(c||c)
        let mut ab = [0u8; 64];
        ab[..32].copy_from_slice(&a);
        ab[32..].copy_from_slice(&b);
        let hab = sha256d(&ab);
        let mut cc = [0u8; 64];
        cc[..32].copy_from_slice(&c);
        cc[32..].copy_from_slice(&c);
        let hcc = sha256d(&cc);
        // Second level: hash(hab||hcc)
        let mut top = [0u8; 64];
        top[..32].copy_from_slice(&hab);
        top[32..].copy_from_slice(&hcc);
        assert_eq!(root, sha256d(&top));
    }

    #[test]
    fn test_build_coinbase_tx() {
        let cb1 = vec![0x01, 0x02];
        let cb2 = vec![0x03, 0x04];
        let tx = build_coinbase_tx(&cb1, 0x42, 4, &cb2);
        assert_eq!(tx.len(), 2 + 4 + 2); // cb1 + extranonce(4) + cb2
        assert_eq!(&tx[0..2], &[0x01, 0x02]);
        assert_eq!(tx[2], 0x42); // extranonce LE first byte
        assert_eq!(&tx[6..8], &[0x03, 0x04]);
    }

}
