//! sans-io 核心：解析器只发 Demand、产出 Event，绝不碰 I/O。

use alloc::vec::Vec;

use crate::model::{Field, Warning};

// NeedBytes/Skip/SeekTo 是 sans-io 契约的一部分，由 driver 测试构造、
// 并将由后续的流式/Push 适配器与多格式解析器构造；当前生产路径（JPEG 全缓冲）
// 只产出 Done，故显式允许它们暂未在非测试代码中构造。
#[allow(dead_code)]
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

/// 已定位的元数据载荷种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Exif,
    Xmp,
}

/// 解析过程中增量产出的事件。Payload 借用驱动缓冲，零拷贝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    Payload { kind: PayloadKind, data: &'a [u8] },
    /// 容器原生字段（width/height 等）。
    Field(Field),
    Warning(Warning),
}

/// 一次 pull 的结果：下一步需求 + 本步消耗字节数 + 产出事件。
#[must_use]
pub struct PullResult<'a> {
    pub demand: Demand,
    pub consumed: usize,
    /// 后续计划可换成内联小缓冲 (EventBatch) 以省去每步堆分配；当前用 Vec 保持简单。
    pub events: Vec<Event<'a>>,
}

/// 格式解析器实现的唯一 trait —— 纯状态机。
pub trait MetaParser {
    /// 用当前可见的输入窗口 `input`（驱动从当前逻辑位置起的连续缓冲）推进一步。
    ///
    /// 返回 `PullResult` 包含：下一步 `demand`、本步消耗的字节数 `consumed`
    /// （相对 `input` 起点，不超过 `input.len()`），以及本步产出的 `events`。
    /// `Demand::NeedBytes(n)` 表示当前 `input` 不足以推进，需补足到 ≥ n 字节后再调；
    /// 此时 `consumed` 通常为 0。一旦返回 `Demand::Done`，不应再次调用 `pull`。
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

    #[test]
    fn event_warning_variant_roundtrips() {
        use crate::model::{WarnKind, Warning};
        let ev: Event<'static> = Event::Warning(Warning { offset: 42, kind: WarnKind::Truncated });
        match ev {
            Event::Warning(w) => {
                assert_eq!(w.offset, 42);
                assert_eq!(w.kind, WarnKind::Truncated);
            }
            _ => panic!("expected warning"),
        }
    }
}
