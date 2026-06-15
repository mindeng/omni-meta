//! GIF block 遍历：header(6)+LSD(7) 后逐 block 推进。
//! LSD 发维度；图像/普通扩展走 sub-block 跳过；Application Extension "XMP DataXMP"
//! 捕获 XMP 包（裸字节直到魔数尾的 0x00 终止）；Trailer 0x3B 发 Done。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::Field;

#[derive(Debug, PartialEq, Eq)]
enum State {
    Header,
    Block,        // 期待引导字节
    SubBlocks,    // 跳过模式：走 sub-block 链至 0x00
    CapturingXmp, // 累积 XMP 包：窗口从包头部开始，找 0x00 终止符
}

#[derive(Debug)]
pub struct GifParser {
    state: State,
    done: bool,
}

impl Default for GifParser {
    fn default() -> Self {
        Self { state: State::Header, done: false }
    }
}

impl GifParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for GifParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        let mut pos = 0usize;

        if self.state == State::Header {
            if input.len() < 13 {
                return PullResult { demand: Demand::NeedBytes(13), consumed: 0, events };
            }
            let sig = &input[0..6];
            if sig != b"GIF87a" && sig != b"GIF89a" {
                self.done = true;
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            let w = u16::from_le_bytes([input[6], input[7]]) as u32;
            let h = u16::from_le_bytes([input[8], input[9]]) as u32;
            events.push(Event::Field(Field::Width(w)));
            events.push(Event::Field(Field::Height(h)));
            let packed = input[10];
            self.state = State::Block;
            pos = 13;
            if packed & 0x80 != 0 {
                // 跳过 Global Color Table：3 * 2^((packed&7)+1)
                let gct = 3usize * (1usize << ((packed & 0x07) + 1));
                return PullResult { demand: Demand::Skip(gct as u64), consumed: pos, events };
            }
        }

        loop {
            match self.state {
                State::Header => unreachable!(),
                State::CapturingXmp => {
                    // 窗口从 pos 开始，此时 pos 在 XMP 包内容起点（紧随 14 字节 app-ext 头）。
                    // 查找终止符 0x00。
                    let rest = &input[pos..];
                    match rest.iter().position(|&b| b == 0) {
                        Some(zero) => {
                            // 找到魔数尾起点以更精确截断包
                            let magic = find_magic(rest);
                            let pkt_end = magic.unwrap_or(zero);
                            let packet = &rest[..pkt_end];
                            events.push(Event::Payload { kind: PayloadKind::Xmp, data: packet });
                            // 消费到 0x00 终止符（含），回到 Block 状态
                            self.state = State::Block;
                            pos += zero + 1;
                            continue;
                        }
                        None => {
                            // 终止符还未到达——请求更多字节（consumed > 0 确保 driver 不认为零前进）
                            return PullResult {
                                demand: Demand::NeedBytes(rest.len() + 1),
                                consumed: pos,
                                events,
                            };
                        }
                    }
                }
                State::SubBlocks => {
                    let rest = &input[pos..];
                    if rest.is_empty() {
                        return PullResult { demand: Demand::NeedBytes(1), consumed: pos, events };
                    }
                    let len = rest[0] as usize;
                    if len == 0 {
                        // 链终止，回到 Block
                        self.state = State::Block;
                        pos += 1;
                        continue;
                    }
                    // 跳过长度字节 + len 数据
                    return PullResult {
                        demand: Demand::Skip(len as u64),
                        consumed: pos + 1,
                        events,
                    };
                }
                State::Block => {
                    let rest = &input[pos..];
                    if rest.is_empty() {
                        return PullResult { demand: Demand::NeedBytes(1), consumed: pos, events };
                    }
                    match rest[0] {
                        0x3B => {
                            // Trailer
                            self.done = true;
                            return PullResult { demand: Demand::Done, consumed: pos + 1, events };
                        }
                        0x2C => {
                            // 图像描述符：需 10 字节(1 引导 + 9 描述符)读 packed
                            if rest.len() < 10 {
                                return PullResult { demand: Demand::NeedBytes(10), consumed: pos, events };
                            }
                            let packed = rest[9];
                            let lct = if packed & 0x80 != 0 {
                                3usize * (1usize << ((packed & 0x07) + 1))
                            } else {
                                0
                            };
                            // 消费 10 字节描述符；跳过 LCT + 1 字节 LZW 最小码长；转 SubBlocks
                            self.state = State::SubBlocks;
                            let skip = (lct as u64) + 1;
                            return PullResult { demand: Demand::Skip(skip), consumed: pos + 10, events };
                        }
                        0x21 => {
                            // 扩展：需第 2 字节 label
                            if rest.len() < 2 {
                                return PullResult { demand: Demand::NeedBytes(2), consumed: pos, events };
                            }
                            let label = rest[1];
                            if label == 0xFF {
                                // Application Extension：需 block size 字节 + 11 字节 id
                                if rest.len() < 3 + 11 {
                                    return PullResult { demand: Demand::NeedBytes(3 + 11), consumed: pos, events };
                                }
                                let id = &rest[3..3 + 11];
                                if id == b"XMP DataXMP" {
                                    // 消费 14 字节头，切换到 CapturingXmp 状态，
                                    // 下次 pull 窗口从 XMP 包内容开始，可安全积累直到 0x00。
                                    self.state = State::CapturingXmp;
                                    return PullResult {
                                        demand: Demand::Skip(0),
                                        consumed: pos + 3 + 11,
                                        events,
                                    };
                                }
                                // 非 XMP 应用扩展：消费 0x21 0xFF size(1) id(11)，转 SubBlocks
                                self.state = State::SubBlocks;
                                return PullResult { demand: Demand::Skip(0), consumed: pos + 3 + 11, events };
                            }
                            // 其他扩展（注释/图形控制/纯文本）：消费 0x21 label，转 SubBlocks
                            self.state = State::SubBlocks;
                            return PullResult { demand: Demand::Skip(0), consumed: pos + 2, events };
                        }
                        _ => {
                            // 畸形引导字节：best-effort 收尾
                            self.done = true;
                            return PullResult { demand: Demand::Done, consumed: pos, events };
                        }
                    }
                }
            }
        }
    }
}

/// 找魔数尾起点（子序列 0x01 0xFF 0xFE）。
fn find_magic(b: &[u8]) -> Option<usize> {
    if b.len() < 3 {
        return None;
    }
    (0..=b.len() - 3).find(|&k| b[k] == 0x01 && b[k + 1] == 0xFF && b[k + 2] == 0xFE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_lsd(w: u16, h: u16, gct: bool) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(b"GIF89a");
        g.extend_from_slice(&w.to_le_bytes());
        g.extend_from_slice(&h.to_le_bytes());
        g.push(if gct { 0x80 } else { 0x00 }); // packed：无 GCT
        g.push(0); // bg
        g.push(0); // aspect
        g
    }

    /// XMP Application Extension：0x21 0xFF 0x0B "XMP Data" "XMP" + 包 + 魔数尾(以 0x00 结束)。
    fn xmp_app_ext(packet: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.push(0x21);
        e.push(0xFF);
        e.push(0x0B);
        e.extend_from_slice(b"XMP DataXMP");
        e.extend_from_slice(packet);
        // 魔数尾：0x01,0xFF,0xFE,...,0x00（递降）。最末 0x00 为终止符。
        e.push(0x01);
        for v in (0u8..=0xFF).rev() {
            e.push(v);
        }
        e
    }

    fn collect(buf: &[u8]) -> crate::driver::Collector {
        let mut p = GifParser::new();
        crate::driver::drive_slice(buf, &mut p, crate::limits::Limits::default())
    }

    #[test]
    fn lsd_dimensions_and_xmp() {
        let mut g = header_lsd(800, 600, false);
        g.extend_from_slice(&xmp_app_ext(br#"<rdf:Description tiff:Make="Acme"/>"#));
        g.push(0x3B); // trailer
        let col = collect(&g);
        let meta = crate::driver::finalize(col, crate::model::FileFormat::Gif);
        assert_eq!(meta.unified.width, Some(800));
        assert_eq!(meta.unified.height, Some(600));
        assert!(meta.raw.xmp.iter().any(|x| x.name == "Make" && x.value == "Acme"));
    }

    #[test]
    fn skips_comment_extension() {
        let mut g = header_lsd(2, 2, false);
        // 注释扩展 0x21 0xFE + sub-block("hi") + 终止 0x00
        g.push(0x21);
        g.push(0xFE);
        g.push(2);
        g.extend_from_slice(b"hi");
        g.push(0x00);
        g.push(0x3B);
        let col = collect(&g);
        assert!(col.warnings.is_empty(), "warnings: {:?}", col.warnings);
        assert!(col.xmp.is_empty());
    }

    #[test]
    fn non_gif_done_no_events() {
        let mut p = GifParser::new();
        let res = p.pull(b"NOTAGIFFFFFFF");
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }
}
