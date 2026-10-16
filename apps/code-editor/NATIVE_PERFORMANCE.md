# Native diagnostic fixture and observer

Build the app and its fixture helper, then seed a **new** source path:

```sh
cargo build -p code-editor --bin code-editor --example seed-code-editor-fixture
./target/debug/examples/seed-code-editor-fixture /tmp/new-editor-fixture.rs
./target/debug/code-editor /tmp/new-editor-fixture.rs
```

The helper creates exactly 2,000 deterministic ASCII Rust source lines using
`create_new`; existing files and symlinks are refused. Its stdout is one JSON
manifest with `schema_version`, `hash_algorithm` (`sha256`), `lines`, `editor_id`,
`body_hash`, `body_bytes`, `prefixed_x_hash`, and `prefixed_x_bytes`. Hashes refer
to UTF-8 bytes; the prefixed variant is exactly one `x` before the baseline.
The app uses its production single-file opening, highlighting, and CRDT routes.

The existing shared opt-in native input observer calls the root's
`native_input_snapshot`. It reads the active **durable** buffer, not optimistic
editor text, and exposes:

- `app`, `editor_id`, `buffer_id`, `hash_algorithm`, `body_hash`, `body_bytes`.
- `editor_focused`, zero-based `caret_line`/`caret_column`, `caret_count`, and
  `selection_chars` (columns and selections count characters, not bytes).
- `offset_x` in pixels and `offset_y` in lines; `offset_x_unit`/`offset_y_unit`
  make these units explicit.
- Logical `root_rect` and `editor_rect` arrays `[x0,y0,x1,y1]`, and `ready`.

Missing active text produces null hash/length and `ready=false`. No source text
is logged. Materializing and hashing text costs O(buffer length), only when the
native diagnostic requests a snapshot; account for that observer overhead.

A native collector must establish focus and caret at the buffer beginning using
real input and observed state, then assert every insertion/backspace hash and
caret transition. For scrolling, position the pointer within the editor bounds,
reverse direction before clamping, and assert every offset change while text
hash and buffer identity stay stable. Fixture creation and observer unit tests
do not prove native input routing, displayed pixels, frame rate, or latency.
