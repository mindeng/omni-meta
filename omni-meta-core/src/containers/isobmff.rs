//! ISO-BMFF (ISO/IEC 14496-12) box 头部读取。共享给 HEIF/AVIF/MP4/MOV。
//! 全程大端。边界安全：字节不足返回 None，绝不 panic、不分配、不前进。

use crate::cursor::{ByteCursor, Endian};

/// 一个 box 的头部信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxHeader {
    /// 四字节 box 类型（如 `b"ftyp"`）。
    pub kind: [u8; 4],
    /// 头部自身字节数：8（32 位 size）或 16（size==1 的 64 位 largesize）。
    pub header_len: u64,
    /// box 总字节数（含头部）。size==0（延伸至文件尾）时为 None。
    pub total_size: Option<u64>,
}

impl BoxHeader {
    /// 载荷字节数 = total_size − header_len；size==0 时未知，返回 None。
    /// A2/A3 box 链续走时使用；当前 A1 仅测试覆盖，故抑制未使用警告。
    #[allow(dead_code)]
    pub fn payload_len(&self) -> Option<u64> {
        self.total_size.and_then(|t| t.checked_sub(self.header_len))
    }
}

/// 从 `input` 起点读一个 box 头。字节不足以读出完整头部时返回 None。
pub fn read_box_header(input: &[u8]) -> Option<BoxHeader> {
    if input.len() < 8 {
        return None;
    }
    let size32 = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
    let mut kind = [0u8; 4];
    kind.copy_from_slice(&input[4..8]);
    match size32 {
        1 => {
            if input.len() < 16 {
                return None;
            }
            let large = u64::from_be_bytes([
                input[8], input[9], input[10], input[11],
                input[12], input[13], input[14], input[15],
            ]);
            Some(BoxHeader { kind, header_len: 16, total_size: Some(large) })
        }
        0 => Some(BoxHeader { kind, header_len: 8, total_size: None }),
        n => Some(BoxHeader { kind, header_len: 8, total_size: Some(n as u64) }),
    }
}

/// 读 FullBox 的 version(1) + flags(3)。`payload` 为 box 头之后的字节。
/// 不足 4 字节返回 None。
pub fn full_box_vf(payload: &[u8]) -> Option<(u8, u32)> {
    if payload.len() < 4 {
        return None;
    }
    Some((payload[0], u32::from_be_bytes([0, payload[1], payload[2], payload[3]])))
}

/// 从游标读大端无符号整数，size ∈ {0,4,8}（ISO-BMFF 可变位宽字段）。
/// size==0 → Some(0) 且不消费；其它非法位宽或越界 → None（越界时游标不前进）。
pub fn read_uint_be(cur: &mut ByteCursor, size: u8) -> Option<u64> {
    match size {
        0 => Some(0),
        4 => cur.u32(Endian::Big).map(u64::from),
        8 => {
            let s = cur.take(8)?;
            Some(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        }
        _ => None,
    }
}

/// 遍历 `payload` 内连续子 box。字节不足 / size0 / 声明长度小于头部或越界 → 停止
/// （不产出残缺项）。每项产出 (头, 该 box 载荷切片)。
pub struct ChildBoxes<'a> {
    rest: &'a [u8],
}

/// 在一段载荷上构造子盒迭代器。
pub fn iter_child_boxes(payload: &[u8]) -> ChildBoxes<'_> {
    ChildBoxes { rest: payload }
}

impl<'a> Iterator for ChildBoxes<'a> {
    type Item = (BoxHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let hdr = read_box_header(self.rest)?;
        let total = usize::try_from(hdr.total_size?).ok()?;
        let header_len = usize::try_from(hdr.header_len).ok()?;
        if total < header_len || total > self.rest.len() {
            return None;
        }
        let payload = &self.rest[header_len..total];
        self.rest = &self.rest[total..];
        Some((hdr, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_32bit_size_box() {
        // size=16, type="ftyp"
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&16u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(&[0u8; 8]); // 载荷
        let h = read_box_header(&b).unwrap();
        assert_eq!(&h.kind, b"ftyp");
        assert_eq!(h.header_len, 8);
        assert_eq!(h.total_size, Some(16));
        assert_eq!(h.payload_len(), Some(8));
    }

    #[test]
    fn reads_64bit_largesize_box() {
        // size=1 → 紧跟 8 字节 largesize
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&4_000_000_000u64.to_be_bytes());
        let h = read_box_header(&b).unwrap();
        assert_eq!(&h.kind, b"mdat");
        assert_eq!(h.header_len, 16);
        assert_eq!(h.total_size, Some(4_000_000_000));
        assert_eq!(h.payload_len(), Some(4_000_000_000 - 16));
    }

    #[test]
    fn malformed_size_smaller_than_header_returns_none() {
        // size32==4 < header_len 8：畸形 box，payload_len 应返回 None。
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        let h = read_box_header(&b).unwrap();
        assert_eq!(h.total_size, Some(4));
        assert_eq!(h.payload_len(), None);
    }

    #[test]
    fn size_zero_means_to_eof() {
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        let h = read_box_header(&b).unwrap();
        assert_eq!(h.total_size, None);
        assert_eq!(h.payload_len(), None);
    }

    #[test]
    fn too_short_returns_none() {
        assert!(read_box_header(&[0, 0, 0]).is_none());
        // 声明 largesize 但缺字节
        let mut b = alloc::vec::Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&[0u8; 3]); // 只 3 字节，不足 8
        assert!(read_box_header(&b).is_none());
    }

    #[test]
    fn full_box_vf_reads_version_flags() {
        assert_eq!(full_box_vf(&[2, 0, 0, 5]), Some((2, 5)));
        assert_eq!(full_box_vf(&[0, 0, 0]), None); // 不足 4 字节
    }

    #[test]
    fn read_uint_be_widths() {
        let buf = [0x00, 0x00, 0x00, 0x09, 0xAA];
        let mut c = crate::cursor::ByteCursor::new(&buf);
        assert_eq!(read_uint_be(&mut c, 0), Some(0)); // 不消费
        assert_eq!(read_uint_be(&mut c, 4), Some(9)); // 消费 4
        assert_eq!(read_uint_be(&mut c, 3), None);    // 非法位宽
        let big = [0, 0, 0, 0, 0, 0, 0, 7u8];
        let mut c2 = crate::cursor::ByteCursor::new(&big);
        assert_eq!(read_uint_be(&mut c2, 8), Some(7));
    }

    #[test]
    fn iter_child_boxes_walks_siblings_and_stops_on_overrun() {
        // 两个子盒：free(8) + ftyp(载荷 4)
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(b"free");
        buf.extend_from_slice(&12u32.to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(&[1, 2, 3, 4]);
        let got: alloc::vec::Vec<([u8; 4], usize)> =
            iter_child_boxes(&buf).map(|(h, p)| (h.kind, p.len())).collect();
        assert_eq!(got, alloc::vec![(*b"free", 0usize), (*b"ftyp", 4usize)]);

        // 声明长度越界 → 停止（不产出残缺项）
        let mut bad = alloc::vec::Vec::new();
        bad.extend_from_slice(&99u32.to_be_bytes()); // 声明 99 > 实际
        bad.extend_from_slice(b"mdat");
        assert_eq!(iter_child_boxes(&bad).count(), 0);
    }
}
