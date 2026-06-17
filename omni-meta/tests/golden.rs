//! 黄金样本测试：真实文件 → 四适配器一致 + Unified 子集锚定 + raw 子集锚定（exiftool 真相）。

use omni_meta::{Options, RawTags, Unified, read_slice};
use omni_meta_fixtures::{GoldenRawTag, assert_all_equal, golden_corpus};

/// Unified 子集断言：仅校验 expected 中为 Some 的字段，其余不约束。
fn assert_unified_subset(name: &str, exp: &Unified, got: &Unified) {
    macro_rules! chk {
        ($f:ident) => {
            if let Some(ref e) = exp.$f {
                assert_eq!(
                    got.$f.as_ref(),
                    Some(e),
                    "[{name}] unified.{} 不符",
                    stringify!($f)
                );
            }
        };
    }
    chk!(width);
    chk!(height);
    chk!(orientation);
    chk!(camera_make);
    chk!(camera_model);
    chk!(duration_ms);
    chk!(created);
    chk!(gps);
    chk!(software);
    chk!(creator);
}

/// raw 子集断言：每个期望标签须在 raw 中存在且值相等。
fn assert_raw_subset(name: &str, exp: &[GoldenRawTag], raw: &RawTags) {
    for t in exp {
        match t {
            GoldenRawTag::Exif { ifd, tag, value } => {
                let hit = raw
                    .exif
                    .iter()
                    .any(|e| e.ifd == *ifd && e.tag == *tag && &e.value == value);
                assert!(
                    hit,
                    "[{name}] 缺 EXIF 标签 ifd={ifd:?} tag={tag:#06x} value={value:?}\n实际 exif={:?}",
                    raw.exif
                );
            }
            GoldenRawTag::Xmp {
                prefix,
                name: pname,
                value,
            } => {
                let hit = raw
                    .xmp
                    .iter()
                    .any(|p| p.prefix == *prefix && p.name == *pname && p.value == *value);
                assert!(
                    hit,
                    "[{name}] 缺 XMP {prefix}:{pname}={value}\n实际 xmp={:?}",
                    raw.xmp
                );
            }
            GoldenRawTag::Container { source, key, value } => {
                let hit = raw
                    .container
                    .iter()
                    .any(|c| c.source == *source && c.key == *key && &c.value == value);
                assert!(
                    hit,
                    "[{name}] 缺容器标签 {source:?} {key}={value:?}\n实际 container={:?}",
                    raw.container
                );
            }
            GoldenRawTag::Text { keyword, value } => {
                let hit = raw.text.iter().any(|t| {
                    t.keyword == *keyword
                        && matches!(
                            &t.value,
                            omni_meta::TextValue::Latin1(s) | omni_meta::TextValue::Utf8(s)
                            if s == *value
                        )
                });
                assert!(
                    hit,
                    "[{name}] 缺文本标签 {keyword}={value}\n实际 text={:?}",
                    raw.text
                );
            }
        }
    }
}

#[test]
fn golden_samples_anchor_to_exiftool_truth() {
    for s in golden_corpus() {
        assert_all_equal(s.bytes);
        let m = read_slice(s.bytes, Options::default())
            .unwrap_or_else(|e| panic!("[{}] read_slice 失败: {e:?}", s.name));
        assert_eq!(m.format, s.format, "[{}] format 不符", s.name);
        assert_unified_subset(s.name, &s.unified, &m.unified);
        assert_raw_subset(s.name, &s.raw_subset, &m.raw);
    }
}
