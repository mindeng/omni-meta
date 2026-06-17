# 黄金样本

真实小文件，用于以 **exiftool 独立核对**的期望值锚定 omni-meta，打破合成 fixture 的同源偏差。
全部由 `regen.sh` 用 ffmpeg + exiftool 自生成 → 无第三方版权。CI **不**调用本目录脚本；
测试经 `include_bytes!` 读取已入库的二进制（见 `../src/golden.rs`、`../../omni-meta/tests/golden.rs`），
零外部工具依赖。

期望值同时由两条独立证据支撑：①生成期**确定性注入**（已知值）；②`exiftool` **读回核对**（权威真相）。
断言只比 **Unified 子集 + raw 标签子集**（容忍额外标签），与 oracle 口径一致。

## 重生

    bash regen.sh    # 需 ffmpeg + exiftool

## 样本与 exiftool 核对的期望值

| 文件 | 格式 | 尺寸 | 大小 | 关键期望（exiftool 核对） |
|---|---|---|---|---|
| jpeg_exif_gps.jpg | JPEG | 64×48 | 598 B | Make=OmniTest, Model=GoldenCam, Orientation=Rotate90(6), DateTimeOriginal=2020:01:02 03:04:05, GPS=35.5°N/139.5°E（lat_e7=355000000, lon_e7=1395000000） |
| png_exif.png | PNG | 80×60 | 658 B | dc:creator=GoldenAuthor（XMP）。**见缺口①**：Make 落在 PNG `tEXt` 而非 `eXIf`，故仅锚定尺寸 + XMP |
| gif_xmp.gif | GIF | 48×32 | 4057 B | dc:creator=GoldenAuthor（XMP） |
| webp_exif.webp | WebP | 72×54 | 170 B | Make=OmniTest（真实 EXIF / `eXIf` chunk） |
| mp4.mp4 | MP4 | 64×48 | 4857 B | duration_ms=2000, created=2020-01-02T03:04:05Z（UTC） |
| mov.mov | MOV | 64×48 | 4804 B | duration_ms=2000, created=2020-01-02T03:04:05Z（UTC） |
| mkv.mkv | Matroska | 64×48 | 4331 B | duration_ms=2000（created 未 pin，见缺口②） |
| webm.webm | WebM | 64×48 | 8899 B | duration_ms=2000（created 未 pin，见缺口②） |

> 视频时长精确 2.000 s：`testsrc duration=2 rate=25` = 恰 50 帧 → 容器头 duration/timescale 给出精确 2000ms。

## 已知缺口

- **① PNG 样本无 EXIF 锚点**：本机 exiftool 把 `-Make=` 写成 PNG 原生 `tEXt` keyword（字面量 `Make`，
  exiftool 报告组 `[PNG]`），**而非** `eXIf` chunk。需区分两件事：
  - **不投影这个 tEXt「Make」是正确的（非 bug）**：`tEXt` 不是 EXIF，`Make` 也非 PNG 注册关键字，
    没有标准规定「tEXt 里叫 Make 的文本 = 相机 Make」——那是 exiftool 私有约定。把它当 `camera_make`
    即违反「绝不臆造」。EXIF 在 PNG 的标准载体是 `eXIf` chunk，omni-meta 支持之（本样本只是没走标准载体）。
    故 `camera_make=None` 忠实，golden 的 (C) 放松成立。
  - **但「完全不读 tEXt/zTXt」是真实的覆盖缺口（非 correctness bug）**：`tEXt` 的**注册关键字**
    （`Author`/`Copyright`/`Software`/`Creation Time`/`Description`/`Comment`…）有明确语义、确可投影；
    现解析器只认 `IHDR`/`eXIf`/`iTXt`（见 `../../omni-meta-core/src/formats/png.rs:76`），一概不读。
    对**隐私剥离**亦是盲区（tEXt 可含 PII）。已列入 ROADMAP §4 待评估。
  - 本 PNG 样本因此只锚定**尺寸 + XMP `dc:creator`**；EXIF-via-`eXIf` 路径由 JPEG/WebP 样本覆盖。

- **② MKV/WebM `created` 未 pin**：exiftool 未经标准日期标签（CreateDate/MediaCreateDate/TrackCreateDate）
  报出 Matroska 的创建时间，故 `created` 留作不约束（subset 语义下 None=不校验）。`duration_ms` 与尺寸照常锚定。

- **③ HEIC/AVIF 未生成**：本机 ffmpeg 编入了 libx265，但**无 HEIF 复用器**，无法产出 `.heic`。
  `regen.sh` 的 HEIC 段为 best-effort，失败即跳过。HEIF 抽取路径由现有**合成** fixture
  `fixture_bmff_heic`（见 `../src/lib.rs`）兜底，并已纳入四适配器差分 + fuzz 语料。
