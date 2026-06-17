//! PNG 剥离 walker：逐 chunk 遍历，eXIf/XMP-iTXt/文本块 Drop、iCCP 视选项、
//! 关键/图像 chunk Emit。keep_orientation 时在 IDAT 前 Insert 合成 eXIf。

use alloc::vec::Vec;

use super::exif_synth::{orientation_tiff, png_exif_chunk};
use super::jpeg::find_orientation_pub as find_orientation;
use super::{RemovedKind, StripCmd, StripDemand, StripOptions, StripPlanner, StripResult};

const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

pub struct PngStripper {
    opts: StripOptions,
}

impl PngStripper {
    pub fn new(opts: StripOptions) -> Self {
        Self { opts }
    }
}

impl StripPlanner for PngStripper {
    fn pull(&mut self, input: &[u8]) -> StripResult {
        let mut cmds: Vec<StripCmd> = Vec::new();
        if input.len() < 8 || input[..8] != SIG {
            if !input.is_empty() {
                cmds.push(StripCmd::Emit(input.len()));
            }
            return StripResult { demand: StripDemand::Done, consumed: input.len(), cmds };
        }
        cmds.push(StripCmd::Emit(8)); // 签名
        let mut pos = 8usize;
        let mut synth_orientation: Option<u16> = None;
        let mut synth_inserted = false;

        loop {
            if pos + 8 > input.len() {
                if pos < input.len() {
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                }
                break;
            }
            let len = u32::from_be_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]]) as usize;
            let ctype = &input[pos + 4..pos + 8];
            let total = match 8usize.checked_add(len).and_then(|v| v.checked_add(4)) {
                Some(v) if pos.checked_add(v).map(|e| e <= input.len()).unwrap_or(false) => v,
                _ => {
                    cmds.push(StripCmd::Emit(input.len() - pos));
                    pos = input.len();
                    break;
                }
            };
            let data = &input[pos + 8..pos + 8 + len];
            let is_iend = ctype == b"IEND";
            let is_idat = ctype == b"IDAT";

            // 合成 eXIf：在首个 IDAT（或 IEND 兜底）之前注入一次。
            if !synth_inserted && (is_idat || is_iend) {
                if let Some(val) = synth_orientation {
                    cmds.push(StripCmd::Insert(png_exif_chunk(&orientation_tiff(val))));
                }
                synth_inserted = true;
            }

            let drop_kind = classify(ctype, data, &self.opts);
            match drop_kind {
                Some((kind, is_exif)) => {
                    if is_exif && self.opts.keep_orientation && synth_orientation.is_none() {
                        synth_orientation = find_orientation(data);
                    }
                    cmds.push(StripCmd::Drop { len: total, kind });
                }
                None => {
                    cmds.push(StripCmd::Emit(total));
                }
            }
            pos += total;
            if is_iend {
                break;
            }
        }

        StripResult { demand: StripDemand::Done, consumed: pos, cmds }
    }
}

fn classify(ctype: &[u8], data: &[u8], opts: &StripOptions) -> Option<(RemovedKind, bool)> {
    match ctype {
        b"eXIf" => Some((RemovedKind::Exif, true)),
        b"iTXt" | b"tEXt" | b"zTXt" => {
            if data.starts_with(b"XML:com.adobe.xmp") {
                Some((RemovedKind::Xmp, false))
            } else {
                Some((RemovedKind::Other, false))
            }
        }
        b"iCCP" => {
            if opts.keep_icc { None } else { Some((RemovedKind::Icc, false)) }
        }
        _ => None, // IHDR/PLTE/IDAT/IEND 等保留
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strip::{drive_strip_slice, RemovedKind, StripOptions};
    use crate::model::FileFormat;

    const SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    fn chunk(ctype: &[u8; 4], data: &[u8]) -> alloc::vec::Vec<u8> {
        let mut c = alloc::vec::Vec::new();
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(ctype);
        c.extend_from_slice(data);
        let mut crc_in = alloc::vec::Vec::new();
        crc_in.extend_from_slice(ctype);
        crc_in.extend_from_slice(data);
        c.extend_from_slice(&super::super::crc32::crc32(&crc_in).to_be_bytes());
        c
    }

    fn ihdr(w: u32, h: u32) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec::Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &d)
    }

    fn itxt_xmp(packet: &[u8]) -> alloc::vec::Vec<u8> {
        let mut d = alloc::vec::Vec::new();
        d.extend_from_slice(b"XML:com.adobe.xmp");
        d.extend_from_slice(&[0, 0, 0, 0, 0]); // kw nul, compflag, compmethod, lang nul, transkw nul
        d.extend_from_slice(packet);
        chunk(b"iTXt", &d)
    }

    fn full_png() -> alloc::vec::Vec<u8> {
        let mut p = alloc::vec::Vec::new();
        p.extend_from_slice(&SIG);
        p.extend_from_slice(&ihdr(8, 8));
        p.extend_from_slice(&chunk(b"eXIf", &crate::strip::exif_synth::orientation_tiff(6)));
        p.extend_from_slice(&itxt_xmp(br#"<rdf:Description tiff:Make="Acme"/>"#));
        p.extend_from_slice(&chunk(b"iCCP", b"prof\0\0somedata"));
        p.extend_from_slice(&chunk(b"IDAT", &[1, 2, 3, 4]));
        p.extend_from_slice(&chunk(b"IEND", &[]));
        p
    }

    fn run(buf: &[u8], opts: StripOptions) -> (alloc::vec::Vec<u8>, crate::strip::StripReport) {
        let mut p = PngStripper::new(opts);
        drive_strip_slice(buf, &mut p, FileFormat::Png)
    }

    #[test]
    fn default_strips_exif_xmp_keeps_icc_and_orientation() {
        let (out, report) = run(&full_png(), StripOptions::default());
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.xmp.is_empty());
        assert_eq!(meta.unified.orientation, Some(crate::model::Orientation::Rotate90));
        assert_eq!(meta.unified.width, Some(8));
        assert!(report.removed.contains(RemovedKind::Exif));
        assert!(report.removed.contains(RemovedKind::Xmp));
        assert!(out.windows(4).any(|w| w == b"iCCP")); // ICC 保留
        assert!(out.windows(4).any(|w| w == b"IDAT")); // 图像保留
        assert!(out.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn aggressive_strips_icc_and_orientation() {
        let (out, report) = run(&full_png(), StripOptions::aggressive());
        assert!(!out.windows(4).any(|w| w == b"iCCP"));
        assert!(report.removed.contains(RemovedKind::Icc));
        let meta = crate::read_slice(&out, crate::Options::default()).unwrap();
        assert!(meta.raw.exif.is_empty());
        assert_eq!(meta.unified.orientation, None);
    }

    #[test]
    fn synthesized_exif_chunk_is_valid_and_reparses() {
        let (out, _r) = run(&full_png(), StripOptions::default());
        assert!(out.windows(4).any(|w| w == b"eXIf"));
    }

    #[test]
    fn non_png_returns_input_unchanged() {
        let buf = [0u8, 1, 2, 3];
        let (out, _r) = run(&buf, StripOptions::default());
        assert_eq!(out, buf);
    }
}
