//! `Metadata::with_xmp_sidecar`：解析后注入旁挂 .xmp sidecar。

use crate::codecs;
use crate::limits::Limits;
use crate::model::Metadata;
use crate::normalize::normalize;

impl Metadata {
    /// 把一段 `.xmp` sidecar 字节折进已解析结果，返回更新后的 Metadata。
    ///
    /// sidecar 经 XMP codec 解析后落 `raw.xmp_sidecar`，随后基于保留的
    /// `structural` 快照重跑 normalize 重投影 `unified`。`packet` 仅在本次调用内借用。
    /// 空/无效 UTF-8/超 `max_payload_bytes` → 仅追加一条 Truncated 告警、本次无新属性
    /// 注入；`unified` 仍基于已有 `raw`（含历次 sidecar）重投影，故首调常态下等同不变。
    ///
    /// 告警去重：sidecar 来源的 normalize 回退分支均静默，且 normalize 只向 warnings
    /// 追加，故记录注入前长度、重投影后 `truncate` 回退——既复用已有分配，又只保留
    /// XMP 解码自身告警、丢弃重投影告警（它们等同解析期已记录者）。
    pub fn with_xmp_sidecar(mut self, packet: &[u8], limits: Limits) -> Self {
        // 解码告警（truncated/无效 UTF-8/超限）是新增的 → 直接落进 self.warnings；
        // 属性直接追加进 sidecar 列（disjoint 字段借用，无需中转 vec）。
        codecs::xmp::decode(packet, &mut self.raw.xmp_sidecar, &mut self.warnings, &limits);
        // 重投影 Unified；truncate 回退丢弃 normalize 追加的告警（见上）。
        let warn_len = self.warnings.len();
        self.unified = normalize(&self.raw, &self.structural, &mut self.warnings);
        self.warnings.truncate(warn_len);
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::slice::{Options, read_slice};
    use alloc::vec::Vec;

    /// 最小 JPEG：SOI + APP1(Exif TIFF: IFD0 Make="Acme") + EOI。无 description。
    /// （与 driver::make_jpeg_with_exif 类似但仅含 Make、无 Orientation；二者均在
    /// 各自 `#[cfg(test)]` 内，不便共享，故此处自建。）
    fn jpeg_with_make() -> Vec<u8> {
        let mut t: Vec<u8> = Vec::new();
        t.extend_from_slice(b"II");
        t.extend_from_slice(&42u16.to_le_bytes());
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes()); // IFD0 count=1
        t.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
        t.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        t.extend_from_slice(&5u32.to_le_bytes()); // count=5
        t.extend_from_slice(&26u32.to_le_bytes()); // offset
        t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        t.extend_from_slice(b"Acme\0");
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"Exif\0\0");
        body.extend_from_slice(&t);
        let len = (body.len() + 2) as u16;
        let mut j: Vec<u8> = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&body);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    const SIDECAR: &[u8] =
        br#"<rdf:Description xmlns:dc="n" dc:description="Sunset" dc:subject="beach"/>"#;

    #[test]
    fn sidecar_injects_description_and_keeps_provenance() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default())
            .unwrap()
            .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        // 描述字段从 sidecar 注入
        assert_eq!(meta.unified.description.as_deref(), Some("Sunset"));
        // provenance：sidecar 属性落 xmp_sidecar，不污染内嵌 xmp
        assert!(meta.raw.xmp_sidecar.iter().any(|p| p.name == "description"));
        assert!(meta.raw.xmp.iter().all(|p| p.name != "description"));
        // keywords（dc:subject）留 raw 层，Unified 无对应字段
        assert!(meta.raw.xmp_sidecar.iter().any(|p| p.name == "subject"));
    }

    #[test]
    fn with_sidecar_unified_matches_manual_renormalize() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default()).unwrap();
        let merged = meta.clone().with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        // 手动路径：把同样的 sidecar props 预置进 raw 后直接 normalize
        let mut raw = meta.raw.clone();
        let mut w = Vec::new();
        crate::codecs::xmp::decode(
            SIDECAR,
            &mut raw.xmp_sidecar,
            &mut w,
            &crate::limits::Limits::default(),
        );
        let manual = crate::normalize::normalize(&raw, &meta.structural, &mut w);
        assert_eq!(merged.unified, manual); // Unified 字节级一致
    }

    #[test]
    fn empty_sidecar_no_change_no_warning() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default()).unwrap();
        let before_warns = meta.warnings.len();
        let after = meta.clone().with_xmp_sidecar(b"", crate::limits::Limits::default());
        assert_eq!(after.unified, meta.unified); // 空包不改 Unified
        assert_eq!(after.warnings.len(), before_warns); // 空包不增告警
        assert!(after.raw.xmp_sidecar.is_empty());
    }

    #[test]
    fn invalid_utf8_sidecar_warns_truncated_unified_unchanged() {
        use crate::model::WarnKind;
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default()).unwrap();
        let after = meta
            .clone()
            .with_xmp_sidecar(&[0xFF, 0xFE, 0x00], crate::limits::Limits::default());
        assert_eq!(after.unified, meta.unified); // 无效字节不改 Unified
        assert!(after.warnings.iter().any(|w| w.kind == WarnKind::Truncated));
    }

    #[test]
    fn double_sidecar_is_stable() {
        let img = jpeg_with_make();
        let meta = read_slice(&img, Options::default()).unwrap();
        let once = meta
            .clone()
            .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        let twice = once
            .clone()
            .with_xmp_sidecar(SIDECAR, crate::limits::Limits::default());
        assert_eq!(twice.unified, once.unified); // 重复注入同一 sidecar，Unified 稳定
    }
}
