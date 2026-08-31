# Application launch lifecycle audit

`ui::launch::LaunchSpec` owns the ordinary qOS window lifecycle: common CLI
validation, persistent state, headless driving, deterministic screenshots,
action registration, and the live event loop. It is intentionally not a
generic command dispatcher. Live roots may be argument-free or use explicitly
allow-listed options and a bounded number of positional document arguments;
fallible root construction reports a normal launch error before opening a
window. Ordinary screenshots are represented by a root factory plus a viewport;
apps with an established multi-scenario capture pipeline can retain that
pipeline behind the runner's custom-shot hook.

## Adopted

The following 27 applications use the shared lifecycle:

`api_tester`, `backups`, `calendar`, `chat`, `code_editor`, `contacts`,
`containers`, `db_client`, `diffmerge`, `disks`, `feeds`, `file-explorer`,
`firewall`, `hex_editor`, `logs`, `mail`, `maps`, `music`, `notes`, `remote`,
`settings`, `sheets`, `slides`, `snippets`, `themes`, `totp`, and `vpn`.

Their `main.rs` files now contain only app-specific root/action setup and, where
needed, scenario-specific capture setup. Chat and Contacts use the shared
state-loader hook to retain their idempotent legacy-document imports without
forking live startup. Code accepts a validated positional project, while Notes
uses fallible root construction and an argument-aware in-memory state loader to
keep its workspace-owned persistence and per-peer actor identity. Calendar keeps
its dual-output screenshot pre-dispatch, and Music keeps its non-GUI probe,
relay, and benchmark commands; their actual GUI lifecycles are shared. Common argument parsing, mode
dispatch, persistence and live startup are shared; ordinary apps also shed the
duplicated sync/layout/draw/raster pipeline.

## Deliberate exceptions

The remaining entry points were inspected and retain the lower-level APIs for
one of these concrete reasons:

| Application | Why its lifecycle is not a `LaunchSpec` |
| --- | --- |
| `agent` | Headless Cap'n Proto participant with a Clap CLI; it has no window lifecycle. |
| `browser` | Installs a web-engine frame waker and optional render-thread compositor, and has live-network screenshot modes. |
| `calculator` | Runs an input/frame benchmark mode before launch and forces a release-only debug setting. |
| `camera` | Generates fixture files and maps value-taking capture options into a disk/camera source before threads start. |
| `ebook` | Accepts a positional EPUB path and the live/headless root depends on that path. |
| `git_client` | Exposes a real, mutating stage-and-commit diagnostic and can emit wide and narrow shots together. |
| `imager` | Emits a whole screenshot suite, probes hardware, and seeds the persistent document in demo mode. |
| `monitor` | Runs interaction benchmarks and drives capture scenarios through multiple frames. |
| `osk` | Installs the input-method bridge and its event-loop waker; it also has a no-bridge diagnostic mode. |
| `pdf_viewer` | Opens a selected PDF into a per-record document and room index rather than the ordinary single store. |
| `photos` | Opens a selected image/library and stages decoded-image captures after synchronization. |
| `store` | Owns a background Flatpak worker whose event-loop wake and shutdown lifetime is tied to the live app. |
| `terminal` | Owns PTY/session wake integration installed through `run_with_setup`. |
| `tube` | Has extraction/network/media probes, live shots, and benchmark modes in addition to its window. |
| `typst_editor` | Supports an explicit per-shot theme argument and a separate compiled-document capture pipeline. |
| `video-player` | Accepts a media path and pumps asynchronous decode until the first real frame is ready. |
| `video_editor` | Imports and probes media, then pumps asynchronous decode into the same state used for the captured frame. |
| `weather` | Includes a live network probe with its own timeout and response loop. |
| `word-processor` | Opens a positional document, assigns a unique CRDT actor, and installs per-document record routing. |

These are extension points for the lower-level `ui::App` and
`ui::headless::FrameHarness`, not missing migrations. If several apps later
share one of these needs (for example positional document opening), that
common lifecycle should be added to `LaunchSpec` once and those apps can move
over together.
