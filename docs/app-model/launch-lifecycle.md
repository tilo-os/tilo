# Application launch lifecycle audit

`ui::launch::LaunchSpec` owns the ordinary qOS window lifecycle: common CLI
validation, persistent state, headless driving, deterministic screenshots,
action registration, and the live event loop. It is intentionally not a
generic command dispatcher. Live roots may be argument-free or use explicitly
allow-listed options and a bounded number of positional document arguments;
fallible root construction reports a normal launch error before opening a
window. Ordinary screenshots are represented by a root factory plus a viewport;
apps with an established multi-scenario capture pipeline can retain that
pipeline behind the runner's custom-shot hook. The narrow custom-live hook
keeps standard mode dispatch while allowing an app to install an event-loop
waker or construct one coordinated file-backed state/root pair.

## Adopted

The following 37 applications use the shared lifecycle:

`api_tester`, `backups`, `calendar`, `chat`, `code_editor`, `contacts`,
`containers`, `db_client`, `diffmerge`, `disks`, `ebook`, `feeds`,
`file-explorer`, `firewall`, `hex_editor`, `logs`, `mail`, `maps`, `music`,
`notes`, `photos`, `remote`, `settings`, `sheets`, `slides`, `snippets`,
`store`, `terminal`, `themes`, `totp`, `tube`, `typst_editor`, `video-player`,
`video_editor`, `vpn`, `weather`, and `word-processor`.

Their `main.rs` files now contain only app-specific root/action setup and, where
needed, scenario-specific capture setup. Chat and Contacts use the shared
state-loader hook to retain their idempotent legacy-document imports without
forking live startup. Code accepts a validated positional project, while Notes
uses fallible root construction and an argument-aware in-memory state loader to
keep its workspace-owned persistence and per-peer actor identity. Calendar keeps
its dual-output screenshot pre-dispatch, and Music keeps its non-GUI probe,
relay, and benchmark commands; their actual GUI lifecycles are shared. Terminal
uses the custom-live hook solely to install its PTY waker. Word Processor uses
it to keep file state, actor identity, record routing, and room autosave in one
coordinated construction path. Common argument parsing, mode dispatch,
persistence and live startup are shared; ordinary apps also shed the duplicated
sync/layout/draw/raster pipeline.

## Deliberate exceptions

The remaining entry points were inspected and retain the lower-level APIs for
one of these concrete reasons:

| Application | Why its lifecycle is not a `LaunchSpec` |
| --- | --- |
| `agent` | Headless Cap'n Proto participant with a Clap CLI; it has no window lifecycle. |
| `browser` | Installs a web-engine frame waker and optional render-thread compositor, and has live-network screenshot modes. |
| `calculator` | Runs an input/frame benchmark mode before launch and forces a release-only debug setting. |
| `camera` | Generates fixture files and maps value-taking capture options into a disk/camera source before threads start. |
| `git_client` | Exposes a real, mutating stage-and-commit diagnostic and can emit wide and narrow shots together. |
| `imager` | Emits a whole screenshot suite, probes hardware, and seeds the persistent document in demo mode. |
| `monitor` | Runs interaction benchmarks and drives capture scenarios through multiple frames. |
| `osk` | Installs the input-method bridge and its event-loop waker; it also has a no-bridge diagnostic mode. |
| `pdf_viewer` | Opens a selected PDF into a per-record document and room index rather than the ordinary single store. |

These are extension points for the lower-level `ui::App` and
`ui::headless::FrameHarness`, not missing migrations. If several apps later
share one of these needs (for example positional document opening), that
common lifecycle should be added to `LaunchSpec` once and those apps can move
over together.
