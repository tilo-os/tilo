#!/usr/bin/env bash
# Regenerate the tracked real-decode fixture used by crates/platform/tilo-media's integration
# tests. The checked-in MP4 is authoritative for CI; this script documents and
# reproduces its codec, timeline, and GOP contract without private assets.
set -euo pipefail

export LC_ALL=C

for tool in ffmpeg ffprobe; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "missing required fixture generator: $tool" >&2
        exit 1
    }
done

fixture_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=$fixture_dir/h264-aac-48s.mp4
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
candidate=$work_dir/h264-aac-48s.mp4

# testsrc2 changes every frame, making stale-frame assertions meaningful. A
# fixed 250-frame GOP at 30 fps puts keyframes 8.333 seconds apart, so accurate
# and key-unit seeks have observably different outcomes. Constrained Baseline
# H.264 and mono AAC exercise the broadly-supported video and audio paths while
# keeping this 48-second fixture below 1 MiB.
ffmpeg -hide_banner -loglevel error -y -threads 1 \
    -f lavfi -i 'testsrc2=size=320x180:rate=30:duration=48' \
    -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=48' \
    -map 0:v:0 -map 1:a:0 \
    -c:v libx264 -preset slow -profile:v baseline -level:v 3.0 \
    -pix_fmt yuv420p -threads:v 1 \
    -x264-params 'threads=1:lookahead_threads=1:keyint=250:min-keyint=250:scenecut=0:bframes=0' \
    -b:v 120k -maxrate 120k -bufsize 240k \
    -c:a aac -b:a 32k -ac 1 -ar 48000 \
    -movflags +faststart -map_metadata -1 \
    -metadata creation_time=1970-01-01T00:00:00Z \
    -fflags +bitexact -flags:v +bitexact -flags:a +bitexact \
    "$candidate"

video_contract=$(ffprobe -v error -select_streams v:0 \
    -show_entries stream=codec_name,profile,width,height,r_frame_rate \
    -of csv=p=0 "$candidate")
audio_contract=$(ffprobe -v error -select_streams a:0 \
    -show_entries stream=codec_name,sample_rate,channels \
    -of csv=p=0 "$candidate")
duration=$(ffprobe -v error -show_entries format=duration \
    -of default=noprint_wrappers=1:nokey=1 "$candidate")
keyframes=$(ffprobe -v error -select_streams v:0 -skip_frame nokey \
    -show_entries frame=pts_time -of csv=p=0 "$candidate" | tr -d ',\r')

[[ $video_contract == 'h264,Constrained Baseline,320,180,30/1' ]]
[[ $audio_contract == 'aac,48000,1' ]]
[[ $duration == '48.000000' ]]
[[ $keyframes == $'0.000000\n8.333333\n16.666667\n25.000000\n33.333333\n41.666667' ]]

mv -- "$candidate" "$output"
sha256sum "$output"
