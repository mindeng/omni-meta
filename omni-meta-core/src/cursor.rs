//! 边界安全的字节游标。所有读取在越界时返回 `None`，绝不 panic。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

pub struct ByteCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    // position/remaining/skip 是游标基础工具：当前由测试覆盖，
    // 将被后续格式解析器（PNG/BMFF 等）使用，故保留并显式允许暂未使用。
    #[allow(dead_code)]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[allow(dead_code)]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// 取 n 字节并前进；越界返回 None 且不改变位置。
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }

    // u8/u16_be：供未来格式解析器使用，当前仅测试覆盖，显式允许暂未使用。
    #[allow(dead_code)]
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    #[allow(dead_code)]
    pub fn u16_be(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }

    pub fn u16(&mut self, e: Endian) -> Option<u16> {
        let s = self.take(2)?;
        Some(match e {
            Endian::Big => u16::from_be_bytes([s[0], s[1]]),
            Endian::Little => u16::from_le_bytes([s[0], s[1]]),
        })
    }

    pub fn u32(&mut self, e: Endian) -> Option<u32> {
        let s = self.take(4)?;
        Some(match e {
            Endian::Big => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            Endian::Little => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        })
    }

    #[allow(dead_code)]
    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    pub fn seek(&mut self, pos: usize) -> Option<()> {
        if pos > self.buf.len() {
            return None;
        }
        self.pos = pos;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_primitives_and_respects_bounds() {
        let buf = [0x12u8, 0x34, 0x56, 0x78, 0xAB];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.u8(), Some(0x12));
        assert_eq!(c.u16_be(), Some(0x3456));
        assert_eq!(c.position(), 3);
        assert_eq!(c.u16(Endian::Little), Some(0xAB78));
        assert_eq!(c.u8(), None);
    }

    #[test]
    fn take_past_end_returns_none_without_advancing() {
        let buf = [1u8, 2, 3];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.take(4), None);
        assert_eq!(c.position(), 0);
        assert_eq!(c.take(2), Some(&buf[0..2]));
    }

    #[test]
    fn seek_and_skip() {
        let buf = [0u8; 10];
        let mut c = ByteCursor::new(&buf);
        assert_eq!(c.skip(4), Some(()));
        assert_eq!(c.position(), 4);
        assert_eq!(c.seek(9), Some(()));
        assert_eq!(c.seek(11), None);
        assert_eq!(c.position(), 9);
    }
}
