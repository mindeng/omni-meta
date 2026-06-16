//! read_push：调用者掌握主动权的 push 适配器（no_std 亦可用）。

use alloc::vec::Vec;

use crate::adapters::slice::Options;
use crate::driver::{finalize, Outcome, StreamDriver};
use crate::error::Error;
use crate::model::{FileFormat, Metadata};
use crate::probe::{parser_for, probe, PROBE_MAX};

/// push 适配器：调用者反复 `feed` 字节、按 `Outcome` 决定下一步，最后 `finish`。
/// 探测格式需要前 PROBE_MAX 字节；在凑齐前 `feed` 累积到内部预缓冲。
pub struct PushParser {
    limits_opts: Options,
    pre: Vec<u8>,
    driver: Option<StreamDriver>,
    format: FileFormat,
    failed: bool,
}

impl PushParser {
    pub fn new(opts: Options) -> Self {
        Self {
            limits_opts: opts,
            pre: Vec::new(),
            driver: None,
            format: FileFormat::Unknown,
            failed: false,
        }
    }

    /// 喂入一块字节（可为空，仅推进），返回下一步 `Outcome`。
    /// 一旦判定格式不可识别，返回 `Err(UnrecognizedFormat)`。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Outcome, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if let Some(d) = self.driver.as_mut() {
            return Ok(d.feed(chunk));
        }
        self.pre.extend_from_slice(chunk);
        let fmt = probe(&self.pre);
        if fmt == FileFormat::Unknown {
            if self.pre.len() >= PROBE_MAX {
                self.failed = true;
                return Err(Error::UnrecognizedFormat);
            }
            return Ok(Outcome::Need(PROBE_MAX - self.pre.len()));
        }
        self.start_driver(fmt)
    }

    /// 调用者已自行向前跳 n 字节后，推进解析器逻辑位置。
    pub fn skip(&mut self, n: u64) {
        if let Some(d) = self.driver.as_mut() {
            d.skip_external(n);
        }
    }

    /// 收尾，返回 Metadata；从未识别出格式则 Err(UnrecognizedFormat)。
    pub fn finish(mut self) -> Result<Metadata, Error> {
        if self.failed {
            return Err(Error::UnrecognizedFormat);
        }
        if self.driver.is_none() {
            let fmt = probe(&self.pre);
            let _ = self.start_driver(fmt);
            if self.failed || self.driver.is_none() {
                return Err(Error::UnrecognizedFormat);
            }
        }
        let driver = self.driver.take().unwrap();
        let col = driver.finish();
        Ok(finalize(col, self.format))
    }

    /// 用已探测格式建驱动；不可识别则置 failed。
    fn start_driver(&mut self, fmt: FileFormat) -> Result<Outcome, Error> {
        match parser_for(fmt.clone(), self.limits_opts.limits) {
            Some(parser) => {
                self.format = fmt;
                let mut d = StreamDriver::new(parser, self.limits_opts.limits);
                let pre = core::mem::take(&mut self.pre);
                let outcome = d.feed(&pre);
                self.driver = Some(d);
                Ok(outcome)
            }
            None => {
                self.failed = true;
                Err(Error::UnrecognizedFormat)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::slice::{read_slice, Options};
    use alloc::vec::Vec;

    fn make_tiff() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&0x010Fu16.to_le_bytes());
        t.extend_from_slice(&2u16.to_le_bytes());
        t.extend_from_slice(&5u32.to_le_bytes());
        t.extend_from_slice(&38u32.to_le_bytes());
        t.extend_from_slice(&0x0112u16.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&6u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"Acme\0");
        t
    }

    fn jpeg_with_exif() -> Vec<u8> {
        let tiff = make_tiff();
        let mut seg_body: Vec<u8> = Vec::new();
        seg_body.extend_from_slice(b"Exif\0\0");
        seg_body.extend_from_slice(&tiff);
        let len = (seg_body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg_body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    /// 以固定 chunk 大小喂完整字节，忽略 SkipHint（driver 内部吞掉）。
    fn push_drive(bytes: &[u8], opts: Options, chunk: usize) -> Result<crate::model::Metadata, crate::error::Error> {
        let mut p = PushParser::new(opts);
        let mut i = 0;
        while i < bytes.len() {
            let end = (i + chunk).min(bytes.len());
            if let Outcome::Done = p.feed(&bytes[i..end])? {
                return p.finish();
            }
            i = end;
        }
        p.finish()
    }

    #[test]
    fn push_matches_slice_various_chunks() {
        let j = jpeg_with_exif();
        let want = read_slice(&j, Options::default()).unwrap();
        for chunk in [1usize, 3, 7, j.len()] {
            let got = push_drive(&j, Options::default(), chunk).unwrap();
            assert_eq!(got, want, "chunk={chunk}");
        }
    }

    #[test]
    fn push_unrecognized_errors() {
        let r = push_drive(&[0x00, 0x01, 0x02], Options::default(), 1);
        assert!(r.is_err());
    }

    #[test]
    fn push_skip_via_caller_seek_equivalent() {
        // 含非元数据段的 JPEG：调用者响应 SkipHint 自行 seek + skip。
        let tiff = make_tiff();
        let mut app1: Vec<u8> = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let app1_len = (app1.len() + 2) as u16;
        // 大的非元数据段 APP0（body 100 字节）放在 APP1 之前
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]);
        j.extend_from_slice(&[0xFF, 0xE0]);
        j.extend_from_slice(&102u16.to_be_bytes()); // body 100
        j.extend_from_slice(&[0u8; 100]);
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&app1_len.to_be_bytes());
        j.extend_from_slice(&app1);
        j.extend_from_slice(&[0xFF, 0xD9]);

        let want = read_slice(&j, Options::default()).unwrap();

        // 模拟 seek：维护一个"源游标"，SkipHint 时直接前移并 skip。
        let mut p = PushParser::new(Options::default());
        let mut src = 0usize;
        let mut outcome = p.feed(&[]).unwrap();
        loop {
            match outcome {
                Outcome::Done => break,
                Outcome::SkipHint(k) => {
                    src += k as usize; // 源级 seek 前移
                    p.skip(k);
                    outcome = p.feed(&[]).unwrap();
                }
                Outcome::Need(_) => {
                    if src >= j.len() {
                        break;
                    }
                    let end = (src + 4).min(j.len());
                    let chunk = &j[src..end];
                    src = end;
                    outcome = p.feed(chunk).unwrap();
                }
            }
        }
        let got = p.finish().unwrap();
        assert_eq!(got, want);
    }
}
