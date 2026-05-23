# Headless e2e examples

Copy-pastable scripts that drive the nirios compositor headlessly via the `QOS_TEST_CONTROL` unix socket. Use these as starting points when writing automation for AI agents, regression tests, or interactive exploration.

**All examples assume the compositor is running headlessly.** Start it once with:

```bash
./scripts/headless-examples/spawn-headless.sh /tmp/nirios.sock
```

The script spawns `shell --backend headless`, waits for the socket, prints `READY <socket>` to stdout when usable. Tear down with `kill` (or use `Ctrl+C` if you ran it in the foreground).

For background lifetime management:

```bash
./scripts/headless-examples/spawn-headless.sh /tmp/nirios.sock &
COMP_PID=$!
trap "kill $COMP_PID" EXIT
# ... your commands ...
```

## Examples

Each script takes the socket path as its first argument (default `/tmp/nirios.sock`).

| Script | What it shows |
|---|---|
| `list-targets.py` | Enumerate every clickable widget; print names + rects. |
| `click-and-snapshot.py` | Click a named widget, write a PNG of the post-click state. |
| `keyboard-flow.py` | Ctrl+K opens the palette, type text, snapshot. |
| `drag-demo.py` | Press-then-drag a tab handle between two coordinates. |
| `full-loop.py` | Spawn → list → click → key → drag → snapshot → tear down (single file; the most copy-pastable). |
| `qos_client.py` | Library with `NiriosClient` class; `from qos_client import NiriosClient`. Higher-level than the raw socket calls below. |
| `library-demo.py` | Same flow as `full-loop.py` but via `NiriosClient` — shows the library's API. |

Each script is < 50 lines of Python, no external deps (stdlib `socket` + `json` only).

## Easier than this: the `qos-control` CLI

If you don't need the Python scripts, the `qos-control` binary wraps the JSON protocol:

```bash
cargo build -p shell --bin qos-control

qos-control list-targets                  # one row per addressable widget
qos-control click add_workspace           # click named
qos-control snapshot /tmp/out.png         # write PNG
qos-control state-dump --pretty           # WorkspaceTree + Chrome as JSON
qos-control type "hello world"            # type text
qos-control key Enter                     # named key OR single char
qos-control modifiers ctrl,shift          # set modifier mask (empty arg clears)
```

Default socket: `$QOS_TEST_CONTROL` else `/tmp/nirios.sock`. Errors exit non-zero with the structured `code: message` on stderr. Reach for the Python scripts only when you need to compose multiple calls or scrape responses.

## Wire protocol cheat-sheet

The socket accepts newline-delimited JSON. Every command produces exactly one JSON response line.

```json
// Commands (excerpt — see shell/src/compositor/test_control.rs::Command):
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

## When NOT to use this

- **For golden-image / regression tests**: use `cargo run --bin shots -- --mode offscreen --out target/<dir>`. That binary owns ~22 catalogued scenarios with deterministic frame output, no socket dance required.
- **For video walkthroughs**: use `cargo run --bin shots -- --mode recording --scenario <name> --out file.h264`.
- **For looking at the live UI as a human**: use `cargo run --bin shots -- --mode window` (real winit window).

The control socket is for **ad-hoc, programmatic, headless** drives — primarily AI agents and integration tests that need to verify post-action state.

## Further reading

- `docs/headless-testing.md` — full overview of the three e2e layers (shots / socket / headless backend)
- `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md` — design rationale + spec
- `shell/src/compositor/test_control.rs` — the protocol's source of truth (every command + response variant lives in the `Command` and `Response` enums)
- `shell/src/bin/shots/scenario.rs` — pre-written scenarios that exercise the same code paths in-process
