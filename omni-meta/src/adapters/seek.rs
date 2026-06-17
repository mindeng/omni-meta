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
                        // 原生 seek 越过 EOF 是合法的、无声的——若就此把整个 k 记为「已跳」，
                        // 越尾区段会被当成 Truncated，与 slice/blocking/push（按真实字节耗尽
                        // 判为 UnreachableSection）分歧。故按文件实长把上报的跳过量夹到 EOF：
                        // 落在文件内则照常跳并上报全量；越尾则只上报真实可跳字节，余量留给
                        // 驱动在 EOF 处判为 UnreachableSection。
                        let target = r.seek(SeekFrom::Current(off)).map_err(|_| Error::Io)?;
                        let end = r.seek(SeekFrom::End(0)).map_err(|_| Error::Io)?;
                        if target <= end {
                            r.seek(SeekFrom::Start(target)).map_err(|_| Error::Io)?;
                            p.skip(k);
                            outcome = p.feed(&[])?;
                        } else {
                            // 越尾：只把真实可达字节记为已跳，余量不存在。就此收尾——
                            // 由 finish() 在 EOF 处对剩余跳过量判为 UnreachableSection，
                            // 与 blocking/push（读到 0 即收尾）一致；继续 feed 会因零进展而死循环。
                            p.skip(k - (target - end));
                            break;
                        }
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
