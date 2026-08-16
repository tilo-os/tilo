# Bundled sample media

These files are **stand-ins** for the seven sources in the demo project
(`doc::sample_project`). They exist so the FUSE-media seam
([`src/media.rs`](../src/media.rs)) resolves the sample project's media
references to **local, decodable** files during `--shots` and tests, without
a live `/var/workspace` FUSE mount or shipping large real footage in the
repo.

## How they're used

* In production, a `MediaItem.path` like `media/Drone_Coastline.mov` resolves
  to `/var/workspace/media/Drone_Coastline.mov` on the P2P FUSE — the bytes
  hydrate on demand from peers.
* In `--shots` / dev, the binary sets `QOS_VE_MEDIA_DIR` to **this directory**
  (see `main.rs`), so `media::resolve("media/Drone_Coastline.mov")` finds the
  stand-in here and reports `Hydration::Local`. That exercises the hydration
  branch deterministically.

The two audio stand-ins (`VO_Take3.wav`, `Ambient_Pad.wav`) are **real
16-bit PCM WAV files** (a few seconds each), so the timeline draws a genuine
waveform from their samples in `--shots` — the editor reads audio peaks
straight from PCM with no decoder (see `src/waveform.rs`). The video/image
stand-ins stay tiny text placeholders: the default build's preview shows a
representative composited frame, so they don't need to be real footage.

The default build's preview shows a **representative composited frame**
(graded gradient), so the video stand-ins don't need to be real video. With
`--features decode` the preview seeks a real `media::MediaEngine` (GStreamer
`playbin`) to the clip's source time and blits the true decoded frame; real
footage dropped in here under the same names would then decode.

## Real decode / import / export

* **Import** — the editor's "Import…" action opens the shared XDG file portal
  (`ui::portal`, compiled into every build) and probes each picked file for
  **real** metadata (duration / dimensions / codec / fps) via `src/probe.rs`,
  with no decoder. A picked file becomes a real clip in the bin. `--import PATH`
  seeds one deterministically for `--shots`/tests.
* **Real decode / export** run through GStreamer and are proven against
  genuinely-encoded clips: the `decode` and `gst` tests **generate** a real
  H.264 clip at test time with `gst-launch-1.0 videotestsrc … ! openh264enc !
  … ! filesink`, so no megabyte binaries live in the repo. These stand-ins
  stay tiny and exercise the *reference → resolved-source → hydration-status*
  contract for the default (no-decode) build.

## Why not real footage in-tree?

Real H.264 sources are megabytes each. The seam is what matters: the tests
produce real bytes on demand, and these stand-ins keep the repo small while
proving the resolution contract end to end.
