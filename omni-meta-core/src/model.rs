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
    Heif,
    Avif,
    Mp4,
    Mov,
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

/// 类型化的标签值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    U16(u16),            // SHORT, cnt==1
    U32(u32),            // LONG,  cnt==1
    Text(String),        // ASCII
    Rational(u32, u32),  // RATIONAL  num/den
    SRational(i32, i32), // SRATIONAL num/den
    Bytes(Vec<u8>),      // BYTE / UNDEFINED
    List(Vec<Value>),    // 任意数值类型 cnt>1（如 GPS lat = 3×Rational）
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

/// EXIF IFD 来源标识。raw 层据此记录每条标签所属的 IFD。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfdKind {
    Primary,   // IFD0
    Thumbnail, // IFD1（next-IFD 链）
    Exif,      // 0x8769
    Gps,       // 0x8825
    Interop,   // 0xA005
}

/// 一条原始 EXIF 标签。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExifTag {
    pub ifd: IfdKind,
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
    fn fileformat_has_bmff_family() {
        // 四个 BMFF 家族变体可构造且互不相等。
        let all = [
            FileFormat::Heif,
            FileFormat::Avif,
            FileFormat::Mp4,
            FileFormat::Mov,
        ];
        assert_eq!(all[0], FileFormat::Heif);
        assert_ne!(FileFormat::Heif, FileFormat::Avif);
        assert_ne!(FileFormat::Mp4, FileFormat::Mov);
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
