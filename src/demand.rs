//! sans-io 核心：解析器只发 Demand、产出 Event，绝不碰 I/O。

use alloc::vec::Vec;

use crate::model::Warning;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demand {
    /// 需要至少 n 字节才能继续。
    NeedBytes(usize),
    /// 从当前位置向前跳过 n 字节。
    Skip(u64),
    /// 跳到绝对偏移（兜底）。
    SeekTo(u64),
    /// 解析完成。
    Done,
}

/// 已定位的元数据载荷种类（本计划只有 Exif，后续加 Xmp/Iptc/Icc）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Exif,
}

/// 解析过程中增量产出的事件。Payload 借用驱动缓冲，零拷贝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    Warning(Warning),
}

/// 一次 pull 的结果：下一步需求 + 本步消耗字节数 + 产出事件。
pub struct PullResult<'a> {
    pub demand: Demand,
    pub consumed: usize,
    pub events: Vec<Event<'a>>,
}

/// 格式解析器实现的唯一 trait —— 纯状态机。
pub trait MetaParser {
    fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个立刻完成、发一条 Exif 载荷的假解析器，验证 trait 形状可用。
    struct Dummy;
    impl MetaParser for Dummy {
        fn pull<'a>(&mut self, input: &'a [u8]) -> PullResult<'a> {
            let events = alloc::vec![Event::Payload {
                kind: PayloadKind::Exif,
                data: input,
            }];
            PullResult {
                demand: Demand::Done,
                consumed: input.len(),
                events,
            }
        }
    }

    #[test]
    fn parser_can_emit_payload_and_finish() {
        let buf = [1u8, 2, 3];
        let mut p = Dummy;
        let res = p.pull(&buf);
        assert_eq!(res.demand, Demand::Done);
        assert_eq!(res.consumed, 3);
        assert_eq!(res.events.len(), 1);
        match &res.events[0] {
            Event::Payload { kind, data } => {
                assert_eq!(*kind, PayloadKind::Exif);
                assert_eq!(*data, &buf[..]);
            }
            _ => panic!("expected payload"),
        }
    }
}
