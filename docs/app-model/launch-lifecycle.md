# Application launch lifecycle audit

`ui::launch::LaunchSpec` owns the ordinary qOS window lifecycle: common CLI
validation, persistent state, headless driving, deterministic screenshots,
action registration, and the live event loop. It is intentionally not a
generic command dispatcher. An entry point should use it when its live root is
argument-free and its screenshot can be represented by a root factory plus a
viewport.

## Adopted

The following 15 applications use the shared lifecycle:

`api_tester`, `backups`, `containers`, `db_client`, `diffmerge`, `feeds`,
`firewall`, `hex_editor`, `logs`, `mail`, `remote`, `slides`, `snippets`,
`themes`, and `vpn`.

Their `main.rs` files now contain only app-specific root selection and action
registration. Screenshot argument parsing and the duplicated
sync/layout/draw/raster pipeline were removed.

## Deliberate exceptions

The remaining entry points were inspected and retain the lower-level APIs for
one of these concrete reasons:

| Application | Why its lifecycle is not a `LaunchSpec` |
| --- | --- |
| `agent` | Headless Cap'n Proto participant with a Clap CLI; it has no window lifecycle. |
| `browser` | Installs a web-engine frame waker and optional render-thread compositor, and has live-network screenshot modes. |
| `calculator` | Runs an input/frame benchmark mode before launch and forces a release-only debug setting. |
| `calendar` | Supports two screenshot outputs in one invocation plus value-taking layout setup and resize simulation. |
| `camera` | Generates fixture files and maps value-taking capture options into a disk/camera source before threads start. |
| `chat` | Migrates a legacy persistent store and pre-warms decoded attachment images after the first sync. |
| `code_editor` | Accepts a positional project path and stages screenshot interactions after document sync. |
| `contacts` | Migrates a legacy address book and stages dialogs only after the sample document has synced. |
| `disks` | Has live-device and multi-value disk/partition screenshot presets. |
| `ebook` | Accepts a positional EPUB path and the live/headless root depends on that path. |
| `file-explorer` | Opens a positional path and installs filesystem/event-loop wake integration. |
| `git_client` | Exposes a real, mutating stage-and-commit diagnostic and can emit wide and narrow shots together. |
| `imager` | Emits a whole screenshot suite, probes hardware, and seeds the persistent document in demo mode. |
| `maps` | Builds many value-taking map/navigation capture presets before rendering. |
| `monitor` | Runs interaction benchmarks and drives capture scenarios through multiple frames. |
| `music` | Has live media probes, playback/relay modes, and multi-frame media captures. |
| `notes` | Creates a per-peer CRDT actor and author identity before constructing its non-persistent app state. |
| `osk` | Installs the input-method bridge and its event-loop waker; it also has a no-bridge diagnostic mode. |
| `pdf_viewer` | Opens a selected PDF into a per-record document and room index rather than the ordinary single store. |
| `photos` | Opens a selected image/library and stages decoded-image captures after synchronization. |
| `settings` | Uses filename-selected responsive captures and enables focus/appearance reconciliation on the live `App`. |
| `sheets` | Stages menus and editing states after the sample spreadsheet has synced into its document. |
| `store` | Owns a background Flatpak worker whose event-loop wake and shutdown lifetime is tied to the live app. |
| `terminal` | Owns PTY/session wake integration installed through `run_with_setup`. |
| `totp` | Retains the public value-taking `--screen` screenshot interface, which `LaunchSpec` intentionally does not model. |
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
