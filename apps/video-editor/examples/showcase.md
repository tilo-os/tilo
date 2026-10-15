# Editor capture

`showcase.rs` hosts the production editor, not a mock UI. It supplies a small
three-track document containing a title, a real MP4 and its audio. It renders at
1440 × 810 with `Theme::dark().with_ui_scale(1.5)`; device scale remains 1.
The example does not change the live application's empty starting document.

The project’s unsaved mark sits on the title’s centreline. Track heads show their
names without repeated Video/Audio sublabels; that second line is reserved for
non-default gain and pan. The Mix header leaves an unmeasured peak blank, while
measured levels and clipping warnings remain visible.

The fixture expects a five-second, 1920 × 1080, 30 fps MP4 with audio. The
[Samplelib five-second sample](https://samplelib.com/lib/preview/mp4/sample-5s.mp4)
works. Supply the downloaded file as `VIDEO` below; no third-party footage is
bundled in the repository.

```sh
cargo build --locked -p video-editor --example showcase
# Always put the real catalog beside the executable, including on a builder.
ln -sfn "$PWD/crates/sdk/ui-core/assets/fonts" target/debug/examples/fonts
cargo run --locked -p video-editor --example showcase -- VIDEO target/editor-capture demo
```

The native runtime needs GStreamer decode plugins and `openh264enc` for the demo
encoder, plus a Vulkan adapter (software Vulkan also works). The example waits
for the editor's preview, thumbnails and waveforms to settle before recording.
A missing decoded preview or unfinished visual load fails capture rather than
silently recording a representative fill.

Outputs:

- `video-editor-showcase.png`: the selected footage, title and waveform lanes.
- `video-editor-demo.mp4`: 210 frames at 60 fps (3.5 seconds). Real pointer input
  scrubs the ruler, `S` splits the selected video clip, and the transport's
  frame-forward control previews the result at the project's 30 fps. The
  example asserts that the split reached the stored document.
- `demo-*.png`: four lossless checkpoints. Also extract and inspect frames from
  the final MP4 to verify the encoded result, not just its source frames:

  ```sh
  ffmpeg -i target/editor-capture/video-editor-demo.mp4 \
    -vf 'select=eq(n\,0)+eq(n\,89)+eq(n\,100)+eq(n\,209)' -fps_mode vfr \
    target/editor-capture/encoded-%02d.png
  ```
- `video-editor-light.png` and `video-editor-export.png`: theme and export-sheet
  inspection frames.

The input coordinates are deliberately confined to this fixed-size recording
recipe. Editor painting, hit-testing and actions use the production geometry.
