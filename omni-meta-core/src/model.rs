//! 双层数据模型：原始标签 (RawTags) + 统一规范字段 (Unified)。
//! Unified 字段在后续计划中受控增长，每个字段需 >=2 种格式来源才纳入。

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
    Unknown,
}

/// EXIF 方向（标准值 1..=8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Normal,     // 1
    FlipH,      // 2
    Rotate180,  // 3
    FlipV,      // 4
    Transpose,  // 5
    Rotate90,   // 6
    Transverse, // 7
    Rotate270,  // 8
}

impl Orientation {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Orientation::Normal,
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => return None,
        })
    }
}

/// 类型化的标签值（本计划只用到 U16 / Text，后续扩展 Rational/Bytes 等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    U16(u16),
    Text(String),
}

/// 容器原生字段（解析器直接从头部读出，不经 codec）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Width(u32),
    Height(u32),
}

/// 一条 XMP 属性。prefix 为惯用前缀（如 "tiff"），原样保留，不解析命名空间 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpProperty {
    pub prefix: String,
    pub name: String,
    pub value: String,
}

/// 一条原始 EXIF 标签。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExifTag {
    pub ifd: u8,
    pub tag: u16,
    pub value: Value,
}

/// 原始标签层，按命名空间分类（本计划只有 exif，后续加 xmp/iptc/icc/container）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
}

/// 统一规范层。全部 Option —— 缺失即 None，绝不臆造。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unified {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarnKind {
    Truncated,
    BadExifHeader,
    UnreachableSection,
    /// 标签存在但取值超出规范范围（如 orientation 不在 1..=8），已丢弃。
    /// 让调用者能区分”缺失”与”存在但无法识别”。
    UnrecognizedValue,
    /// 压缩块被跳过（本库零依赖、不解压）。
    CompressedChunkSkipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub offset: u64,
    pub kind: WarnKind,
}

/// 顶层解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_maps_known_values() {
        assert_eq!(Orientation::from_u16(1), Some(Orientation::Normal));
        assert_eq!(Orientation::from_u16(2), Some(Orientation::FlipH));
        assert_eq!(Orientation::from_u16(3), Some(Orientation::Rotate180));
        assert_eq!(Orientation::from_u16(4), Some(Orientation::FlipV));
        assert_eq!(Orientation::from_u16(5), Some(Orientation::Transpose));
        assert_eq!(Orientation::from_u16(6), Some(Orientation::Rotate90));
        assert_eq!(Orientation::from_u16(7), Some(Orientation::Transverse));
        assert_eq!(Orientation::from_u16(8), Some(Orientation::Rotate270));
        assert_eq!(Orientation::from_u16(0), None);
        assert_eq!(Orientation::from_u16(9), None);
    }

    #[test]
    fn unified_defaults_to_all_none() {
        let u = Unified::default();
        assert_eq!(u.orientation, None);
        assert_eq!(u.camera_make, None);
        assert_eq!(u.camera_model, None);
    }

    #[test]
    fn unified_has_dimensions_defaulting_none() {
        let u = Unified::default();
        assert_eq!(u.width, None);
        assert_eq!(u.height, None);
    }

    #[test]
    fn rawtags_has_empty_xmp_by_default() {
        let r = RawTags::default();
        assert!(r.xmp.is_empty());
    }

    #[test]
    fn field_and_xmp_property_construct() {
        let f = Field::Width(1920);
        assert_eq!(f, Field::Width(1920));
        let p = XmpProperty {
            prefix: String::from("tiff"),
            name: String::from("Orientation"),
            value: String::from("1"),
        };
        assert_eq!(p.name, "Orientation");
    }
}
