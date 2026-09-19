# Scratchpad

A small native Tilo app using the UI toolkit, replayable actions, and FS content
storage together. Edit one note, then choose **Save**. Close and reopen the app to
recover the saved note. Unsaved edits are discarded when the window closes.

```sh
cargo run --locked -p scratchpad
```

On macOS, build a locally signed app bundle you can open in Finder:

```sh
bash apps/scratchpad/build-macos.sh
open target/macos/Scratchpad.app
```

This is a local development bundle; it uses the build machine's library paths.

To use a separate scratch directory:

```sh
cargo run --locked -p scratchpad -- --data-dir /tmp/my-scratchpad
```

The default directory is `dirs::data_local_dir()/scratchpad` (on macOS,
`~/Library/Application Support/scratchpad`). Only one instance can open a data
directory at a time. An unreadable saved note stops startup with an error.

## Components

- **UI:** shared multiline TextInput, Button, Text and layout widgets through the
  `tilo-app` facade. Native startup uses the toolkit's scoped winit event loop.
- **Actions:** each committed edit dispatches the exported `scratchpad/edit`
  document action. Save dispatches the exported `scratchpad/save` effect. Both
  use the same resource/action path as the action palette and headless driver.
- **FS:** `fs::store::ContentStore` stores the Automerge document by content hash.
  `current` is an atomically replaced reference to the last successful save.
  This app uses FS storage APIs directly; the repository's macOS configuration
  does not provide native filesystem mounting. It does not connect other peers.

## Verification

```sh
cargo test --locked -p scratchpad
cargo clippy --locked -p scratchpad --all-targets -- -D warnings
cargo run --locked -p scratchpad -- --data-dir /tmp/my-scratchpad --shots /tmp/scratchpad.png
```

The screenshot uses the same app tree and saved content as the live window.

Native performance fixtures use the normal storage and retained widgets:

```sh
cargo run -p scratchpad --example seed-native-fixture -- /tmp/new-scratchpad-fixture
cargo run -p scratchpad -- --data-dir /tmp/new-scratchpad-fixture
```

The seeder refuses every existing destination, writes a deterministic 1,000-line
ASCII body through the application's Save operation, and prints one JSON manifest.
`body_hash`/`body_bytes` describe the baseline; `prefixed_x_hash`/`prefixed_x_bytes`
describe one `x` prepended to it. Hashes are BLAKE3 hex of the UTF-8 body, not of the
Automerge serialization. The seed helper does not launch a GUI.

When the shared native input diagnostic requests a snapshot, Scratchpad reports
`body_hash`, `body_bytes`, `offset_x`, `offset_y`, `editor_focused`, `ready`, and
`root_rect`, `editor_rect`, `scroll_rect` (logical `[x0,y0,x1,y1]` bounds), plus stable
app/editor identifiers. It reads the actual document and shared Scrollable offset;
it does not include the note text or mutate state. This observes input effects,
not pixels or display completion. Normal input and drawing do not request snapshots.
