#include <metal_stdlib>
using namespace metal;

// SHA-256 constants
constant uint K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
};

// SHA-256 helper functions
inline uint rotr(uint x, uint n) {
    return (x >> n) | (x << (32 - n));
}

inline uint ch(uint x, uint y, uint z) {
    return (x & y) ^ (~x & z);
}

inline uint maj(uint x, uint y, uint z) {
    return (x & y) ^ (x & z) ^ (y & z);
}

inline uint sigma0(uint x) {
    return rotr(x, 2) ^ rotr(x, 13) ^ rotr(x, 22);
}

inline uint sigma1(uint x) {
    return rotr(x, 6) ^ rotr(x, 11) ^ rotr(x, 25);
}

inline uint gamma0(uint x) {
    return rotr(x, 7) ^ rotr(x, 18) ^ (x >> 3);
}

inline uint gamma1(uint x) {
    return rotr(x, 17) ^ rotr(x, 19) ^ (x >> 10);
}

// SHA-256 compression on a 64-byte block, updating state in-place
void sha256_compress(thread uint *state, thread uint *w) {
    // Expand message schedule
    for (int i = 16; i < 64; i++) {
        w[i] = gamma1(w[i-2]) + w[i-7] + gamma0(w[i-15]) + w[i-16];
    }

    uint a = state[0];
    uint b = state[1];
    uint c = state[2];
    uint d = state[3];
    uint e = state[4];
    uint f = state[5];
    uint g = state[6];
    uint h = state[7];

    for (int i = 0; i < 64; i++) {
        uint t1 = h + sigma1(e) + ch(e, f, g) + K[i] + w[i];
        uint t2 = sigma0(a) + maj(a, b, c);
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }

    state[0] += a;
    state[1] += b;
    state[2] += c;
    state[3] += d;
    state[4] += e;
    state[5] += f;
    state[6] += g;
    state[7] += h;
}

// Input buffer layout:
// [0..7]   = midstate (8 x uint32, big-endian SHA-256 state after first 64 bytes)
// [8..11]  = tail_data (4 x uint32: bytes 64-79 of header, but nonce at [11] is overwritten)
// [12..19] = target (8 x uint32, big-endian 256-bit target)
// [20]     = nonce_start offset
//
// Output buffer:
// [0]      = found flag (0 = not found, 1 = found)
// [1]      = found nonce
// [2..9]   = found hash (8 x uint32)

kernel void sha256d_mine(
    device const uint *input [[buffer(0)]],
    device atomic_uint *output [[buffer(1)]],
    uint gid [[thread_position_in_grid]]
) {
    // Read midstate
    uint state[8];
    for (int i = 0; i < 8; i++) {
        state[i] = input[i];
    }

    // Compute nonce for this thread
    uint nonce_start = input[20];
    uint nonce = nonce_start + gid;

    // Build the second (final) 64-byte block:
    // Bytes 64-75 of header (tail_data minus nonce) + nonce + padding + length
    uint w[64];

    // Words 0-3: tail data (bytes 64-79 of the 80-byte header)
    w[0] = input[8];   // bytes 64-67 (timestamp portion that falls in second block)
    w[1] = input[9];   // bytes 68-71 (bits)
    w[2] = input[10];  // bytes 72-75
    w[3] = nonce;      // bytes 76-79 (nonce, little-endian as-is since midstate expects it)

    // Padding: 0x80 followed by zeros, then 64-bit big-endian bit count
    w[4] = 0x80000000; // padding start
    for (int i = 5; i < 15; i++) {
        w[i] = 0;
    }
    w[15] = 640;       // bit length of the 80-byte message (80 * 8 = 640)

    // First SHA-256: complete from midstate
    sha256_compress(state, w);

    // state now contains the first SHA-256 hash as 8 uint32 words (big-endian)
    // Second SHA-256: hash the 32-byte result
    uint state2[8] = {
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19
    };

    uint w2[64];
    // The 32-byte first hash as 8 big-endian words
    for (int i = 0; i < 8; i++) {
        w2[i] = state[i];
    }
    w2[8] = 0x80000000;  // padding
    for (int i = 9; i < 15; i++) {
        w2[i] = 0;
    }
    w2[15] = 256;  // 32 bytes * 8 = 256 bits

    sha256_compress(state2, w2);

    // Check against target (big-endian comparison, most significant word first)
    // state2 is the double-SHA-256 hash as big-endian uint32 words
    // target is also big-endian uint32 words
    bool below_target = false;
    bool equal_so_far = true;

    for (int i = 0; i < 8 && equal_so_far; i++) {
        uint h_word = state2[i];
        uint t_word = input[12 + i];

        if (h_word < t_word) {
            below_target = true;
            equal_so_far = false;
        } else if (h_word > t_word) {
            below_target = false;
            equal_so_far = false;
        }
    }

    if (below_target || equal_so_far) {
        // Found a valid nonce! Write to output using atomic to handle races
        uint expected = 0;
        if (atomic_compare_exchange_weak_explicit(
                &output[0], &expected, 1,
                memory_order_relaxed, memory_order_relaxed)) {
            // We won the race — write our result
            atomic_store_explicit(&output[1], nonce, memory_order_relaxed);
            for (int i = 0; i < 8; i++) {
                atomic_store_explicit(&output[2 + i], state2[i], memory_order_relaxed);
            }
        }
    }
}
