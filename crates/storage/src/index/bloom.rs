//! Bloom filter used for high-cardinality equality lookups (trace IDs,
//! request IDs, customer IDs...). False positives cost a block decode; false
//! negatives would lose data, so every insert/probe goes through the same
//! canonical key hashing in [`super::keys`].

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bloom {
    words: Vec<u64>,
    k: u8,
}

impl Bloom {
    /// Size for `n` keys at `bits_per_key` (10 bits ≈ 1% false positives).
    pub fn with_capacity(n: usize, bits_per_key: usize) -> Self {
        let bits = (n.max(1) * bits_per_key.max(1)).next_multiple_of(64).max(64);
        let k = ((bits_per_key as f64) * std::f64::consts::LN_2).round() as u8;
        Self { words: vec![0; bits / 64], k: k.clamp(1, 16) }
    }

    pub fn num_bits(&self) -> u64 {
        self.words.len() as u64 * 64
    }

    fn positions(&self, hash: u64) -> impl Iterator<Item = u64> + '_ {
        // Kirsch–Mitzenmacher double hashing from one 64-bit hash.
        let h1 = hash & 0xffff_ffff;
        let h2 = (hash >> 32) | 1;
        let m = self.num_bits();
        (0..self.k as u64).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % m)
    }

    pub fn insert(&mut self, hash: u64) {
        let positions: Vec<u64> = self.positions(hash).collect();
        for p in positions {
            self.words[(p / 64) as usize] |= 1 << (p % 64);
        }
    }

    pub fn may_contain(&self, hash: u64) -> bool {
        self.positions(hash).all(|p| self.words[(p / 64) as usize] & (1 << (p % 64)) != 0)
    }

    pub fn byte_size(&self) -> usize {
        self.words.len() * 8 + 1
    }

    pub fn encode(&self, w: &mut Writer) {
        w.u8(self.k);
        w.varint(self.words.len() as u64);
        for word in &self.words {
            w.u64(*word);
        }
    }

    pub fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let k = r.u8()?;
        if k == 0 || k > 16 {
            return Err(DecodeError::Invalid("bloom hash count"));
        }
        let n = r.varint()? as usize;
        if n == 0 || n > r.remaining() / 8 {
            return Err(DecodeError::LengthOutOfBounds);
        }
        let mut words = Vec::with_capacity(n);
        for _ in 0..n {
            words.push(r.u64()?);
        }
        Ok(Self { words, k })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xxhash_rust::xxh3::xxh3_64;

    #[test]
    fn no_false_negatives() {
        let mut b = Bloom::with_capacity(1000, 10);
        for i in 0..1000u32 {
            b.insert(xxh3_64(&i.to_le_bytes()));
        }
        for i in 0..1000u32 {
            assert!(b.may_contain(xxh3_64(&i.to_le_bytes())));
        }
    }

    #[test]
    fn false_positive_rate_is_reasonable() {
        let mut b = Bloom::with_capacity(1000, 10);
        for i in 0..1000u32 {
            b.insert(xxh3_64(&i.to_le_bytes()));
        }
        let fp = (1000..101_000u32).filter(|i| b.may_contain(xxh3_64(&i.to_le_bytes()))).count();
        // ~1% expected; allow generous slack.
        assert!(fp < 3_000, "false positives: {fp}");
    }

    #[test]
    fn encode_round_trip() {
        let mut b = Bloom::with_capacity(10, 10);
        b.insert(42);
        let mut w = Writer::new();
        b.encode(&mut w);
        let back = Bloom::decode(&mut Reader::new(&w.buf)).unwrap();
        assert_eq!(back, b);
        assert!(back.may_contain(42));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Bloom::decode(&mut Reader::new(&[0, 1])).is_err());
        assert!(Bloom::decode(&mut Reader::new(&[7, 200, 1])).is_err());
    }
}
