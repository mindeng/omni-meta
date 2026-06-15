//! ISO-BMFF (ISO/IEC 14496-12) box 头部读取。共享给 HEIF/AVIF/MP4/MOV。
//! 全程大端。边界安全：字节不足返回 None，绝不 panic、不分配、不前进。

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
    pub fn payload_len(&self) -> Option<u64> {
        self.total_size.map(|t| t.saturating_sub(self.header_len))
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
}
