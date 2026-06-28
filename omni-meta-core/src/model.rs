//! 双层数据模型：原始标签 (RawTags) + 统一规范字段 (Unified)。
//! Unified 字段在后续计划中受控增长，每个字段需 >=2 种格式来源才纳入。

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FileFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
    Heif,
    Avif,
    Mp4,
    Mov,
    Mkv,
    Webm,
    #[default]
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

/// 民用时间戳。容器/EXIF 共用的归一时间表示。
/// `tz_offset_min`:
///   None     = 无时区信息（如 EXIF 本地时间，不臆造）
///   Some(0)  = UTC（如 BMFF moov 的 1904 纪元秒）
///   Some(±n) = UTC±n 分钟（如 EXIF OffsetTime "+09:00" → Some(540)）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTimeParts {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub tz_offset_min: Option<i16>,
}

/// 地理坐标。E7 = 度 × 10^7（±180e7 < i32 上限；Android/Google Location 行业标准定点）。
/// `alt_mm` 高程毫米（正 = 海平面以上）。全整数 → 保留 Eq，无浮点相等脆弱性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gps {
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_mm: Option<i32>,
}

/// 容器原生标签的命名空间来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerSource {
    /// QuickTime `moov/meta/ilst`，键为反向 DNS 全名。
    QuickTimeMdta,
    /// QuickTime `moov/udta` 的 `©`-atoms，键为 FourCC（© → U+00A9）。
    Udta,
}

/// 一条容器原生标签（QuickTime mdta / udta ©-atoms）。复用 `Value` 表示值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerTag {
    pub source: ContainerSource,
    pub key: String,
    pub value: Value,
}

/// 容器原生字段（解析器直接从头部读出，不经 codec）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Width(u32),
    Height(u32),
    /// 媒体时长，毫秒。
    Duration(u64),
    /// 创建时间。
    Created(DateTimeParts),
    /// 地理坐标（容器原生，如 mp4/mov udta/mdta）。
    Gps(Gps),
}

/// 一条 XMP 属性。prefix 为惯用前缀（如 "tiff"），原样保留，不解析命名空间 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpProperty {
    pub prefix: String,
    pub name: String,
    pub value: String,
}

/// PNG 文本块（tEXt/iTXt/zTXt）的一条 keyword→value。
/// keyword 在四种块里都是明文，故始终可读；value 的载体/编码/压缩状态
/// 由 `TextValue` 单一表达——不单设 source 字段（避免与 value 变体冲突）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTag {
    pub keyword: String,
    pub value: TextValue,
}

/// 文本值，自描述其编码与压缩状态。
/// 压缩变体仅保留原始压缩字节（本库零依赖、不解压）；上层可按变体决定
/// 解压后用 Latin-1 还是 UTF-8 解码。未来解压走独立 feature-gated crate。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextValue {
    /// tEXt：Latin-1 已逐字节无损映射为 UTF-8 String（永不失败）。
    Latin1(String),
    /// iTXt 未压缩、非 XMP：原生 UTF-8。
    Utf8(String),
    /// zTXt：zlib 压缩字节，未解压；解压后应按 Latin-1 解码。
    CompressedLatin1(Vec<u8>),
    /// 压缩 iTXt：zlib 压缩字节，未解压；解压后应按 UTF-8 解码。
    CompressedUtf8(Vec<u8>),
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

/// 二进制结构来源候选（无命名空间、parser 权威）：容器结构头与二进制 udta。
/// 由 `driver::Collector` 从 `Event::Field` 累积，作为 normalize 的一类来源传入；
/// 并由 `finalize` 存入 `Metadata.structural` 供 `with_xmp_sidecar` 重投影复用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StructuralFields {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub created: Option<DateTimeParts>,
    pub gps: Option<Gps>,
}

/// 原始标签层，按命名空间分类（本计划只有 exif，后续加 xmp/iptc/icc/container）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTags {
    pub exif: Vec<ExifTag>,
    pub xmp: Vec<XmpProperty>,
    /// 旁挂 `.xmp` sidecar 来源（`Metadata::with_xmp_sidecar` 注入），与内嵌 `xmp` 分列以留 provenance。
    pub xmp_sidecar: Vec<XmpProperty>,
    pub container: Vec<ContainerTag>,
    pub text: Vec<TextTag>,
}

/// 统一规范层。全部 Option —— 缺失即 None，绝不臆造。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unified {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<Orientation>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub duration_ms: Option<u64>,
    pub created: Option<DateTimeParts>,
    pub gps: Option<Gps>,
    pub software: Option<String>,
    pub creator: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub copyright: Option<String>,
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
    /// 剥离时遇歧义/损坏结构，为安全保留该区字节未删。
    StripSkippedAmbiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub offset: u64,
    pub kind: WarnKind,
}

/// 顶层解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    pub unified: Unified,
    pub raw: RawTags,
    pub warnings: Vec<Warning>,
    pub format: FileFormat,
    /// 重投影所需的结构来源快照（内部辅助；内容已全在 `unified` 暴露，故 `pub(crate)`）。
    pub(crate) structural: StructuralFields,
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

    #[test]
    fn datetime_parts_construct_and_eq() {
        let a = DateTimeParts {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            tz_offset_min: Some(0),
        };
        let b = DateTimeParts {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            tz_offset_min: None,
        };
        assert_eq!(a.tz_offset_min, Some(0)); // BMFF: UTC
        assert_eq!(b.tz_offset_min, None); // EXIF 本地: 无时区
        assert_ne!(a, b);
    }

    #[test]
    fn field_has_duration_and_created() {
        let d = Field::Duration(1_501_500);
        assert_eq!(d, Field::Duration(1_501_500));
        let c = Field::Created(DateTimeParts {
            year: 2018,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            tz_offset_min: Some(0),
        });
        assert_ne!(
            c,
            Field::Created(DateTimeParts {
                year: 2019,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                tz_offset_min: Some(0),
            })
        );
    }

    #[test]
    fn unified_has_duration_and_created_defaulting_none() {
        let u = Unified::default();
        assert_eq!(u.duration_ms, None);
        assert_eq!(u.created, None);
    }

    #[test]
    fn fileformat_has_ebml_family() {
        assert_ne!(FileFormat::Mkv, FileFormat::Webm);
        assert_ne!(FileFormat::Mkv, FileFormat::Unknown);
    }

    #[test]
    fn gps_constructs_and_eq() {
        let a = Gps {
            lat_e7: 275_916_000,
            lon_e7: 865_640_000,
            alt_mm: Some(8_850_000),
        };
        let b = Gps {
            lat_e7: 275_916_000,
            lon_e7: 865_640_000,
            alt_mm: None,
        };
        assert_eq!(a.lat_e7, 275_916_000);
        assert_ne!(a, b);
    }

    #[test]
    fn unified_has_gps_defaulting_none() {
        let u = Unified::default();
        assert_eq!(u.gps, None);
    }

    #[test]
    fn field_has_gps_variant() {
        let g = Field::Gps(Gps {
            lat_e7: 1,
            lon_e7: 2,
            alt_mm: None,
        });
        assert_eq!(
            g,
            Field::Gps(Gps {
                lat_e7: 1,
                lon_e7: 2,
                alt_mm: None
            })
        );
    }

    #[test]
    fn container_tag_constructs_and_eq() {
        let a = ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: String::from("com.apple.quicktime.software"),
            value: Value::Text(String::from("13.5.1")),
        };
        let b = ContainerTag {
            source: ContainerSource::QuickTimeMdta,
            key: String::from("com.apple.quicktime.software"),
            value: Value::Text(String::from("13.5.1")),
        };
        assert_eq!(a, b);
        assert_ne!(a.source, ContainerSource::Udta);
    }

    #[test]
    fn rawtags_default_has_empty_container() {
        let r = RawTags::default();
        assert!(r.container.is_empty());
    }

    #[test]
    fn unified_has_software_and_creator_default_none() {
        let u = Unified::default();
        assert!(u.software.is_none());
        assert!(u.creator.is_none());
    }

    #[test]
    fn text_tag_constructs_and_value_variants() {
        let t = TextTag {
            keyword: String::from("Author"),
            value: TextValue::Latin1(String::from("Ada")),
        };
        assert_eq!(t.keyword, "Author");
        assert_eq!(t.value, TextValue::Latin1(String::from("Ada")));
        assert_ne!(
            TextValue::Utf8(String::from("x")),
            TextValue::Latin1(String::from("x"))
        );
        assert_ne!(
            TextValue::CompressedLatin1(alloc::vec![1, 2]),
            TextValue::CompressedUtf8(alloc::vec![1, 2])
        );
    }

    #[test]
    fn rawtags_default_has_empty_text() {
        let r = RawTags::default();
        assert!(r.text.is_empty());
    }

    #[test]
    fn unified_has_title_description_copyright_default_none() {
        let u = Unified::default();
        assert!(u.title.is_none());
        assert!(u.description.is_none());
        assert!(u.copyright.is_none());
    }
}
