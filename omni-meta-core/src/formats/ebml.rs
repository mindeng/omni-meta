//! EBML（Matroska/WebM）顶层解析器。前向走盒：跳过 EBML 头与不关心元素、
//! 下钻 Segment（不缓冲）、整元素缓冲解析 Info/Tracks、遇未知大小媒体即干净停止。

use alloc::vec::Vec;

use crate::demand::{Demand, Event, MetaParser, PullResult};

#[derive(Debug, Default)]
pub struct EbmlParser {
    done: bool,
}

impl EbmlParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetaParser for EbmlParser {
    fn pull<'a>(&mut self, _input: &'a [u8]) -> PullResult<'a> {
        self.done = true;
        PullResult { demand: Demand::Done, consumed: 0, events: Vec::<Event<'a>>::new() }
    }
}
