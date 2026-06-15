//! read_slice：全内存/零拷贝随机访问适配器。

use crate::driver::{drive_slice, finalize};
use crate::error::Error;
use crate::limits::Limits;
use crate::model::Metadata;
use crate::probe::{parser_for, probe};

/// 解析选项。
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub limits: Limits,
}

/// 从一整块内存缓冲解析元数据。无法识别格式时返回 Err。
pub fn read_slice(buf: &[u8], opts: Options) -> Result<Metadata, Error> {
    let fmt = probe(buf);
    match parser_for(fmt.clone()) {
        Some(mut parser) => {
            let col = drive_slice(buf, parser.as_mut(), opts.limits);
            Ok(finalize(col, fmt))
        }
        None => Err(Error::UnrecognizedFormat),
    }
}
