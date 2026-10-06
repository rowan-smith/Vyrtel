//! Byte-level primitives: LEB128 varints, zigzag, length-prefixed strings.
//!
//! Every multi-byte fixed-width integer is little-endian. Decoding never
//! trusts lengths read from disk: it checks them against the remaining input
//! before allocating, so a corrupt length cannot trigger a huge allocation.

use crate::error::DecodeError;

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n) }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    pub fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }

    pub fn varint_i(&mut self, v: i64) {
        self.varint(zigzag(v));
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    /// Optional string: 0 = absent, otherwise len+1 followed by bytes.
    pub fn opt_str(&mut self, s: Option<&str>) {
        match s {
            None => self.varint(0),
            Some(s) => {
                self.varint(s.len() as u64 + 1);
                self.buf.extend_from_slice(s.as_bytes());
            }
        }
    }

    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.remaining() {
            return Err(DecodeError::UnexpectedEof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_bits(u64::from_le_bytes(self.array()?)))
    }

    pub fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return Err(DecodeError::VarintOverflow);
            }
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 63 {
                return Err(DecodeError::VarintOverflow);
            }
        }
    }

    pub fn varint_i(&mut self) -> Result<i64, DecodeError> {
        Ok(unzigzag(self.varint()?))
    }

    /// A varint used as a count/length; bounded by remaining bytes so
    /// corrupt input cannot request absurd allocations.
    pub fn len(&mut self) -> Result<usize, DecodeError> {
        let n = self.varint()?;
        if n > self.remaining() as u64 {
            return Err(DecodeError::LengthOutOfBounds);
        }
        Ok(n as usize)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.len()?;
        self.take(n)
    }

    pub fn str(&mut self) -> Result<&'a str, DecodeError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| DecodeError::InvalidUtf8)
    }

    pub fn string(&mut self) -> Result<String, DecodeError> {
        self.str().map(str::to_string)
    }

    pub fn opt_string(&mut self) -> Result<Option<String>, DecodeError> {
        let n = self.varint()?;
        if n == 0 {
            return Ok(None);
        }
        let n = n - 1;
        if n > self.remaining() as u64 {
            return Err(DecodeError::LengthOutOfBounds);
        }
        let b = self.take(n as usize)?;
        std::str::from_utf8(b).map(|s| Some(s.to_string())).map_err(|_| DecodeError::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn varint_edges() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut w = Writer::new();
            w.varint(v);
            assert_eq!(Reader::new(&w.buf).varint().unwrap(), v);
        }
    }

    #[test]
    fn varint_overflow_rejected() {
        let bad = [0xffu8; 11];
        assert_eq!(Reader::new(&bad).varint(), Err(DecodeError::VarintOverflow));
    }

    #[test]
    fn corrupt_length_rejected_without_allocating() {
        let mut w = Writer::new();
        w.varint(u64::MAX / 2);
        assert_eq!(Reader::new(&w.buf).bytes(), Err(DecodeError::LengthOutOfBounds));
    }

    #[test]
    fn eof_detected() {
        assert_eq!(Reader::new(&[1, 2]).u32(), Err(DecodeError::UnexpectedEof));
        assert_eq!(Reader::new(&[0x80]).varint(), Err(DecodeError::UnexpectedEof));
    }

    proptest! {
        #[test]
        fn zigzag_round_trip(v in any::<i64>()) {
            prop_assert_eq!(unzigzag(zigzag(v)), v);
        }

        #[test]
        fn mixed_round_trip(a in any::<u64>(), b in any::<i64>(), s in ".*", o in proptest::option::of(".*"), f in any::<f64>()) {
            let mut w = Writer::new();
            w.varint(a);
            w.varint_i(b);
            w.str(&s);
            w.opt_str(o.as_deref());
            w.f64(f);
            let mut r = Reader::new(&w.buf);
            prop_assert_eq!(r.varint().unwrap(), a);
            prop_assert_eq!(r.varint_i().unwrap(), b);
            prop_assert_eq!(r.str().unwrap(), s.as_str());
            prop_assert_eq!(r.opt_string().unwrap(), o);
            prop_assert_eq!(r.f64().unwrap().to_bits(), f.to_bits());
            prop_assert!(r.is_empty());
        }
    }
}
