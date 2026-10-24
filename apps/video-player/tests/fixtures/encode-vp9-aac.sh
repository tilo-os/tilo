#!/bin/sh
# Generated demo clip: 640x360, 25 fps, 4 s; a white ball crosses left to
# right and is centred at 0:02. VP9 Profile 0 (8-bit 4:2:0) + AAC-LC mono.
set -eu
out=${1:-vp9-aac.mp4}
ffmpeg -hide_banner -loglevel error -y \
 -f lavfi -i "color=c=black:s=640x360:r=25:d=4,format=yuv420p,geq=lum='16+219*clip(30.5-hypot(X-(320+(T-2)*100)\,Y-180)\,0\,1)':cb=128:cr=128" \
 -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4,volume=0.1" \
 -map 0:v -map 1:a -c:v libvpx-vp9 -profile:v 0 -pix_fmt yuv420p -b:v 0 -crf 32 -row-mt 1 -g 25 -threads 4 \
 -c:a aac -ac 1 -b:a 64k -shortest -map_metadata -1 -fflags +bitexact -flags:v +bitexact -flags:a +bitexact -movflags +faststart \
 "$out"
