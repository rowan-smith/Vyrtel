//! Plain bitmap over block numbers. Used for low-cardinality bitmap indexes:
//! for each level / service / environment value, which blocks contain it.

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bitmap {
    words: Vec<u64>,
}

impl Bitmap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_len(bits: usize) -> Self {
        Self { words: vec![0; bits.div_ceil(64)] }
    }

    /// A bitmap with bits `0..bits` set.
    pub fn full(bits: usize) -> Self {
        let mut b = Self::with_len(bits);
        for i in 0..bits {
            b.set(i);
        }
        b
    }

    pub fn set(&mut self, i: usize) {
        let w = i / 64;
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        self.words[w] |= 1 << (i % 64);
    }

    pub fn get(&self, i: usize) -> bool {
        self.words.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn or_with(&mut self, other: &Bitmap) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= b;
        }
    }

    pub fn and_with(&mut self, other: &Bitmap) {
        for (i, a) in self.words.iter_mut().enumerate() {
            *a &= other.words.get(i).copied().unwrap_or(0);
        }
    }

    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, w)| {
            let w = *w;
            (0..64).filter(move |b| w & (1 << b) != 0).map(move |b| wi * 64 + b)
        })
    }

    pub fn byte_size(&self) -> usize {
        self.words.len() * 8
    }

    pub fn encode(&self, w: &mut Writer) {
        // Trim trailing zero words to keep the encoding canonical.
        let n = self.words.iter().rposition(|w| *w != 0).map_or(0, |i| i + 1);
        w.varint(n as u64);
        for word in &self.words[..n] {
            w.u64(*word);
        }
    }

    pub fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let n = r.varint()? as usize;
        if n > r.remaining() / 8 {
            return Err(DecodeError::LengthOutOfBounds);
        }
        let mut words = Vec::with_capacity(n);
        for _ in 0..n {
            words.push(r.u64()?);
        }
        Ok(Self { words })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_iter() {
        let mut b = Bitmap::new();
        for i in [0, 3, 64, 130] {
            b.set(i);
        }
        assert!(b.get(3) && b.get(130) && !b.get(4) && !b.get(10_000));
        assert_eq!(b.iter().collect::<Vec<_>>(), vec![0, 3, 64, 130]);
        assert_eq!(b.count(), 4);
    }

    #[test]
    fn boolean_ops() {
        let mut a = Bitmap::new();
        a.set(1);
        a.set(70);
        let mut b = Bitmap::new();
        b.set(70);
        b.set(200);
        let mut or = a.clone();
        or.or_with(&b);
        assert_eq!(or.iter().collect::<Vec<_>>(), vec![1, 70, 200]);
        let mut and = a.clone();
        and.and_with(&b);
        assert_eq!(and.iter().collect::<Vec<_>>(), vec![70]);
    }

    #[test]
    fn encode_round_trip() {
        let mut b = Bitmap::with_len(500);
        b.set(5);
        b.set(65);
        let mut w = Writer::new();
        b.encode(&mut w);
        let back = Bitmap::decode(&mut Reader::new(&w.buf)).unwrap();
        assert_eq!(back.iter().collect::<Vec<_>>(), vec![5, 65]);
        assert!(Bitmap::full(3).get(2) && !Bitmap::full(3).get(3));
    }
}
