//! read_slice：全内存/零拷贝随机访问适配器。

use crate::driver::drive_slice;
use crate::error::Error;
use crate::formats::jpeg::JpegParser;
use crate::limits::Limits;
use crate::model::{FileFormat, Metadata, RawTags};
use crate::normalize::normalize;
use crate::probe::probe;

/// 解析选项。
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub limits: Limits,
}

/// 从一整块内存缓冲解析元数据。无法识别格式时返回 Err。
pub fn read_slice(buf: &[u8], opts: Options) -> Result<Metadata, Error> {
    match probe(buf) {
        FileFormat::Jpeg => {
            let mut parser = JpegParser::new();
            let col = drive_slice(buf, &mut parser, opts.limits);
            let raw = RawTags { exif: col.exif };
            // normalize 可能追加 UnrecognizedValue 警告，故复用 driver 收集的警告向量。
            let mut warnings = col.warnings;
            let unified = normalize(&raw, &mut warnings);
            Ok(Metadata {
                unified,
                raw,
                warnings,
                format: FileFormat::Jpeg,
            })
        }
        FileFormat::Unknown => Err(Error::UnrecognizedFormat),
    }
}
