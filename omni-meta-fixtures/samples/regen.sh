#!/usr/bin/env bash
# 重生黄金样本。本机运行，需 ffmpeg + exiftool；产物入库、CI 不调用。
# 所有元数据值是确定性注入的（见 README.md），exiftool 读回仅作交叉核对。
set -euo pipefail
cd "$(dirname "$0")"

# ---- JPEG: 64x48 + EXIF(Make/Model/Orientation/DateTimeOriginal) + GPS ----
ffmpeg -y -f lavfi -i color=c=red:s=64x48 -frames:v 1 jpeg_exif_gps.jpg
exiftool -overwrite_original \
  -Make=OmniTest -Model=GoldenCam -Orientation#=6 \
  -DateTimeOriginal="2020:01:02 03:04:05" \
  -GPSLatitudeRef=N -GPSLatitude=35.5 \
  -GPSLongitudeRef=E -GPSLongitude=139.5 \
  jpeg_exif_gps.jpg

# ---- PNG: 80x60 + eXIf(Make) + XMP(dc:creator) ----
ffmpeg -y -f lavfi -i color=c=green:s=80x60 -frames:v 1 png_exif.png
exiftool -overwrite_original -Make=OmniTest -XMP-dc:Creator=GoldenAuthor png_exif.png

# ---- GIF: 48x32 + XMP(dc:creator) ----
ffmpeg -y -f lavfi -i color=c=blue:s=48x32 -frames:v 1 gif_xmp.gif
exiftool -overwrite_original -XMP-dc:Creator=GoldenAuthor gif_xmp.gif

# ---- WebP: 72x54 + EXIF(Make) ----
ffmpeg -y -f lavfi -i color=c=yellow:s=72x54 -frames:v 1 -c:v libwebp webp_exif.webp
exiftool -overwrite_original -Make=OmniTest webp_exif.webp

echo "图片样本已生成。"
