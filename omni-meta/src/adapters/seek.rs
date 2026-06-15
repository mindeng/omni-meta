//! read_seek：可 Seek 源。SkipHint 时原生向前 seek 省 I/O（不读跳过的字节）。

use std::io::{Read, Seek, SeekFrom};

use omni_meta_core::{Error, Metadata, Options, Outcome, PushParser};

const CHUNK: usize = 8192;

pub fn read_seek<R: Read + Seek>(mut r: R, opts: Options) -> Result<Metadata, Error> {
    let mut p = PushParser::new(opts);
    let mut buf = [0u8; CHUNK];
    let mut outcome = p.feed(&[])?; // 取得首个需求（探测需 2 字节）
    loop {
        match outcome {
            Outcome::Done => break,
            Outcome::SkipHint(k) => {
                // 巨量跳跃用 i64 可能溢出：超界则回退为读弃（照常 feed）。
                match i64::try_from(k) {
                    Ok(off) => {
                        r.seek(SeekFrom::Current(off)).map_err(|_| Error::Io)?;
                        p.skip(k);
                        outcome = p.feed(&[])?;
                    }
                    Err(_) => {
                        let n = r.read(&mut buf).map_err(|_| Error::Io)?;
                        if n == 0 {
                            break;
                        }
                        outcome = p.feed(&buf[..n])?;
                    }
                }
            }
            Outcome::Need(_) => {
                let n = r.read(&mut buf).map_err(|_| Error::Io)?;
                if n == 0 {
                    break;
                }
                outcome = p.feed(&buf[..n])?;
            }
        }
    }
    p.finish()
}
