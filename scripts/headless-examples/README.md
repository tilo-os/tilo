# Headless e2e examples

Scripts for controlling the headless compositor through the `TILO_TEST_CONTROL` Unix socket.

For app automation, begin with `tilo-control actions list QUERY --json`, then
invoke the chosen `(provider, id)`. The [search selection guide](../../docs/headless-testing.md#select-search-results-by-identity)
explains how to distinguish same-named native and host apps and verify execution
without opening the palette. Use the input examples below when testing input or
rendering itself.

## Proofs

Two proofs start a headless session of their own, on a scratch home, and read
every claim back off that session rather than off the thing that asked for it.
`tools/ci/session-proofs.sh` runs both, and the nightly workflow runs that.

| Proof | What it asserts |
|---|---|
| `agent-seats-proof.py` | Two agents at the session's agent door, each admitted as itself through the workspace's governance, each served its own tools, the desk's Stop ending one of their connections alone. |
| `session-lock-proof.py` | The desk's own lock action locks the session, the lock screen's own frame covers the desk, a password that is not the person's is refused, and the person's own password brings the desk back. |

`agent-files-story.py` drives a desktop with the tree's own apps as an agent
does, through the session's agent door and nothing else: it names Files as the
listing does and starts it by one of its actions, finds the device and folder
Files' changes run in from Files' own read, renames a file with
`files.folder#rename` and reads the new name off the disk, sends the file into
a Chat conversation with Chat's `send`, and turns do not disturb on until the
bar's bell says so. The person only answers the sheets that ask about the agent.

`notes-persist-proof.py` is the same kind of proof for a desktop with the
tree's own apps: it opens Notes, types into a page, stops the session and starts
it again, and reads the text back off the record on disk. It needs the session
and its apps built (`dist/dev/nested-session-build.sh`) and so is not part of
`tools/ci/session-proofs.sh`.

`notes-caret-proof.py` opens Notes the same way, types a page with no input
method running, and checks that the desk holds the body's caret, that it follows
the typing, and that Super+V, pressed the moment the caret moves, opens the
clipboard pane under that caret's line, its first frame as tall as its page.

For examples that connect to an existing compositor, start it once:

```bash
python3 scripts/headless-examples/real_apps.py --start --keep --state ~/tilo-headless
```

It prints `{control, runtime, state, pid}` once the desktop answers: pass `control`
as a script's first argument, or export it as `TILO_TEST_CONTROL` so every
script and `tilo-control` finds the session by itself. It starts
`dist/dev/nested-session.sh` (the tree's apps installed as signed packages and
launched through the desktop's own launcher) on a private home, runtime
directory and throwaway identity, so your real identity and workspaces are
never touched. Stop a kept session with `kill -TERM -<pid>`, which stops its
apps, services and bus too.

`cargo headless` is the same session from the tree's build with no window, and
prints how to drive it; `--ready FILE` writes the same JSON. A harness that has
to drive a build of its own (this tree's debug binaries, a release session a
performance lane compiled) starts it through `real_apps.ScriptedSession`, which
keeps the same rules: a scratch profile, the runtime directory
`dist/dev/nested-session.sh --print-runtime` names for it, and nothing left
behind.

## Examples

Each script drives a running session: the socket path is its first argument, else `$TILO_TEST_CONTROL`, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`. `real_apps.py` is the client they all use (`Session.attach` for a session another tool started, `ScriptedSession` for the tree's own build on a scratch profile, `Agent` for the session's agent door).

| Script | What it shows |
|---|---|
| `list-targets.py` | Enumerate every clickable widget; print names + rects. |
| `click-and-snapshot.py` | Click a named widget, write a PNG of the post-click state. |
| `keyboard-flow.py` | Ctrl+K opens the palette, type text, snapshot. |
| `drag-demo.py` | Press-then-drag a tab handle between two coordinates. |
| `full-loop.py` | Spawn a session of this tree's build → open search → type → snapshot → close → tear down. |
| `real_apps.py` | A headless session with the tree's own apps installed, launched through the desktop's own launcher: `Session` for a check, `--start --open notes --shot out.png` for a person. Every check of a desktop with apps starts here. |
| `files-clipboard-e2e.py` | Copy and cut files in Files, paste them into another folder through the session's file clipboard, and read the bytes back off disk. |

The input examples use the Python standard library. Native protocol proofs below
have their own explicit external prerequisites.

## Native protocol journeys

`startup-frame-timeline.py` launches a synthetic native SDK payload through a
private registered desktop action and the production net-off sandbox. It records
SDK presentation events, compositor-painted frame observation bounds and a live
frame trace. It does not measure a physical display or bypass ordinary OS
enrollment.

## `tilo-control` CLI

Use `tilo-control` from the command line:

```bash
cargo build -p tilo-control

tilo-control list-targets                  # one row per addressable widget
tilo-control click add_workspace           # click named
tilo-control snapshot /tmp/out.png         # write PNG
tilo-control state-dump --pretty           # WorkspaceTree + Chrome as JSON
tilo-control type "hello world"            # type text
tilo-control key Enter                     # named key OR single char
tilo-control modifiers ctrl,shift          # set modifier mask (empty arg clears)
```

Default socket: `$TILO_TEST_CONTROL` else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`. Errors exit non-zero with `code: message` on stderr.

## Wire protocol cheat-sheet

The socket accepts newline-delimited JSON. Every command produces exactly one JSON response line.

```json
// Commands (excerpt — see crates/session/session/src/test_control.rs::Command):
{"op": "list_targets"}
{"op": "click_at",     "x": 400, "y": 300}
{"op": "click_named",  "name": "command_palette_input"}
{"op": "drag",         "x_from": 100, "y_from": 100, "x_to": 300, "y_to": 200, "steps": 12}
{"op": "key",          "key": "Enter"}                   // or "a", "ArrowLeft", "Escape", etc.
{"op": "type",         "text": "hello"}
{"op": "modifiers",    "ctrl": true, "shift": false, "alt": false, "logo": false}
{"op": "snapshot",     "path": "/tmp/state.png"}

// Responses:
{"targets": [{"name": "...", "rect": [x, y, w, h]}, ...]}
{"ok":      {"frame": 0}}
{"err":     {"code": "unknown_target", "message": "..."}}
```

Error codes that show up in normal use:
- `parse_error` — malformed JSON or unknown `op`
- `unknown_target` — `click_named` resolved to nothing (or a zero-area widget)
- `invalid_path` — `snapshot` path is empty
- `not_implemented` — placeholder for in-flight phases

## Offscreen rendering and recordings

- **For golden-image / regression tests**: use `cargo run -p tilo-session --features hosted-test-control,test-support --bin shots -- --mode offscreen --out target/<dir>` for deterministic scenario output.
- **For video walkthroughs**: use `cargo run -p tilo-session --features hosted-test-control,test-support --bin shots -- --mode recording --scenario <name> --out file.h264`.
- **For a windowed preview**: use `cargo run -p tilo-session --features hosted-test-control,test-support --bin shots -- --mode window`.

## Further reading

- `docs/headless-testing.md` — the maintained overview and protocol guide for the three e2e layers (shots / socket / headless backend)
- `crates/session/session/src/test_control.rs` — `Command` and `Response` protocol definitions
- `crates/session/session/src/test_control.rs` — pre-written scenarios that exercise the same code paths in-process
