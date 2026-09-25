# Scratchpad

One note, kept for you. The whole app is a resource (the note), one action
(edit it) and a view, on the `tilo-app` framework. There is no Save button:
every edit is an action the framework stores, and the status line says when it
is durable.

```sh
cargo run --locked -p scratchpad
```

On macOS, build a locally signed app bundle you can open in Finder:

```sh
bash apps/scratchpad/build-macos.sh
open target/macos/Scratchpad.app
```

This is a local development bundle; it uses the build machine's library paths.

To use a separate profile directory instead of the launcher's:

```sh
cargo run --locked -p scratchpad -- --data-dir /tmp/my-scratchpad
```

Only one instance can open a profile at a time. An unreadable stored note stops
startup with an error; it is never replaced with an empty one.

## What the framework provides

- **Storage and sync:** the note is a record whose text is collaborative;
  concurrent edits merge character by character.
- **Undo and history:** each edit is one undo step, attributed in the record's
  history; History mode previews a past version with the app's own view.
- **Actions and permissions:** `scratchpad.note/edit` is what the editor, the
  Ctrl+K palette, agents and scripts invoke. A viewer's edits are refused.

## Verification

```sh
cargo test --locked -p scratchpad
cargo clippy --locked -p scratchpad --all-targets -- -D warnings
cargo run --locked -p scratchpad -- --shots /tmp/scratchpad.png
```

Screenshots render a private capture profile and never touch your note.

Native performance fixtures use the normal storage and retained widgets:

```sh
cargo run -p scratchpad --example seed-native-fixture -- /tmp/new-scratchpad-fixture
cargo run -p scratchpad -- --data-dir /tmp/new-scratchpad-fixture
```

The seeder refuses every existing destination, writes a deterministic 1,000-line
ASCII body through the app's own edit action and runtime, and prints one JSON
manifest. `body_hash`/`body_bytes` describe the baseline;
`prefixed_x_hash`/`prefixed_x_bytes` describe one `x` prepended to it. Hashes are
BLAKE3 hex of the UTF-8 body, not of the stored document.

When the shared native input diagnostic requests a snapshot, Scratchpad reports
`body_hash`, `body_bytes`, `offset_x`, `offset_y`, `editor_focused`, `ready`, and
`root_rect`, `editor_rect`, `scroll_rect` (logical `[x0,y0,x1,y1]` bounds), plus
stable app/editor identifiers. It reads the stored note and the shared Scrollable
offset; it never includes the note text or mutates state.
