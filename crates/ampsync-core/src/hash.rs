//! QuickXorHash (the hash OneDrive / SharePoint report as `file.hashes.quickXorHash`).
//!
//! v1 cannot obtain it with cookie auth (D18) but the engine verifies with it whenever a
//! `Remote` supplies one.
//!
//! References:
//! - Microsoft docs, "QuickXorHash Algorithm" (C# reference implementation).
//! - rclone Go implementation and test vectors (MIT).
//!
//! Algorithm: a 160-bit circular register. Byte number `n` (0-based, over the whole stream) is
//! XORed into the register starting at bit `(n * 11) mod 160`, wrapping around at bit 160.
//! Finalisation XORs the stream length (u64 little endian) into the last 8 bytes of the register.
//! The result is the 20 register bytes, usually base64 encoded.

use base64::Engine;

const WIDTH_BYTES: usize = 20; // 160 bits
const WIDTH_BITS: u64 = 160;
const SHIFT: u64 = 11;

/// Streaming QuickXorHash hasher.
#[derive(Debug, Clone)]
pub struct QuickXorHash {
    reg: [u8; WIDTH_BYTES],
    len: u64,
}

impl Default for QuickXorHash {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickXorHash {
    pub fn new() -> Self {
        Self {
            reg: [0; WIDTH_BYTES],
            len: 0,
        }
    }

    /// Feed more bytes of the stream.
    pub fn update(&mut self, data: &[u8]) {
        // Bit position of the next byte, always < 160.
        let mut pos = (self.len.wrapping_mul(SHIFT) % WIDTH_BITS) as usize;
        for &b in data {
            let byte_idx = pos / 8;
            let bit_off = (pos % 8) as u32;
            let v = (b as u16) << bit_off; // spans at most two register bytes
            self.reg[byte_idx] ^= (v & 0xff) as u8;
            self.reg[(byte_idx + 1) % WIDTH_BYTES] ^= (v >> 8) as u8;
            pos += SHIFT as usize;
            if pos >= WIDTH_BITS as usize {
                pos -= WIDTH_BITS as usize;
            }
        }
        self.len = self.len.wrapping_add(data.len() as u64);
    }

    /// The 20-byte digest of everything fed so far. The hasher can keep being updated afterwards.
    pub fn finalize(&self) -> [u8; WIDTH_BYTES] {
        let mut out = self.reg;
        for (i, lb) in self.len.to_le_bytes().iter().enumerate() {
            out[WIDTH_BYTES - 8 + i] ^= lb;
        }
        out
    }

    /// Standard base64 (with padding) of [`finalize`](Self::finalize): the form SharePoint reports.
    pub fn finalize_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.finalize())
    }
}

/// One-shot helper: base64 QuickXorHash of `data`.
pub fn quickxor_base64(data: &[u8]) -> String {
    let mut h = QuickXorHash::new();
    h.update(data);
    h.finalize_base64()
}

#[cfg(test)]
#[path = "hash_vectors.rs"]
mod vectors;

#[cfg(test)]
mod tests {
    use super::vectors::VECTORS;
    use super::*;

    fn b64d(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn empty_input() {
        assert_eq!(quickxor_base64(b""), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }

    #[test]
    fn rclone_vectors_one_shot() {
        for (size, input, want) in VECTORS {
            let data = b64d(input);
            assert_eq!(data.len(), *size);
            assert_eq!(&quickxor_base64(&data), want, "size {size}");
        }
    }

    #[test]
    fn rclone_vectors_chunked() {
        for chunk in [1usize, 2, 7, 8, 64, 100] {
            for (size, input, want) in VECTORS {
                let data = b64d(input);
                let mut h = QuickXorHash::new();
                for c in data.chunks(chunk) {
                    h.update(c);
                }
                assert_eq!(&h.finalize_base64(), want, "size {size} chunk {chunk}");
            }
        }
    }

    #[test]
    fn single_byte_hand_computed() {
        // byte 0x4a at bit 0 -> reg[0] = 0x4a; length 1 xored at reg[12].
        let raw = b64d(&quickxor_base64(&[0x4a]));
        assert_eq!(raw[0], 0x4a);
        assert_eq!(raw[12], 1);
    }
}
