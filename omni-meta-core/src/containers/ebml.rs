//! EBML（Matroska/WebM）结构层：变长整数（vint）与元素遍历。
//! 全程大端。边界安全：字节不足返回 None，绝不 panic、不分配、不前进。

/// 读 EBML 元素 ID（**保留**标记位，ID 即规范值）。长度 1–4。
/// 首字节为 0（长度 >8）或长度 >4 → None。
pub fn read_elem_id(input: &[u8]) -> Option<(u32, usize)> {
    let first = *input.first()?;
    if first == 0 {
        return None; // 长度 > 8，非法
    }
    let len = first.leading_zeros() as usize + 1; // 1..=8
    if len > 4 {
        return None; // ID 长度上限 4（EBMLMaxIDLength 默认）
    }
    let bytes = input.get(..len)?;
    let mut id = 0u32;
    for &b in bytes {
        id = (id << 8) | u32::from(b);
    }
    Some((id, len))
}

/// 读 EBML 元素 size（**剥去**标记位取值）。长度 1–8。
/// 数据位全 1 → 未知大小（返回 `(None, len)`）。截断/长度 >8 → None。
pub fn read_elem_size(input: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *input.first()?;
    if first == 0 {
        return None; // 长度 > 8
    }
    let len = first.leading_zeros() as usize + 1; // 1..=8
    let bytes = input.get(..len)?;
    let mask = if len == 8 { 0u8 } else { 0xFFu8 >> len }; // 首字节数据位
    let mut val = u64::from(first & mask);
    for &b in &bytes[1..] {
        val = (val << 8) | u64::from(b);
    }
    let data_bits = 7 * len;
    let all_ones = if data_bits >= 64 {
        val == u64::MAX
    } else {
        val == (1u64 << data_bits) - 1
    };
    Some((if all_ones { None } else { Some(val) }, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elem_id_widths() {
        assert_eq!(read_elem_id(&[0xB0]), Some((0xB0, 1)));            // PixelWidth
        assert_eq!(read_elem_id(&[0x42, 0x82]), Some((0x4282, 2)));   // DocType
        assert_eq!(read_elem_id(&[0x2A, 0xD7, 0xB1]), Some((0x2AD7B1, 3))); // TimestampScale
        assert_eq!(read_elem_id(&[0x1A, 0x45, 0xDF, 0xA3]), Some((0x1A45DFA3, 4))); // EBML
        assert_eq!(read_elem_id(&[0x00]), None); // 长度 > 8 非法
        assert_eq!(read_elem_id(&[0x08, 0, 0, 0, 0]), None); // 长度 5 > 4 上限
        assert_eq!(read_elem_id(&[]), None);
    }

    #[test]
    fn elem_size_known_unknown_truncated() {
        // 单字节 size：0x81 → 值 1
        assert_eq!(read_elem_size(&[0x81]), Some((Some(1), 1)));
        // 双字节 size：0x40 0x05 → 值 5
        assert_eq!(read_elem_size(&[0x40, 0x05]), Some((Some(5), 2)));
        // 单字节未知大小：0xFF（数据位全 1）→ None size
        assert_eq!(read_elem_size(&[0xFF]), Some((None, 1)));
        // 八字节未知大小：0x01 + 7×0xFF → None size
        assert_eq!(read_elem_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), Some((None, 8)));
        // 八字节定长：0x01 + 7 字节 = 256
        assert_eq!(read_elem_size(&[0x01, 0, 0, 0, 0, 0, 1, 0]), Some((Some(256), 8)));
        // 截断：声明 2 字节但只给 1
        assert_eq!(read_elem_size(&[0x40]), None);
        assert_eq!(read_elem_size(&[0x00]), None); // 长度 > 8
        assert_eq!(read_elem_size(&[]), None);
    }
}
