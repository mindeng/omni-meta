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

# ---- 视频：精确 2.000s（testsrc 50 帧 @25fps）、64x48、固定创建时间 ----
VID_FILTER="testsrc=duration=2:size=64x48:rate=25"
CT="2020-01-02T03:04:05Z"

# MP4（H.264）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p \
  -metadata creation_time="$CT" mp4.mp4
# MOV（H.264，brand qt）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p -f mov \
  -metadata creation_time="$CT" mov.mov
# MKV（H.264）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libx264 -pix_fmt yuv420p \
  -metadata creation_time="$CT" mkv.mkv
# WebM（VP9，最小）
ffmpeg -y -f lavfi -i "$VID_FILTER" -c:v libvpx-vp9 -b:v 50k webm.webm

# HEIC（best-effort：编码器常缺，失败不致命）
ffmpeg -y -f lavfi -i color=c=red:s=64x48 -frames:v 1 -c:v libx265 heic.heic 2>/dev/null \
  && echo "HEIC 已生成。" || echo "HEIC 跳过（无 libx265/heif 复用器）——见 README 缺口。"

echo "图片样本已生成。"
