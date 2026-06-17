//! IEEE CRC32（PNG chunk 合成用）。零依赖，逐字节查表。

/// 计算 IEEE CRC32（PNG 用：多项式 0xEDB88320，初值/末值全反转）。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_of_iend_chunk_type_known_vector() {
        // PNG IEND chunk（类型 + 空数据）的 CRC32 是众所周知的 0xAE426082。
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn crc32_of_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }
}
