//! JPEG 段遍历（增量状态机）：SOI 起逐段推进。
//! 元数据段（APP1/Exif）整段入窗后发 Payload；非元数据段发 Skip 让驱动跳过
//! （可 Seek 源借此原生 seek 省 I/O）；SOS/EOI 发 Done。窗口不足发 NeedBytes。
//! 契约：仅在 input.len() < 所需 时发 NeedBytes(n)，n 相对 consumed 之后的新窗口起点。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PayloadKind, PullResult};
use crate::model::Field;

#[derive(Debug, Default)]
pub struct JpegParser {
    saw_soi: bool,
    done: bool,
}

impl JpegParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for JpegParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let mut events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }

        let mut pos = 0usize;
        if !self.saw_soi {
            if input.len() < 2 {
                return PullResult { demand: Demand::NeedBytes(2), consumed: 0, events };
            }
            if input[0] != 0xFF || input[1] != 0xD8 {
                self.done = true; // 非 JPEG：best-effort 收尾
                return PullResult { demand: Demand::Done, consumed: 0, events };
            }
            self.saw_soi = true;
            pos = 2;
        }

        loop {
            let rest = &input[pos..];
            // 段以 0xFF + 码字开头，码字前可有重复 0xFF 填充字节。
            if rest.is_empty() {
                return PullResult { demand: Demand::NeedBytes(2), consumed: pos, events };
            }
            if rest[0] != 0xFF {
                self.done = true; // 畸形：停止，已收集照常返回
                return PullResult { demand: Demand::Done, consumed: pos, events };
            }
            let mut i = 1;
            while i < rest.len() && rest[i] == 0xFF {
                i += 1;
            }
            if i >= rest.len() {
                // 还差码字字节
                return PullResult { demand: Demand::NeedBytes(i + 1), consumed: pos, events };
            }
            let marker = rest[i];
            let after = i + 1; // rest 内：码字之后

            match marker {
                0xD9 | 0xDA => {
                    // EOI / SOS：元数据到此为止
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: pos + after, events };
                }
                0x01 | 0xD0..=0xD7 => {
                    // TEM / RSTn：无长度字段
                    pos += after;
                    continue;
                }
                0x00 => {
                    // 字节填充（0xFF 0x00）出现在标记区属畸形；best-effort 停止，
                    // 不尝试把后续字节解释为长度（否则产生虚假巨型 Skip）。
                    self.done = true;
                    return PullResult { demand: Demand::Done, consumed: pos, events };
                }
                0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                    // SOF：读 precision(1) + height(2 BE) + width(2 BE)
                    if rest.len() < after + 2 {
                        return PullResult { demand: Demand::NeedBytes(after + 2), consumed: pos, events };
                    }
                    let len = u16::from_be_bytes([rest[after], rest[after + 1]]) as usize;
                    if len < 2 {
                        self.done = true;
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                    let body_start = after + 2;
                    // 需要 body 前 5 字节：precision(1)+height(2)+width(2)
                    if rest.len() < body_start + 5 {
                        return PullResult { demand: Demand::NeedBytes(body_start + 5), consumed: pos, events };
                    }
                    let h = u16::from_be_bytes([rest[body_start + 1], rest[body_start + 2]]) as u32;
                    let w = u16::from_be_bytes([rest[body_start + 3], rest[body_start + 4]]) as u32;
                    events.push(Event::Field(Field::Width(w)));
                    events.push(Event::Field(Field::Height(h)));
                    // 跳过整段剩余（消费段头，Skip body）
                    let body_len = len - 2;
                    return PullResult {
                        demand: Demand::Skip(body_len as u64),
                        consumed: pos + body_start,
                        events,
                    };
                }
                _ => {
                    if rest.len() < after + 2 {
                        return PullResult { demand: Demand::NeedBytes(after + 2), consumed: pos, events };
                    }
                    let len = u16::from_be_bytes([rest[after], rest[after + 1]]) as usize;
                    if len < 2 {
                        self.done = true; // 畸形长度
                        return PullResult { demand: Demand::Done, consumed: pos, events };
                    }
                    let body_len = len - 2;
                    let body_start = after + 2; // rest 内 body 起点
                    let seg_total = body_start + body_len; // rest 内段尾

                    if marker == 0xE1 {
                        // APP1：需整段入窗才能判定并发出
                        if rest.len() < seg_total {
                            return PullResult { demand: Demand::NeedBytes(seg_total), consumed: pos, events };
                        }
                        let body = &rest[body_start..seg_total];
                        if body.starts_with(b"Exif\0\0") {
                            events.push(Event::Payload { kind: PayloadKind::Exif, data: &body[6..] });
                        }
                        pos += seg_total;
                        continue;
                    } else {
                        // 非元数据段：跳过段体（消费段头，Skip body_len）
                        return PullResult {
                            demand: Demand::Skip(body_len as u64),
                            consumed: pos + body_start,
                            events,
                        };
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 JPEG：SOI + APP1(Exif + 3 字节假 TIFF) + EOI。
    fn jpeg_with_exif() -> Vec<u8> {
        let tiff = [0xAAu8, 0xBB, 0xCC]; // 占位 TIFF 内容
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    #[test]
    fn emits_exif_payload() {
        let j = jpeg_with_exif();
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Done);
        assert_eq!(res.events.len(), 1);
        match &res.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0xAA, 0xBB, 0xCC][..]); // "Exif\0\0" 已剥离
            }
            _ => panic!("expected payload"),
        }
    }

    #[test]
    fn non_jpeg_emits_nothing() {
        let mut p = JpegParser::new();
        let res = p.pull(&[0x00, 0x01, 0x02, 0x03]);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn skips_preceding_app0_segment() {
        // 真实 JPEG 通常有 APP0(JFIF) 在 APP1 之前；增量解析器通过 Skip + 续跑找到 Exif。
        let tiff = [0x11u8, 0x22, 0x33];
        let mut exif_body: Vec<u8> = Vec::new();
        exif_body.extend_from_slice(b"Exif\0\0");
        exif_body.extend_from_slice(&tiff);
        let exif_len = (exif_body.len() + 2) as u16;

        let app0_body = [0x4Au8, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01]; // "JFIF\0" + 2
        let app0_len = (app0_body.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE0]); // APP0
        j.extend_from_slice(&app0_len.to_be_bytes());
        j.extend_from_slice(&app0_body);
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&exif_len.to_be_bytes());
        j.extend_from_slice(&exif_body);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let mut p = JpegParser::new();
        // pull #1：APP0 → Skip(body_len)，consumed = SOI + marker + len = 6
        let r1 = p.pull(&j);
        assert!(matches!(r1.demand, Demand::Skip(_)));
        let skip_n = match r1.demand { Demand::Skip(n) => n, _ => unreachable!() };
        let pos2 = r1.consumed + skip_n as usize;
        // pull #2：从 APP1 开始 → Payload + Done
        let r2 = p.pull(&j[pos2..]);
        assert_eq!(r2.demand, Demand::Done);
        assert_eq!(r2.events.len(), 1);
        match &r2.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0x11, 0x22, 0x33][..]);
            }
            _ => panic!("expected payload"),
        }
    }

    #[test]
    fn app1_non_exif_emits_nothing() {
        // APP1 但非 Exif（如 XMP 命名空间）不应被当作 Exif 发出。
        let body = b"http://ns.adobe.com/xap/1.0/\0xmpdata";
        let len = (body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        assert!(res.events.is_empty());
    }

    #[test]
    fn truncated_mid_segment_is_safe() {
        // APP1 声称 length=100 但实际字节不足；增量解析器发 NeedBytes、无事件、不 panic。
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&100u16.to_be_bytes()); // 声称 98 字节 body
        j.extend_from_slice(b"Exif\0\0ab"); // 只有 8 字节，截断
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        assert!(matches!(res.demand, Demand::NeedBytes(_)));
        assert!(res.events.is_empty());
    }

    #[test]
    fn stuffed_ff00_in_marker_area_stops_without_false_event() {
        // SOI 后出现 0xFF 0x00（标记区非法填充）→ 干净停止，无事件，不 panic。
        // 增量解析器对畸形标记 best-effort：NeedBytes 或 Done 均可，不 panic 为要。
        let j = [0xFFu8, 0xD8, 0xFF, 0x00, 0xFF, 0xD9];
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        // 畸形段不应发出任何事件。
        assert!(res.events.is_empty());
    }

    /// 标记区字节填充（0xFF 0x00）→ 干净 Done，drive_slice 无警告。
    #[test]
    fn marker_stuffing_in_segment_region_stops_cleanly() {
        use crate::driver::drive_slice;
        use crate::limits::Limits;
        // SOI + 0xFF 0x00（非法标记区填充）+ 更多字节（应被忽略）
        let j = [0xFFu8, 0xD8, 0xFF, 0x00, 0xAA, 0xBB, 0xCC];
        let mut p = JpegParser::new();
        // (a) 解析器直接返回 Done
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
        // (b) 通过 drive_slice 跑：无警告（停止干净，不触发 UnreachableSection）
        let mut p2 = JpegParser::new();
        let col = drive_slice(&j, &mut p2, Limits::default());
        assert!(col.warnings.is_empty(), "expected no warnings, got: {:?}", col.warnings);
    }

    /// 截断在 APP1 段体中间：窗口不足应发 NeedBytes 而非静默 Done。
    #[test]
    fn truncated_app1_requests_more_bytes() {
        // SOI + APP1(声明 len=20，但只给 4 字节 body)
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&20u16.to_be_bytes()); // 段长 20 → body 18
        j.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // 只有 4 字节 body
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        match res.demand {
            Demand::NeedBytes(_) => {}
            other => panic!("expected NeedBytes, got {other:?}"),
        }
        assert!(res.events.is_empty());
    }

    /// SOF0 段应发出 Width/Height Field，并继续到 EOI。
    #[test]
    fn sof_emits_dimensions() {
        // SOI + SOF0(len=17: precision1 + height2 + width2 + ...) + EOI
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xC0]); // SOF0
        // 段长 = 2(len) + 1(precision) + 2(height) + 2(width) + 6(1 组件) = 13
        j.extend_from_slice(&13u16.to_be_bytes());
        j.push(8); // precision
        j.extend_from_slice(&1080u16.to_be_bytes()); // height
        j.extend_from_slice(&1920u16.to_be_bytes()); // width
        j.extend_from_slice(&[1, 0x11, 0]); // 1 个组件
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let mut p = JpegParser::new();
        let res = p.pull(&j);
        let mut w = None;
        let mut h = None;
        for ev in &res.events {
            if let Event::Field(crate::model::Field::Width(x)) = ev {
                w = Some(*x);
            }
            if let Event::Field(crate::model::Field::Height(x)) = ev {
                h = Some(*x);
            }
        }
        assert_eq!(w, Some(1920));
        assert_eq!(h, Some(1080));
    }

    /// 非元数据段（APP0/JFIF）应发 Skip 跳过段体，consumed 指向段体起点。
    #[test]
    fn non_metadata_segment_emits_skip() {
        // SOI + APP0(len=8 → body 6)
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]);
        j.extend_from_slice(&8u16.to_be_bytes());
        j.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // 6 字节 body
        let mut p = JpegParser::new();
        let res = p.pull(&j);
        assert_eq!(res.demand, Demand::Skip(6));
        // consumed = SOI(2) + 段头(marker2 + len2 = 4) = 6，指向 body 起点
        assert_eq!(res.consumed, 6);
        assert!(res.events.is_empty());
    }

    /// 跨多次 pull 拼出 APP0(skip) → APP1(payload) → EOI(done)。
    #[test]
    fn resumes_across_pulls() {
        let tiff = [0xAAu8, 0xBB, 0xCC];
        let mut app1: Vec<u8> = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let app1_len = (app1.len() + 2) as u16;

        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&[0xFF, 0xE0]); // APP0
        j.extend_from_slice(&8u16.to_be_bytes());
        j.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        j.extend_from_slice(&[0xFF, 0xE1]); // APP1
        j.extend_from_slice(&app1_len.to_be_bytes());
        j.extend_from_slice(&app1);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let mut p = JpegParser::new();
        // pull #1：到 APP0 段头 → Skip(6)
        let r1 = p.pull(&j);
        assert_eq!(r1.demand, Demand::Skip(6));
        let _pos = r1.consumed + 6; // 模拟 driver 跳过段体
        // pull #2：APP1 → payload，随后 EOI → Done
        let r2 = p.pull(&j[_pos..]);
        assert_eq!(r2.demand, Demand::Done);
        assert_eq!(r2.events.len(), 1);
        match &r2.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &[0xAA, 0xBB, 0xCC][..]);
            }
            _ => panic!("expected payload"),
        }
    }
}
