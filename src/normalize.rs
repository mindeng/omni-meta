//! 把原始标签投影成统一规范字段。映射规则集中在此，便于测试。

use crate::model::{Orientation, RawTags, Unified, Value};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;

pub fn normalize(raw: &RawTags) -> Unified {
    let mut u = Unified::default();
    for t in &raw.exif {
        match (t.tag, &t.value) {
            (TAG_MAKE, Value::Text(s)) => u.camera_make = Some(s.clone()),
            (TAG_MODEL, Value::Text(s)) => u.camera_model = Some(s.clone()),
            (TAG_ORIENTATION, Value::U16(v)) => u.orientation = Orientation::from_u16(*v),
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
                ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(6) },
            ]),
        };
        let u = normalize(&raw);
        assert_eq!(u.camera_make.as_deref(), Some("Acme"));
        assert_eq!(u.camera_model, None);
        assert_eq!(u.orientation, Some(Orientation::Rotate90));
    }

    #[test]
    fn unknown_orientation_value_is_dropped() {
        let raw = RawTags {
            exif: Vec::from([ExifTag { ifd: 0, tag: 0x0112, value: Value::U16(99) }]),
        };
        assert_eq!(normalize(&raw).orientation, None);
    }
}
