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

The default build's preview shows a **representative composited frame**
(graded gradient), so the stand-ins don't need to be real video. With
`--features decode` the preview feeds the resolved bytes to
`ui::VideoPlayer` (openh264); real footage dropped in here under the same
names would then decode.

## Why not real footage?

Real H.264 sources are megabytes each and the headless CI box has no GPU
decode path enabled by default. The seam is what matters: these prove the
*reference → resolved-source → hydration-status* contract end to end while
keeping the repo small.
