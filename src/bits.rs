//! MSB-first bit reader over an in-memory buffer.

pub struct Bits {
    pub data: Vec<u8>,
    pub pos: usize, // bit position
}

impl Bits {
    pub fn new(data: Vec<u8>) -> Self {
        Bits { data, pos: 0 }
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn bytepos(&self) -> usize {
        self.pos >> 3
    }
    pub fn rewind(&mut self) {
        self.pos = 0;
    }
    pub fn remain(&self) -> i64 {
        (self.data.len() * 8) as i64 - self.pos as i64
    }
    fn bit(&mut self) -> u8 {
        let b = self.data.get(self.pos >> 3).map_or(0, |&x| (x >> (7 - (self.pos & 7))) & 1);
        self.pos += 1;
        b
    }
    pub fn read(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | self.bit() as u64;
        }
        v
    }
    pub fn bool(&mut self) -> bool {
        self.bit() == 1
    }
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }
    pub fn byte(&mut self) -> u8 {
        if self.pos & 7 == 0 {
            let b = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            self.pos += 8;
            b
        } else {
            self.read(8) as u8
        }
    }
    /// Refuses when the request reaches the end of the buffer.
    pub fn bytes(&mut self, n: usize) -> Option<Vec<u8>> {
        let p = self.bytepos();
        if p + n >= self.data.len() {
            return None;
        }
        let v = self.data[p..p + n].to_vec();
        self.pos += n * 8;
        Some(v)
    }
    pub fn seek_bytes(&mut self, delta: isize) {
        self.pos = (self.pos as isize + delta * 8).max(0) as usize;
    }
    /// Unsigned Exp-Golomb.
    pub fn ue(&mut self) -> u32 {
        let mut z = 0u32;
        while self.remain() > 0 && !self.bool() {
            z += 1;
            if z > 31 {
                return u32::MAX;
            }
        }
        (((1u64 << z) - 1) + self.read(z as usize)) as u32
    }
    /// Skips an unsigned or signed Exp-Golomb code.
    pub fn skip_ue(&mut self) {
        self.ue();
    }
}

/// Removes H.26x emulation-prevention bytes (00 00 03 -> 00 00).
pub fn unescape(d: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(d.len());
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        o.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exp_golomb() {
        // 1 | 010 | 011 | 00100 -> 0,1,2,3
        let mut b = Bits::new(vec![0b1010_0110, 0b0100_0000]);
        assert_eq!((b.ue(), b.ue(), b.ue(), b.ue()), (0, 1, 2, 3));
    }
    #[test]
    fn unescape_works() {
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3]), vec![0, 0, 1, 0, 0]);
    }
}
