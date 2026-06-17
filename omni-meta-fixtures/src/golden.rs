//! 黄金样本：真实文件 + exiftool 独立核对的期望（Unified 子集 + raw 标签子集）。
//! 文件由 `samples/regen.sh` 生成；期望值是确定性注入并经 exiftool 读回核对的真相。

use omni_meta::{ContainerSource, DateTimeParts, FileFormat, Gps, IfdKind, Unified, Value};

/// 一条 raw 标签期望（断言「存在且值相等」，容忍额外标签）。
#[derive(Debug, Clone)]
pub enum GoldenRawTag {
    Exif {
        ifd: IfdKind,
        tag: u16,
        value: Value,
    },
    Xmp {
        prefix: &'static str,
        name: &'static str,
        value: &'static str,
    },
    Container {
        source: ContainerSource,
        key: &'static str,
        value: Value,
    },
}

/// 一个黄金样本：真实字节 + 期望格式 + 期望 Unified 子集（None 字段=不约束）+ raw 子集。
pub struct GoldenSample {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub format: FileFormat,
    pub unified: Unified,
    pub raw_subset: Vec<GoldenRawTag>,
}

fn jpeg_exif_gps() -> GoldenSample {
    GoldenSample {
        name: "jpeg_exif_gps",
        bytes: include_bytes!("../samples/jpeg_exif_gps.jpg"),
        format: FileFormat::Jpeg,
        unified: Unified {
            width: Some(64),
            height: Some(48),
            orientation: Some(omni_meta::Orientation::Rotate90),
            camera_make: Some("OmniTest".into()),
            camera_model: Some("GoldenCam".into()),
            created: Some(DateTimeParts {
                year: 2020,
                month: 1,
                day: 2,
                hour: 3,
                minute: 4,
                second: 5,
                tz_offset_min: None,
            }),
            gps: Some(Gps {
                lat_e7: 355_000_000,
                lon_e7: 1_395_000_000,
                alt_mm: None,
            }),
            ..Default::default()
        },
        raw_subset: vec![
            GoldenRawTag::Exif {
                ifd: IfdKind::Primary,
                tag: 0x010F,
                value: Value::Text("OmniTest".into()),
            },
            GoldenRawTag::Exif {
                ifd: IfdKind::Primary,
                tag: 0x0110,
                value: Value::Text("GoldenCam".into()),
            },
        ],
    }
}

fn png_exif() -> GoldenSample {
    // 注：exiftool `-Make=OmniTest` 在 PNG 上写的是 PNG `tEXt`（keyword="Make"，组 [PNG]），
    // 而非 EXIF `eXIf` chunk —— 本文件根本没有 eXIf。exiftool 读回也归类为 [PNG] Make，非 [EXIF]。
    // omni-meta 仅把 eXIf/iTXt(XMP) 视为元数据，按设计忽略 PNG tEXt 关键字，故无 EXIF Make/camera_make。
    // 这是「设计非投影 (C)」而非冲突：文件里确实没有 EXIF。XMP dc:creator 则忠实解析。
    GoldenSample {
        name: "png_exif",
        bytes: include_bytes!("../samples/png_exif.png"),
        format: FileFormat::Png,
        unified: Unified {
            width: Some(80),
            height: Some(60),
            creator: Some("GoldenAuthor".into()),
            ..Default::default()
        },
        raw_subset: vec![GoldenRawTag::Xmp {
            prefix: "dc",
            name: "creator",
            value: "GoldenAuthor",
        }],
    }
}

fn gif_xmp() -> GoldenSample {
    GoldenSample {
        name: "gif_xmp",
        bytes: include_bytes!("../samples/gif_xmp.gif"),
        format: FileFormat::Gif,
        unified: Unified {
            width: Some(48),
            height: Some(32),
            creator: Some("GoldenAuthor".into()),
            ..Default::default()
        },
        raw_subset: vec![GoldenRawTag::Xmp {
            prefix: "dc",
            name: "creator",
            value: "GoldenAuthor",
        }],
    }
}

fn webp_exif() -> GoldenSample {
    GoldenSample {
        name: "webp_exif",
        bytes: include_bytes!("../samples/webp_exif.webp"),
        format: FileFormat::Webp,
        unified: Unified {
            width: Some(72),
            height: Some(54),
            camera_make: Some("OmniTest".into()),
            ..Default::default()
        },
        raw_subset: vec![GoldenRawTag::Exif {
            ifd: IfdKind::Primary,
            tag: 0x010F,
            value: Value::Text("OmniTest".into()),
        }],
    }
}

/// 全部黄金样本。视频样本在后续任务追加。
pub fn golden_corpus() -> Vec<GoldenSample> {
    vec![jpeg_exif_gps(), png_exif(), gif_xmp(), webp_exif()]
}
