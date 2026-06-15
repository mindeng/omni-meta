//! 把原始标签投影成统一规范字段。映射规则集中在此，便于测试。

use alloc::vec::Vec;

use crate::model::{Orientation, RawTags, Unified, Value, WarnKind, Warning};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;

/// 把原始标签投影到统一模型。
///
/// 遇到“存在但取值超出规范范围”的标签（如 orientation 不在 1..=8）时，
/// 丢弃该值并向 `warnings` 追加一条 `WarnKind::UnrecognizedValue`，使调用者能
/// 区分“缺失”与“存在但无法识别”。normalize 作用于已解码标签、无字节偏移，
/// 故此类警告的 `offset` 固定为 0。
pub fn normalize(raw: &RawTags, warnings: &mut Vec<Warning>) -> Unified {
    let mut u = Unified::default();
    for t in &raw.exif {
        match (t.tag, &t.value) {
            (TAG_MAKE, Value::Text(s)) => u.camera_make = Some(s.clone()),
            (TAG_MODEL, Value::Text(s)) => u.camera_model = Some(s.clone()),
            (TAG_ORIENTATION, Value::U16(v)) => match Orientation::from_u16(*v) {
                Some(o) => u.orientation = Some(o),
                None => warnings.push(Warning {
                    offset: 0,
                    kind: WarnKind::UnrecognizedValue,
                }),
            },
            _ => {}
        }
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExifTag;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn projects_exif_tags_to_unified() {
        let raw = RawTags {
            exif: Vec::from([
                ExifTag { ifd: 0, tag: 0x010F, value: Value::Text(String::from("Acme")) },
                ExifTag { ifd: 0, tag: 0x0110, value: Value::Text(String::from("X100")) },
                ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(6) },
            ]),
            xmp: Vec::new(),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.camera_make.as_deref(), Some("Acme"));
        assert_eq!(u.camera_model.as_deref(), Some("X100"));
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);
    }

    #[test]
    fn unknown_orientation_value_is_dropped_with_warning() {
        let raw = RawTags {
            exif: Vec::from([ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(99) }]),
            xmp: Vec::new(),
        };
        let mut warnings = Vec::new();
        let u = normalize(&raw, &mut warnings);
        assert_eq!(u.orientation, None);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, WarnKind::UnrecognizedValue);
    }
}
