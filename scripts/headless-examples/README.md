# Headless e2e examples

Scripts for controlling the headless compositor through the `TILO_TEST_CONTROL` Unix socket.

For app automation, begin with `tilo-control actions list QUERY --json`, then
invoke the chosen `(provider, id)`. The [search selection guide](../../docs/headless-testing.md#select-search-results-by-identity)
explains how to distinguish same-named native and host apps and verify execution
without opening the palette. Use the input examples below when testing input or
rendering itself. `action-api-demo.py` and `app-launch-proof.py` start their own
isolated sessions and exercise semantic invocation. `agent-seats-proof.py`
starts an `agent-control` session and drives it from two MCP sessions at once:
each agent's strip, screenshots and held keys stay its own.

For examples that connect to an existing compositor, start it once with:

```bash
./scripts/headless-examples/spawn-headless.sh
```

The socket defaults to `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`; pass a path to use another (keep it in a directory only you can write, never `/tmp`). The script refuses a socket a live compositor still answers on. It spawns `shell --backend headless` against a scratch profile — its own HOME, XDG homes, device identity and filesystem-daemon handoff directories, so your real identity and workspaces are never touched — waits for the socket, and prints `READY <socket>` to stdout when usable. Tear down with `kill` (or use `Ctrl+C` if you ran it in the foreground); the scratch profile goes with it.

`TILO_HEADLESS_TIMEOUT` sets how long to wait for the socket, in seconds (default 60). `TILO_HEADLESS_PROFILE=DIR` keeps a scratch profile in `DIR` across runs instead of a fresh one each time.

For background lifetime management:

```bash
./scripts/headless-examples/spawn-headless.sh &
COMP_PID=$!
trap "kill $COMP_PID" EXIT
# ... your commands ...
```

## Examples

Each script takes the socket path as its first argument (default `$TILO_TEST_CONTROL`, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`).

| Script | What it shows |
|---|---|
| `list-targets.py` | Enumerate every clickable widget; print names + rects. |
| `click-and-snapshot.py` | Click a named widget, write a PNG of the post-click state. |
| `keyboard-flow.py` | Ctrl+K opens the palette, type text, snapshot. |
| `drag-demo.py` | Press-then-drag a tab handle between two coordinates. |
| `full-loop.py` | Spawn → list → click → key → drag → snapshot → tear down. |
| `tilo_client.py` | Library with `TiloClient` class; `from tilo_client import TiloClient`. Higher-level than the raw socket calls below. |
| `library-demo.py` | Same flow as `full-loop.py` but via `TiloClient` — shows the library's API. |

The input examples use the Python standard library. Native protocol proofs below
have their own explicit external prerequisites.

## Native protocol journeys

`airdrop-transfer-proof.py` boots its own isolated compositor, signed thin Files
package and pinned OpenDrop loopback receiver. It exercises actual Files actions,
trusted fingerprint entry, status/progress, cancellation and failure recovery.
`cast-session-proof.py` drives the real prebuilt projector service, through its
control socket, against a bounded TLS Cast receiver, checking media LOAD,
tokenized HLS isolation and clean stop.

These proofs have explicit prerequisites and fail when they cannot observe the
subject. They never build, configure radios or use the user's desktop/accounts.
See [continuity interoperability](../../docs/architecture/continuity-interop.md#native-journey-evidence-and-reproduction)
for build inputs, pinned external dependencies and qualification limits.

`native-pair-journey.py` checks two certified native Chat peers with private
synthetic audio. It currently fails at governed app creation and writes an
explicitly unqualified receipt. See [native Chat pair acceptance](../../docs/native-chat-pair-acceptance.md)
for remote build inputs, observed blockers and the steps that have not executed.

`startup-frame-timeline.py` launches a synthetic native SDK payload through a
private registered desktop action and the production net-off sandbox. It records
SDK presentation events, compositor-painted frame observation bounds and a live
frame trace. It does not measure a physical display or bypass ordinary OS
enrollment. See [native first-frame measurements](../../docs/benchmarks/native-first-frame-2026-10-06.md)
for the remote wrapper, build inputs, raw samples and cache limitations.

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
// Commands (excerpt — see crates/session/shell/src/compositor/test_control.rs::Command):
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

- **For golden-image / regression tests**: use `cargo run --bin shots -- --mode offscreen --out target/<dir>` for deterministic scenario output.
- **For video walkthroughs**: use `cargo run --bin shots -- --mode recording --scenario <name> --out file.h264`.
- **For a windowed preview**: use `cargo run --bin shots -- --mode window`.

## Further reading

- `docs/headless-testing.md` — the maintained overview and protocol guide for the three e2e layers (shots / socket / headless backend)
- `crates/session/shell/src/compositor/test_control.rs` — `Command` and `Response` protocol definitions
- `crates/session/shell/src/bin/shots/scenario.rs` — pre-written scenarios that exercise the same code paths in-process
