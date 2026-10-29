//! Variable-length integer encoding and a bounds-checked byte cursor, as used by the mapsforge format.

use anyhow::{Context, Result, bail};

/// Appends a variable-byte encoded unsigned integer.
pub fn put_uvar(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// Appends a variable-byte encoded signed integer (sign in bit 6 of the last byte).
pub fn put_svar(buf: &mut Vec<u8>, value: i64) {
    let negative = value < 0;
    let mut magnitude = value.unsigned_abs();
    while magnitude >= 0x40 {
        buf.push((magnitude & 0x7f) as u8 | 0x80);
        magnitude >>= 7;
    }
    buf.push(magnitude as u8 | if negative { 0x40 } else { 0 });
}

/// Appends a length-prefixed UTF-8 string.
pub fn put_string(buf: &mut Vec<u8>, value: &str) {
    put_uvar(buf, value.len() as u64);
    buf.extend_from_slice(value.as_bytes());
}

/// A forward-only reader over a byte slice that reports truncation as errors.
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Creates a cursor positioned at `pos`.
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    /// Current absolute position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Moves to an absolute position.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Consumes `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("length overflow")?;
        if end > self.data.len() {
            bail!("unexpected end of data at offset {}", self.pos);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into()?))
    }

    /// Reads a variable-byte unsigned integer.
    pub fn uvar(&mut self) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift > 63 {
                bail!("varint too long");
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Reads a variable-byte signed integer.
    pub fn svar(&mut self) -> Result<i64> {
        let mut value = 0i64;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift > 56 {
                bail!("varint too long");
            }
            if byte & 0x80 != 0 {
                value |= i64::from(byte & 0x7f) << shift;
                shift += 7;
            } else {
                value |= i64::from(byte & 0x3f) << shift;
                return Ok(if byte & 0x40 != 0 { -value } else { value });
            }
        }
    }

    /// Reads a length-prefixed UTF-8 string.
    pub fn string(&mut self) -> Result<String> {
        let len = self.uvar()? as usize;
        let bytes = self.take(len)?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_round_trip() {
        for v in [0u64, 1, 127, 128, 300, 16_384, u32::MAX as u64, 1 << 62] {
            let mut buf = Vec::new();
            put_uvar(&mut buf, v);
            assert_eq!(Cursor::new(&buf, 0).uvar().unwrap(), v);
        }
    }

    #[test]
    fn signed_round_trip() {
        for v in [0i64, 1, -1, 63, 64, -64, -65, 1_000_000, -1_000_000, i32::MIN as i64] {
            let mut buf = Vec::new();
            put_svar(&mut buf, v);
            assert_eq!(Cursor::new(&buf, 0).svar().unwrap(), v);
        }
    }

    #[test]
    fn known_encodings() {
        let mut buf = Vec::new();
        put_svar(&mut buf, -5);
        assert_eq!(buf, [0x45]);
        buf.clear();
        put_uvar(&mut buf, 300);
        assert_eq!(buf, [0xac, 0x02]);
    }

    #[test]
    fn fixed_width_and_strings() {
        let mut buf = vec![0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3];
        put_string(&mut buf, "héllo");
        let mut c = Cursor::new(&buf, 0);
        assert_eq!(c.u16().unwrap(), 1);
        assert_eq!(c.i32().unwrap(), 2);
        assert_eq!(c.i64().unwrap(), 3);
        assert_eq!(c.string().unwrap(), "héllo");
        assert_eq!(c.pos(), buf.len());
        c.seek(0);
        assert_eq!(c.u8().unwrap(), 0);
    }

    #[test]
    fn truncation_and_invalid_data_are_errors() {
        assert!(Cursor::new(&[], 0).u8().is_err());
        assert!(Cursor::new(&[0x80], 0).uvar().is_err());
        assert!(Cursor::new(&[0x80], 0).svar().is_err());
        assert!(Cursor::new(&[0xff; 12], 0).uvar().is_err());
        assert!(Cursor::new(&[0xff; 12], 0).svar().is_err());
        assert!(Cursor::new(&[1, 0xff], 0).string().is_err());
        assert!(Cursor::new(&[5, b'a'], 0).string().is_err());
        assert!(Cursor::new(&[1], usize::MAX).take(2).is_err());
    }
}
