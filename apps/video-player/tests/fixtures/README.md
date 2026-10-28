# MP4 playback fixture

`h264-aac.mp4` is a generated four-second 160×120 moving-ball video with an AAC
silence track (48 kHz mono). It contains no third-party media. The integration
test requires actual H.264 pictures and decoded PCM at the production audio sink,
then exercises pause, an accurate two-second seek, and resume through app actions.

Generated with GStreamer 1.28.6, OpenH264 and the libav AAC encoder:

```sh
gst-launch-1.0 -e -q mp4mux name=mux ! filesink location=h264-aac.mp4 \
  videotestsrc num-buffers=100 pattern=ball ! \
  video/x-raw,width=160,height=120,framerate=25/1 ! videoconvert ! \
  openh264enc bitrate=128000 gop-size=25 ! h264parse ! queue ! mux.video_0 \
  audiotestsrc num-buffers=188 samplesperbuffer=1024 wave=silence ! \
  audio/x-raw,rate=48000,channels=1 ! audioconvert ! avenc_aac ! \
  aacparse ! queue ! mux.audio_0
```

Playback tests need H.264 and AAC decoders, but do not require these encoders.

# Demo clip

`vp9-aac.mp4` is the clip a capture plays when its launch names no file. It is
a generated four-second 640×360 video, 25 fps, of a white ball that crosses
left to right and is centred at 0:02, where a capture parks it. It has a quiet
440 Hz AAC-LC tone track (48 kHz mono) and contains no third-party media. It is
VP9 Profile 0, 8-bit 4:2:0, which every VP9 decoder plays; `tests/demo_clip.rs`
holds the pin to that.

Generated with FFmpeg 8.1.3 and libvpx; the output is bit-exact, so the recipe
reproduces the pinned SHA-256:

```sh
apps/video-player/tests/fixtures/encode-vp9-aac.sh vp9-aac.mp4
```
