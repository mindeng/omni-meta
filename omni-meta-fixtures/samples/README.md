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

- **① PNG 无 EXIF 锚点**：本机 exiftool 把 `-Make=` 写入 PNG 原生 `tEXt` keyword（exiftool 报告组 `[PNG]`），
  而非 `eXIf` chunk。omni-meta 的 PNG 解析器有意只认 `IHDR`/`eXIf`/`iTXt`，故对 tEXt 中的 Make 不投影——
  这是**正确行为**，不是缺陷。因此 PNG 样本只锚定**尺寸 + XMP `dc:creator`**；PNG 的 EXIF-via-`eXIf` 路径
  未被真实样本覆盖（JPEG/WebP 已覆盖 EXIF codec 路径）。如需补强，需让 exiftool 真正写出 `eXIf` chunk
  （`-Make=` 默认路由到 tEXt）。

- **② MKV/WebM `created` 未 pin**：exiftool 未经标准日期标签（CreateDate/MediaCreateDate/TrackCreateDate）
  报出 Matroska 的创建时间，故 `created` 留作不约束（subset 语义下 None=不校验）。`duration_ms` 与尺寸照常锚定。

- **③ HEIC/AVIF 未生成**：本机 ffmpeg 编入了 libx265，但**无 HEIF 复用器**，无法产出 `.heic`。
  `regen.sh` 的 HEIC 段为 best-effort，失败即跳过。HEIF 抽取路径由现有**合成** fixture
  `fixture_bmff_heic`（见 `../src/lib.rs`）兜底，并已纳入四适配器差分 + fuzz 语料。
