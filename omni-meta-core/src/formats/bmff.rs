//! ISO-BMFF 顶层解析骨架。本里程碑（A1）只校验首个 box 是 `ftyp` 即 `Done`；
//! `meta`/`moov` 下钻在 A2/A3 引入。沿用既有 sans-io MetaParser 契约。

use alloc::vec::Vec;

use crate::containers::isobmff::read_box_header;
use crate::demand::{Demand, Event, MetaParser, PullResult};

#[derive(Debug, Default)]
pub struct BmffParser {
    done: bool,
}

impl BmffParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for BmffParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
        let events: Vec<Event<'a>> = Vec::new();
        if self.done {
            return PullResult { demand: Demand::Done, consumed: 0, events };
        }
        // 读首个 box 头需 ≥8 字节（largesize 也只需头部，不读 ftyp 载荷）。
        let hdr = match read_box_header(input) {
            Some(h) => h,
            None => {
                return PullResult { demand: Demand::NeedBytes(8), consumed: 0, events };
            }
        };
        // probe 已确保首盒为 ftyp（hdr 仅用于确认头部可完整读出）。
        // A1 不抽取元数据，读到首盒头即完成；box 链续走留给 A2/A3。
        let _ = hdr.kind;
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp_box() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&20u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(b"heic");
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"mif1");
        b
    }

    #[test]
    fn ftyp_then_done_no_events() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert!(res.events.is_empty());
    }

    #[test]
    fn short_input_needs_bytes() {
        let mut p = BmffParser::new();
        let res = p.pull(&[0, 0, 0]); // <8 字节
        assert_eq!(res.demand, Demand::NeedBytes(8));
        assert_eq!(res.consumed, 0);
    }

    #[test]
    fn second_pull_after_done_stays_done() {
        let buf = ftyp_box();
        let mut p = BmffParser::new();
        let _ = p.pull(&buf);
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
    }
}
