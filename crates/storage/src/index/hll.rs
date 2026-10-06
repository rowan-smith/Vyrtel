//! Tiny HyperLogLog (64 registers, ~13% standard error) used for per-field
//! distinct-count estimates in segment statistics. Cheap enough to keep for
//! every tracked field; precise enough to tell "5 values" from "50,000".

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;

const P: u32 = 6;
const M: usize = 1 << P;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hll {
    registers: [u8; M],
}

impl Default for Hll {
    fn default() -> Self {
        Self { registers: [0; M] }
    }
}

impl Hll {
    pub fn insert(&mut self, hash: u64) {
        let idx = (hash >> (64 - P)) as usize;
        let rest = hash << P;
        let rank = (rest.leading_zeros() + 1).min(64 - P + 1) as u8;
        if rank > self.registers[idx] {
            self.registers[idx] = rank;
        }
    }

    pub fn merge(&mut self, other: &Hll) {
        for (a, b) in self.registers.iter_mut().zip(other.registers.iter()) {
            *a = (*a).max(*b);
        }
    }

    pub fn estimate(&self) -> u64 {
        let m = M as f64;
        let alpha = 0.709; // alpha_64
        let sum: f64 = self.registers.iter().map(|r| 2f64.powi(-(*r as i32))).sum();
        let raw = alpha * m * m / sum;
        let zeros = self.registers.iter().filter(|r| **r == 0).count();
        if raw <= 2.5 * m && zeros > 0 {
            // Linear counting is much better for small cardinalities.
            (m * (m / zeros as f64).ln()).round() as u64
        } else {
            raw.round() as u64
        }
    }

    pub fn encode(&self, w: &mut Writer) {
        w.raw(&self.registers);
    }

    pub fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let b = r.take(M)?;
        let mut registers = [0u8; M];
        registers.copy_from_slice(b);
        Ok(Self { registers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xxhash_rust::xxh3::xxh3_64;

    fn estimate(n: u64) -> u64 {
        let mut h = Hll::default();
        for i in 0..n {
            h.insert(xxh3_64(&i.to_le_bytes()));
            // Duplicates must not inflate the estimate.
            h.insert(xxh3_64(&i.to_le_bytes()));
        }
        h.estimate()
    }

    #[test]
    fn small_cardinalities_are_close() {
        assert_eq!(estimate(0), 0);
        let e = estimate(5);
        assert!((4..=6).contains(&e), "{e}");
    }

    #[test]
    fn large_cardinalities_within_error() {
        for n in [1_000u64, 50_000] {
            let e = estimate(n) as f64;
            let err = (e - n as f64).abs() / n as f64;
            assert!(err < 0.4, "n={n} estimate={e}");
        }
    }

    #[test]
    fn merge_and_round_trip() {
        let mut a = Hll::default();
        let mut b = Hll::default();
        for i in 0..100u64 {
            a.insert(xxh3_64(&i.to_le_bytes()));
            b.insert(xxh3_64(&(i + 100).to_le_bytes()));
        }
        a.merge(&b);
        let mut w = Writer::new();
        a.encode(&mut w);
        let back = Hll::decode(&mut Reader::new(&w.buf)).unwrap();
        assert_eq!(back, a);
        let e = back.estimate();
        assert!((140..=260).contains(&e), "{e}");
    }
}
