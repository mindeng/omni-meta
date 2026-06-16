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

/// 一个 EBML 元素头。`size` 为 None 表示未知大小（数据位全 1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElemHeader {
    pub id: u32,
    pub header_len: u64, // id 字节数 + size 字节数
    pub size: Option<u64>,
}

/// 读一个元素头（ID + size）。字节不足返回 None。
pub fn read_element_header(input: &[u8]) -> Option<ElemHeader> {
    let (id, idlen) = read_elem_id(input)?;
    let (size, szlen) = read_elem_size(input.get(idlen..)?)?;
    Some(ElemHeader { id, header_len: (idlen + szlen) as u64, size })
}

/// 在已见引导字节后，精确计算读出完整元素头所需字节数（供增量索取）。
pub fn needed_header_bytes(input: &[u8]) -> usize {
    let idlen = match input.first() {
        Some(&f) if f != 0 => ((f.leading_zeros() as usize) + 1).min(4),
        _ => return 2, // 首字节尚不可见：最小元素头 = 2
    };
    match input.get(idlen) {
        Some(&f) if f != 0 => idlen + (f.leading_zeros() as usize) + 1,
        _ => idlen + 1, // size 首字节尚不可见
    }
}

/// 大端读无符号整数（1–8 B；空 → 0）。
pub fn read_uint(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &x in b.iter().take(8) {
        v = (v << 8) | u64::from(x);
    }
    v
}

/// 大端读有符号整数（1–8 B，符号扩展；空 → 0）。
pub fn read_int(b: &[u8]) -> i64 {
    let n = b.len().min(8);
    if n == 0 {
        return 0;
    }
    let mut v = 0u64;
    for &x in &b[..n] {
        v = (v << 8) | u64::from(x);
    }
    let shift = 64 - 8 * n as u32;
    ((v << shift) as i64) >> shift
}

/// 大端读 IEEE 浮点（4 → f32、8 → f64、0 → 0.0、其它 → None）。
pub fn read_float(b: &[u8]) -> Option<f64> {
    match b.len() {
        0 => Some(0.0),
        4 => Some(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        8 => Some(f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])),
        _ => None,
    }
}

/// 遍历已缓冲定长载荷内的连续子元素。未知大小子元素 / 声明长度越界 → 停止
/// （不产出残缺项）。每项产出 (元素头, 子载荷切片)。
pub struct ChildElements<'a> {
    rest: &'a [u8],
}

/// 在一段载荷上构造子元素迭代器。
pub fn iter_child_elements(payload: &[u8]) -> ChildElements<'_> {
    ChildElements { rest: payload }
}

impl<'a> Iterator for ChildElements<'a> {
    type Item = (ElemHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let hdr = read_element_header(self.rest)?;
        let size = usize::try_from(hdr.size?).ok()?; // 未知大小 → 停止
        let header_len = usize::try_from(hdr.header_len).ok()?;
        let total = header_len.checked_add(size)?;
        if total > self.rest.len() {
            return None; // 越界 → 停止
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

    fn elem(id: &[u8], payload: &[u8]) -> alloc::vec::Vec<u8> {
        // 用 8 字节 vint size 编码，便于构造
        let mut e = alloc::vec::Vec::new();
        e.extend_from_slice(id);
        e.push(0x01);
        e.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]); // 低 7 字节
        e.extend_from_slice(payload);
        e
    }

    #[test]
    fn element_header_reads_id_size() {
        let e = elem(&[0xA3], &[1, 2, 3]); // 单字节 ID 0xA3
        let h = read_element_header(&e).unwrap();
        assert_eq!(h.id, 0xA3);
        assert_eq!(h.header_len, 9); // 1 id + 8 size
        assert_eq!(h.size, Some(3));
    }

    #[test]
    fn child_iter_walks_and_stops_on_overrun() {
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&elem(&[0xB0], &[0, 0, 5, 0])); // 子元素 A
        buf.extend_from_slice(&elem(&[0xBA], &[0, 0, 2, 0])); // 子元素 B
        let got: alloc::vec::Vec<(u32, usize)> =
            iter_child_elements(&buf).map(|(h, p)| (h.id, p.len())).collect();
        assert_eq!(got, alloc::vec![(0xB0u32, 4usize), (0xBA, 4)]);

        // 声明长度越界 → 停止，不产出残缺项
        let mut bad = alloc::vec::Vec::new();
        bad.extend_from_slice(&[0xB0, 0x01]);
        bad.extend_from_slice(&999u64.to_be_bytes()[1..]); // 声明 999 > 实际
        assert_eq!(iter_child_elements(&bad).count(), 0);
    }

    #[test]
    fn be_readers() {
        assert_eq!(read_uint(&[0x01, 0x00]), 256);
        assert_eq!(read_uint(&[]), 0);
        assert_eq!(read_int(&[0xFF]), -1);        // 符号扩展
        assert_eq!(read_int(&[0x00, 0x05]), 5);
        assert_eq!(read_int(&[]), 0);
        assert_eq!(read_float(&[]), Some(0.0));
        assert_eq!(read_float(&5000.0f64.to_be_bytes()), Some(5000.0));
        assert_eq!(read_float(&1.5f32.to_be_bytes()), Some(1.5));
        assert_eq!(read_float(&[1, 2, 3]), None); // 非法长度
    }

    #[test]
    fn needed_header_bytes_progresses() {
        // 仅 ID 首字节可见（4 字节 ID）→ 需 4 + size 首字节
        assert_eq!(needed_header_bytes(&[0x1A]), 5);
        // ID 全到 + size 首字节可见（8 字节 size）→ 需 4 + 8
        assert_eq!(needed_header_bytes(&[0x1A, 0x45, 0xDF, 0xA3, 0x01]), 12);
    }
}
