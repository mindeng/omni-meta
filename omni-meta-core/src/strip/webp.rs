//! WebP 剥离 walker：逐 RIFF chunk 遍历，删 EXIF/XMP/ICC（视选项），
//! 重算 RIFF filesize、更新 VP8X flags；keep_orientation 合成回 EXIF chunk。
//! 一次 pull 完成（slice 全缓冲）。

use alloc::vec::Vec;

use super::exif_synth::{orientation_tiff, webp_exif_chunk};
use super::jpeg::find_orientation_pub as find_orientation;
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

const FLAG_ICC: u8 = 0x20;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;

pub struct WebpStripper {
    opts: StripOptions,
}

impl WebpStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

impl StripPlanner for WebpStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        // 非 WebP：原样保留。
        if input.len() < 12 || &input[0..4] != b"RIFF" || &input[8..12] != b"WEBP" {
            let mut cmds = Vec::new();
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult { demand: StripDemand::Done, consumed: input.len(), cmds };
        }

        let mut report_removed: Vec<(usize, RemovedKind)> = Vec::new(); // (len, kind)
        let mut synth_orientation: Option<u16> = None;

        // 第一遍：找 orientation（在 EXIF chunk 内），决定是否合成。
        {
            let mut p = 12usize;
            while p + 8 <= input.len() {
                let fourcc = &input[p..p + 4];
                let size = u32::from_le_bytes([input[p + 4], input[p + 5], input[p + 6], input[p + 7]]) as usize;
                let pad = size & 1;
                let data_end = match p.checked_add(8).and_then(|v| v.checked_add(size)) {
                    Some(v) if v <= input.len() => v,
                    _ => break,
                };
                if fourcc == b"EXIF" && self.opts.keep_orientation {
                    synth_orientation = find_orientation(&input[p + 8..data_end]);
                }
                p = match data_end.checked_add(pad) {
                    Some(v) => v,
                    None => break,
                };
            }
        }

        // 第二遍：重建 body。
        let mut new_body: Vec<u8> = Vec::new();
        new_body.extend_from_slice(b"WEBP");
        let mut vp8x_flag_pos: Option<usize> = None; // new_body 内 VP8X data[0] 偏移

        let mut p = 12usize;
        let mut truncated_tail: Option<usize> = None;
        while p + 8 <= input.len() {
            let fourcc = [input[p], input[p + 1], input[p + 2], input[p + 3]];
            let size = u32::from_le_bytes([input[p + 4], input[p + 5], input[p + 6], input[p + 7]]) as usize;
            let pad = size & 1;
            let chunk_end = match p.checked_add(8).and_then(|v| v.checked_add(size)).and_then(|v| v.checked_add(pad)) {
                Some(v) if v <= input.len() => v,
                _ => {
                    truncated_tail = Some(p);
                    break;
                }
            };

            let kind = match &fourcc {
                b"EXIF" => Some(RemovedKind::Exif),
                b"XMP " => Some(RemovedKind::Xmp),
                b"ICCP" => {
                    if self.opts.keep_icc { None } else { Some(RemovedKind::Icc) }
                }
                _ => None,
            };

            match kind {
                Some(k) => {
                    report_removed.push((chunk_end - p, k));
                }
                None => {
                    if &fourcc == b"VP8X" && size >= 1 {
                        vp8x_flag_pos = Some(new_body.len() + 8); // data[0]
                    }
                    new_body.extend_from_slice(&input[p..chunk_end]);
                    if &fourcc == b"VP8X" {
                        if let Some(val) = synth_orientation.take() {
                            new_body.extend_from_slice(&webp_exif_chunk(&orientation_tiff(val)));
                        }
                    }
                }
            }
            p = chunk_end;
        }

        // 若没有 VP8X 但仍需合成 orientation（极少见）：追加 EXIF chunk 到 body 末尾。
        if let Some(val) = synth_orientation.take() {
            new_body.extend_from_slice(&webp_exif_chunk(&orientation_tiff(val)));
        }

        // 更新 VP8X flags：清 XMP；EXIF/ICC 视最终是否保留。
        if let Some(fp) = vp8x_flag_pos {
            if fp < new_body.len() {
                let mut flags = new_body[fp];
                flags &= !FLAG_XMP;
                let has_exif = new_body.windows(4).any(|w| w == b"EXIF");
                if has_exif { flags |= FLAG_EXIF; } else { flags &= !FLAG_EXIF; }
                let has_icc = new_body.windows(4).any(|w| w == b"ICCP");
                if has_icc { flags |= FLAG_ICC; } else { flags &= !FLAG_ICC; }
                new_body[fp] = flags;
            }
        }

        // 越界尾：把原始 [truncated_tail..] 追加（安全保留）。
        if let Some(tt) = truncated_tail {
            new_body.extend_from_slice(&input[tt..]);
        }

        // 组装：RIFF + filesize(LE) + new_body。
        let mut out_chunk: Vec<u8> = Vec::with_capacity(8 + new_body.len());
        out_chunk.extend_from_slice(b"RIFF");
        out_chunk.extend_from_slice(&(new_body.len() as u32).to_le_bytes());
        out_chunk.extend_from_slice(&new_body);

        let mut cmds: Vec<StripCmd> = Vec::new();
        cmds.push(StripCmd::Replace { consume: input.len(), with: out_chunk });
        for (len, kind) in report_removed {
            cmds.push(StripCmd::Account { len: len as u64, kind });
        }

        StripResult { demand: StripDemand::Done, consumed: input.len(), cmds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{drive_strip_slice, RemovedKind, StripOptions};
    use crate::model::FileFormat;

    fn riff_chunk(fourcc: &[u8; 4], data: &[u8]) -> alloc::vec::Vec<u8> {
        let mut c = alloc::vec::Vec::new();
        c.extend_from_slice(fourcc);
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    }

    fn vp8x(flags: u8, w: u32, h: u32) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec![0u8; 10];
        d[0] = flags;
        let wm1 = w - 1;
        let hm1 = h - 1;
        d[4] = (wm1 & 0xFF) as u8;
        d[5] = ((wm1 >> 8) & 0xFF) as u8;
        d[6] = ((wm1 >> 16) & 0xFF) as u8;
        d[7] = (hm1 & 0xFF) as u8;
        d[8] = ((hm1 >> 8) & 0xFF) as u8;
        d[9] = ((hm1 >> 16) & 0xFF) as u8;
        riff_chunk(b"VP8X", &d)
    }

    /// RIFF/WEBP，VP8X(flags=EXIF|XMP|ICC) + ICCP + VP8 + EXIF + XMP
    fn full_webp() -> alloc::vec::Vec<u8> {
        let mut body = alloc::vec::Vec::new();
        body.extend_from_slice(b"WEBP");
        body.extend_from_slice(&vp8x(0x08 | 0x04 | 0x20, 8, 8));
        body.extend_from_slice(&riff_chunk(b"ICCP", b"iccdata1")); // 8 字节
        body.extend_from_slice(&riff_chunk(b"VP8 ", &[0u8; 12]));
        let mut exif = alloc::vec::Vec::new();
        exif.extend_from_slice(&crate::strip::exif_synth::orientation_tiff(6));
        body.extend_from_slice(&riff_chunk(b"EXIF", &exif));
        body.extend_from_slice(&riff_chunk(b"XMP ", br#"<x/>"#));
        let mut f = alloc::vec::Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = WebpStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Webp)
    }

    #[test]
    fn filesize_recomputed_and_valid() {
        let (out, _r) = run(&full_webp(), StripOptions::default());
        assert_eq!(&out[0..4], b"RIFF");
        let declared = u32::from_le_bytes([out[4], out[5], out[6], out[7]]) as usize;
        assert_eq!(declared, out.len() - 8, "filesize 应等于其后字节数");
        assert_eq!(&out[8..12], b"WEBP");
    }

    #[test]
    fn default_strips_exif_xmp_keeps_icc_orientation() {
        let (out, report) = run(&full_webp(), StripOptions::default());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        assert!(out.windows(4).any(|w| w == b"ICCP"));
        assert!(!out.windows(4).any(|w| w == b"XMP "));
    }

    #[test]
    fn vp8x_flags_updated() {
        let (out, _r) = run(&full_webp(), StripOptions::default());
        let idx = out.windows(4).position(|w| w == b"VP8X").unwrap();
        let flags = out[idx + 8];
        assert_eq!(flags & 0x04, 0, "XMP bit 应清除");
        assert_eq!(flags & 0x08, 0x08, "EXIF bit 应保留（合成回 orientation）");
        assert_eq!(flags & 0x20, 0x20, "ICC bit 应保留");
    }

    #[test]
    fn aggressive_clears_exif_icc_bits() {
        let (out, _r) = run(&full_webp(), StripOptions::aggressive());
        let idx = out.windows(4).position(|w| w == b"VP8X").unwrap();
        let flags = out[idx + 8];
        assert_eq!(flags & 0x08, 0, "EXIF bit 清除");
        assert_eq!(flags & 0x20, 0, "ICC bit 清除");
        assert!(!out.windows(4).any(|w| w == b"ICCP"));
    }

    #[test]
    fn non_webp_returns_input_unchanged() {
        let buf = [0u8, 1, 2, 3];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf);
    }

    #[test]
    fn malformed_vp8x_zero_size_no_panic() {
        // VP8X 声明 size=0（畸形，无 flag 字节），且仅含 EXIF chunk（被剥除）→
        // 剥除后 fp == new_body.len()，new_body[fp] 越界 panic。绝不 panic，安全输出。
        let mut body = alloc::vec::Vec::new();
        body.extend_from_slice(b"WEBP");
        body.extend_from_slice(b"VP8X");
        body.extend_from_slice(&0u32.to_le_bytes()); // size = 0（畸形）
        // EXIF chunk 会被剥除，剥除后 new_body 仅有 WEBP(4)+VP8X_header(8)=12 字节。
        // fp=12 == new_body.len() → 越界。
        body.extend_from_slice(&riff_chunk(b"EXIF", &[1u8, 2, 3, 4]));
        let mut f = alloc::vec::Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        // 不得 panic；输出 RIFF 头自洽。
        let (out, _r) = run(&f, StripOptions::default());
        assert_eq!(&out[0..4], b"RIFF");
        let declared = u32::from_le_bytes([out[4], out[5], out[6], out[7]]) as usize;
        assert_eq!(declared, out.len() - 8);
    }
}
