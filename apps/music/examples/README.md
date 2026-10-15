# Music visual review

`showcase.rs` mounts the production framework surface at **1440 × 810** with
`Theme::dark().with_ui_scale(1.5)`. It also captures a light theme and a narrow
720 × 900 viewport. Fonts are linked to `crates/sdk/ui-core/assets/fonts`; Cargo
prepares the checkout's verified font catalogue.

Run on a headless GPU/audio host through the normal test environment:

```sh
cargo run --locked -p music --example showcase -- target/music-review video
ffmpeg -framerate 60 -i target/music-review/frames/%03d.png \
  -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -an \
  -movflags +faststart target/music-review/music-demo.mp4
```

The optional movie renders 210 native frames (3.5 seconds at 60 fps). A white
arrow follows quadratic curves parameterized by `min_jerk`, opens Night Drive,
presses Play, selects Harbor Lights, and opens the updated Queue. All presses
and releases go through the ordinary input host. The temporary cache contains
original WAV tones of the corresponding tracks' full durations, so the real
cache and audio decoder handle playback without a network. The cache is deleted
when the capture returns. `verification.json` requires the selected track,
decoder-confirmed playing status, and remaining queue contents through the
production accessibility surface. The MP4 intentionally has no audio track.

The original abstract covers for the fictional sample catalogue are generated
from `../src/seed/covers.rs`. No binary assets are committed. These illustrations
use the repository's license and are staged only in screenshot scenarios. Live
missing covers still use the neutral toolkit placeholder; a fresh live library
still starts empty.

Queue and Lyrics remain full-page destinations, with the existing queue-toggle
return behavior and compact transport's More menu. History uses the one app
header, not a second framework toolbar. The full-width transport leaves enough
space for the song, while all spacing and breakpoints derive from theme metrics
and text roles. Keyboard scrolling keeps its scroll indicator visible; pointer
browsing lets the shared scrollbar fade normally.
