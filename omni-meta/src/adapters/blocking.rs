//! read_blocking：仅顺序读源（管道/网络流等）。忽略 SkipHint——照常喂，
//! StreamDriver 内部把待跳字节吞掉。

use std::io::Read;

use omni_meta_core::{Error, Metadata, Options, Outcome, PushParser};

const CHUNK: usize = 8192;

pub fn read_blocking<R: Read>(mut r: R, opts: Options) -> Result<Metadata, Error> {
    let mut p = PushParser::new(opts);
    let mut buf = [0u8; CHUNK];
    loop {
        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
        if n == 0 {
            break; // EOF
        }
        if let Outcome::Done = p.feed(&buf[..n])? {
            return p.finish();
        }
    }
    p.finish()
}
