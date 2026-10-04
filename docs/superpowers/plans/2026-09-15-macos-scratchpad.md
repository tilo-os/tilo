# macOS Scratchpad implementation plan

**Goal:** Build and run a small native app using the real UI, FS and actions crates.

**Design:** One persistent note with a multiline editor and Save action. Text edits
use a serializable `tilo-actions` document action through the UI action dispatcher.
Save is an exported effect that writes the Automerge document to FS ContentStore,
flushes that store, and atomically replaces a hash reference. Restart loads that
reference; corrupt or unavailable data produces an error, never a blank replacement.
An exclusive data-directory lock prevents two independent windows overwriting it.

**Scope:** Native standalone macOS app, shared widgets, same tree for headless input
tests and screenshots. FS storage APIs are used directly: native FS mounting is
unavailable on macOS. No claims about a mounted filesystem or live collaboration.

**Files:** `apps/scratchpad/{Cargo.toml,README.md,src/lib.rs,src/storage.rs,src/main.rs}`,
`apps/scratchpad/tests/scratchpad.rs`, workspace membership and lockfile.

## Implementation and verification

- [x] Add workspace package and failing save/reopen integration test. It must type
  Unicode into the real editor, click Save, close, reopen and inspect actual content.
- [x] Implement the serialized edit action, shared widget tree and FS store.
  Export both edit and save actions; propagate save failure to status and receipt.
- [x] Test deterministic action replay, corrupted-reference refusal, save failure
  retaining the previous note, and rejection of concurrent data-directory access.
- [x] Add native entry point, explicit `--data-dir`, and `--shots` using the same
  widget tree. Default storage is in the platform's local data directory.
- [x] Run `cargo test -p tilo-scratchpad`, package clippy, and formatting checks.
- [x] Build and launch the native app, verify a real macOS window, render and inspect
  its screenshot, and document commands and the mounting limitation.

The tests catch missing dispatcher registration, lost edits, unflushed storage,
wrong reference publication order and unsafe fallback after storage corruption.

## Evidence (2026-09-15, Darwin arm64)

- Initial integration test failed at the missing app implementation; the final
  six tests pass. They cover real editor input, action dispatch, FS save/reopen,
  replay equivalence, concurrent opens, corrupt/missing content, save failure,
  and responsive editor geometry.
- The corruption test exposed a panic in the dependency's permissive hash parser.
  The app now validates its 64-character hex reference before parsing.
- Package clippy with `-D warnings` and formatting checks pass.
- `bash apps/scratchpad/build-macos.sh` builds a local `.app`; `codesign --verify
  --strict` and `plutil -lint` pass. CoreGraphics reports its 800×632 native window
  onscreen. The shared-tree 800×600 PNG was inspected after fixing editor width.
- Input was verified through the real framework's headless event driver. macOS
  accessibility automation is unavailable to this session, so OS-injected input
  and VoiceOver behavior are not claimed.
- Independent code review found no actionable findings.
