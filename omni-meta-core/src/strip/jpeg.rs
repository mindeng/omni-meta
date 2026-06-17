//! JPEG 剥离 walker：逐段遍历，元数据段 Drop、结构/图像段 Emit。
//! SOS 起的熵编码数据整段 Emit 到尾。keep_orientation 时合成最小 EXIF。
//! slice 全缓冲驱动：一次 pull 处理整个输入。

use alloc::vec::Vec;

use super::exif_synth::{jpeg_app1_exif, orientation_tiff};
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

pub struct JpegStripper {
    opts: StripOptions,
}

impl JpegStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

/// 在 EXIF TIFF 内就地查 Orientation(0x0112) 值（仅扫 IFD0，best-effort）。
fn find_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 {
        return None;
    }
    let le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let rd16 = |b: &[u8]| {
        if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    };
    let rd32 = |b: &[u8]| {
        if le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    };
    let ifd0 = rd32(&tiff[4..8]) as usize;
    let count_end = ifd0.checked_add(2)?;
    if count_end > tiff.len() {
        return None;
    }
    let count = rd16(&tiff[ifd0..]) as usize;
    let mut e = count_end;
    for _ in 0..count {
        let entry_end = e.checked_add(12)?;
        if entry_end > tiff.len() {
            return None;
        }
        let tag = rd16(&tiff[e..]);
        if tag == 0x0112 {
            return Some(rd16(&tiff[e + 8..])); // SHORT 内联值在 entry+8
        }
        e = entry_end;
    }
    None
}

impl StripPlanner for JpegStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        let mut cmds: Vec<StripCmd> = Vec::new();

        // 非 JPEG 或太短：原样保留全部。
        if input.len() < 2 || input[0] != 0xFF || input[1] != 0xD8 {
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult {
                demand: StripDemand::Done,
                consumed: input.len(),
                cmds,
            };
        }

        cmds.push(StripCmd::Emit(2)); // SOI
        let mut pos = 2usize;
        // 记录待合成 orientation（首个含 orientation 的 EXIF 段决定）。
        let mut synth_orientation: Option<u16> = None;

        loop {
            // 标记区：FF + (可能多个 FF) + 码字
            if pos >= input.len() {
                break;
            }
            if input[pos] != 0xFF {
                // 畸形（标记区无 FF）：保留剩余字节，安全停止。
                cmds.push(StripCmd::Emit(input.len() - pos));
                pos = input.len();
                break;
            }
            let mut i = pos + 1;
            while i < input.len() && input[i] == 0xFF {
                i += 1;
            }
            if i >= input.len() {
                cmds.push(StripCmd::Emit(input.len() - pos));
                pos = input.len();
                break;
            }
            let marker = input[i];
            let marker_hdr = i + 1 - pos; // pos..i+1（FF...码字）字节数

            match marker {
                0xDA => {
                    // SOS：其后熵编码数据 + 后续标记 + EOI 全部原样到尾。
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                    break;
                }
                0xD9 => {
                    // EOI：保留并结束。
                    cmds.push(StripCmd::Emit(marker_hdr));
                    pos = i + 1;
                    break;
                }
                0x01 | 0xD0..=0xD7 => {
                    // 无长度字段的标记：保留。
                    cmds.push(StripCmd::Emit(marker_hdr));
                    pos = i + 1;
                }
                _ => {
                    // 有长度字段的段：码字后 2 字节为段长。
                    if i + 3 > input.len() {
                        cmds.push(StripCmd::Emit(input.len() - pos));
                        pos = input.len();
                        break;
                    }
                    let seg_len = u16::from_be_bytes([input[i + 1], input[i + 2]]) as usize;
                    if seg_len < 2 {
                        cmds.push(StripCmd::Emit(input.len() - pos));
                        pos = input.len();
                        break;
                    }
                    let body_start = i + 3; // 段体起点
                    let body_len = seg_len - 2;
                    let seg_end = match body_start.checked_add(body_len) {
                        Some(v) if v <= input.len() => v,
                        _ => {
                            cmds.push(StripCmd::Emit(input.len() - pos));
                            pos = input.len();
                            break;
                        }
                    };
                    let body = &input[body_start..seg_end];
                    let total = seg_end - pos; // 整段（含 FF+码字+len+body）字节数

                    let drop_kind = classify(marker, body, &self.opts);
                    match drop_kind {
                        Some((kind, is_exif)) => {
                            if is_exif && self.opts.keep_orientation && synth_orientation.is_none()
                            {
                                if body.len() > 6 && body.starts_with(b"Exif\0\0") {
                                    synth_orientation = find_orientation(&body[6..]);
                                }
                            }
                            cmds.push(StripCmd::Drop { len: total, kind });
                        }
                        None => {
                            cmds.push(StripCmd::Emit(total));
                        }
                    }
                    pos = seg_end;
                }
            }
        }

        // 合成最小 EXIF：紧随 SOI Emit(2) 注入（cmds[1]），保证读路径在 SOS 前命中。
        if let Some(val) = synth_orientation {
            let seg = jpeg_app1_exif(&orientation_tiff(val));
            cmds.insert(1, StripCmd::Insert(seg));
        }

        StripResult {
            demand: StripDemand::Done,
            consumed: pos,
            cmds,
        }
    }
}

/// 判定一个段是否该删。返回 Some((kind, is_exif))。is_exif 触发 orientation 合成。
fn classify(marker: u8, body: &[u8], opts: &StripOptions) -> Option<(RemovedKind, bool)> {
    match marker {
        0xE1 => {
            if body.starts_with(b"Exif\0\0") {
                Some((RemovedKind::Exif, true))
            } else if body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                Some((RemovedKind::Xmp, false))
            } else {
                None
            }
        }
        0xED => {
            if body.windows(4).any(|w| w == b"8BIM") {
                Some((RemovedKind::Iptc, false))
            } else {
                None
            }
        }
        0xE2 => {
            if body.starts_with(b"ICC_PROFILE\0") {
                if opts.keep_icc {
                    None
                } else {
                    Some((RemovedKind::Icc, false))
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 供 png/webp walker 复用：在裸 TIFF 中查 orientation。
pub(crate) fn find_orientation_pub(tiff: &[u8]) -> Option<u16> {
    find_orientation(tiff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileFormat;
    use crate::strip::{RemovedKind, StripOptions, drive_strip_slice};

    fn app_seg(marker: u8, body: &[u8]) -> alloc::vec::Vec<u8> {
        let mut s = alloc::vec::Vec::new();
        s.extend_from_slice(&[0xFF, marker]);
        s.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(body);
        s
    }

    /// SOI + APP0(JFIF) + APP1(Exif) + APP1(XMP) + SOF0 + SOS + 图像 + EOI
    fn full_jpeg() -> alloc::vec::Vec<u8> {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&app_seg(0xE0, b"JFIF\0\x01\x01")); // APP0
        let mut exif_body = alloc::vec::Vec::new();
        exif_body.extend_from_slice(b"Exif\0\0");
        exif_body.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(6));
        j.extend_from_slice(&app_seg(0xE1, &exif_body)); // APP1 Exif
        j.extend_from_slice(&app_seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>")); // APP1 XMP
        // SOF0：len=10 precision/h/w/comp
        j.extend_from_slice(&[0xFF, 0xC0]);
        j.extend_from_slice(&10u16.to_be_bytes());
        j.push(8);
        j.extend_from_slice(&8u16.to_be_bytes()); // height
        j.extend_from_slice(&8u16.to_be_bytes()); // width
        j.extend_from_slice(&[1, 0x11, 0]);
        j.extend_from_slice(&[0xFF, 0xDA]); // SOS
        j.extend_from_slice(&4u16.to_be_bytes()); // SOS header len
        j.extend_from_slice(&[1, 0, 0]); // SOS body
        j.extend_from_slice(&[0x12, 0x34, 0x56]); // 熵编码数据
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = JpegStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Jpeg)
    }

    #[test]
    fn default_strips_exif_and_xmp_keeps_orientation() {
        let j = full_jpeg();
        let (out, report) = run(&j, StripOptions::default());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(
            meta.unified.orientation,
            Some(crate::model::Orientation::Rotate90)
        );
        assert_eq!(meta.unified.width, Some(8));
        assert_eq!(meta.unified.height, Some(8));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert_eq!(&out[out.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn aggressive_strips_orientation_too_zero_exif() {
        let j = full_jpeg();
        let (out, _r) = run(&j, StripOptions::aggressive());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(
            meta.raw.exif.is_empty(),
            "exif should be empty: {:?}",
            meta.raw.exif
        );
        assert_eq!(meta.unified.orientation, None);
    }

    #[test]
    fn keeps_app0_jfif() {
        let j = full_jpeg();
        let (out, _r) = run(&j, StripOptions::aggressive());
        assert!(out.windows(4).any(|w| w == b"JFIF"));
    }

    #[test]
    fn strips_app13_iptc() {
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        j.extend_from_slice(&app_seg(0xED, b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0")); // APP13 8BIM
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 1, 0, 0]); // SOS
        j.extend_from_slice(&[0xFF, 0xD9]);
        let (out, report) = run(&j, StripOptions::default());
        assert!(report.removed.contains(RemovedKind::Iptc));
        assert!(!out.windows(4).any(|w| w == b"8BIM"));
    }

    #[test]
    fn non_jpeg_returns_input_unchanged_no_panic() {
        let buf = [0u8, 1, 2, 3, 4];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf);
    }

    #[test]
    fn icc_kept_by_default_dropped_aggressive() {
        // 构造 JPEG：SOI + APP2(ICC_PROFILE) + SOS + EOI。
        let icc_body = b"ICC_PROFILE\0\x01\x01somedata";
        let mut j = alloc::vec::Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
        j.extend_from_slice(&app_seg(0xE2, icc_body)); // APP2 ICC
        j.extend_from_slice(&[0xFF, 0xDA]); // SOS
        j.extend_from_slice(&4u16.to_be_bytes()); // SOS header len
        j.extend_from_slice(&[1, 0, 0]); // SOS body
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI

        // default：ICC_PROFILE 保留。
        let (out_default, _r) = run(&j, StripOptions::default());
        assert!(
            out_default.windows(11).any(|w| w == b"ICC_PROFILE"),
            "default: ICC_PROFILE bytes should be present"
        );

        // aggressive：ICC_PROFILE 被删，report 含 Icc。
        let (out_agg, report_agg) = run(&j, StripOptions::aggressive());
        assert!(
            !out_agg.windows(11).any(|w| w == b"ICC_PROFILE"),
            "aggressive: ICC_PROFILE bytes should be removed"
        );
        assert!(report_agg.removed.contains(RemovedKind::Icc));
    }
}
