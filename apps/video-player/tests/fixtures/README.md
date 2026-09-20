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
