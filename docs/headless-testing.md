# Headless testing: screenshots, recordings, remote control

This guide covers everything you need to test the nirios shell (and individual apps in a follow-up) **without opening a window**. Every command below writes PNG / H.264 / JSON to disk; nothing requires a display server attached to your terminal.

Three layers, each useful on its own:

1. **`shots` — scenarios → PNG / H.264** (run pre-written interactions, capture per-step output)
2. **`NIRIOS_TEST_CONTROL` socket — JSON RPC over a unix socket** (drive a running binary programmatically: click, drag, type, snapshot)
3. **`--backend headless` — live compositor without a window** (the same binary that boots on a real display, running through the same code paths, but rendering into an off-screen wgpu texture)

## Layer 1: `shots` — pre-written scenarios

Best for: regression captures, golden-image diffs, video walkthroughs of known UX flows.

```bash
# Inventory the catalogue (currently ~22 scenarios — palette typing, workspace switch, etc.):
cargo run --bin shots --quiet -- --list-scenarios

# Capture every scenario in offscreen mode (default). Produces one PNG per scene
# under `target/<dir>/`. Pixel-identical to a live run.
cargo run --bin shots --quiet -- --mode offscreen --out target/snapshots

# A single scenario, with explicit viewport:
cargo run --bin shots --quiet -- --mode offscreen --scenario palette_typing --width 1600 --height 900 --out target/palette

# H.264 video of a scenario (30 fps default; one second per scenario step).
# Output is raw Annex-B NALU stream — playable by mpv / ffplay / vlc.
cargo run --bin shots --quiet -- --mode recording --scenario workspace_switch --out target/ws.h264

# Interactive window mode (real winit window — useful for eyeballing while developing):
cargo run --bin shots --quiet -- --mode window --scenario palette_typing
```

Scenarios are defined in `quick-shell/src/bin/shots/scenario.rs`. Each is a `Vec<Action>` of `ClickNamed("add_workspace")`, `Drag {...}`, `Key(...)`, `Type(...)` steps. The engine resolves names against the live widget tree (see Layer 2's widget registry).

**No display server required.** `shots --mode offscreen` and `--mode recording` use `quick_ui::offscreen::OffscreenRenderer` — wgpu only, no window.

## Layer 2: `NIRIOS_TEST_CONTROL` — drive the live binary via a unix socket

Best for: ad-hoc interactive testing from a script or an AI agent, exploratory "click here, snapshot that" flows, integration tests where you want to assert against the live compositor's state.

### Enable the socket

```bash
# Pick a socket path (must be writable; typically under /run/user/$UID/ or /tmp/):
export NIRIOS_TEST_CONTROL=/tmp/nirios-test-control.sock

# Spawn the compositor. The env var enables the socket; nothing else changes.
cargo run --bin quick-desktop -- --backend headless &
# (`--backend headless` runs without a window — see Layer 3. For interactive
# development you can also use the default winit backend; the socket works
# either way.)

# Confirm the socket bound:
ls -la $NIRIOS_TEST_CONTROL
```

The compositor prints `nirios compositor test_control listening on "<path>"` to stderr once the socket is ready to accept.

### Easiest path: the `nirios-control` CLI

A wrapper binary ships in the same workspace. Subcommands take human-friendly arguments instead of raw JSON:

```bash
cargo build -p quick-desktop --bin nirios-control      # builds alongside quick-desktop

nirios-control list-targets                             # PNAME (X, Y, W, H) per line
nirios-control click add_workspace                      # click_named
nirios-control click-at 400 300                         # click_at
nirios-control drag 100 100 300 200 12                  # drag with 12 motion steps
nirios-control key Enter                                # named key OR single char
nirios-control type "hello"                             # one press+release per character
nirios-control modifiers ctrl,shift                     # comma-separated set; empty clears all
nirios-control snapshot /tmp/state.png                  # PNG to disk
nirios-control state-dump --pretty                      # live WorkspaceTree + Chrome as JSON
nirios-control raw '{"op":"list_targets"}'              # escape hatch for unwrapped commands
```

Default socket: `$NIRIOS_TEST_CONTROL` env var, else `/tmp/nirios.sock`. Override with `--socket /path`. Errors from the compositor surface as non-zero exit + structured `code: message` on stderr.

### Wire protocol (when you can't use the CLI)

The socket accepts newline-delimited JSON. Every command returns one JSON response line.

```bash
# List every clickable widget by name + rect:
echo '{"op":"list_targets"}' | nc -U $NIRIOS_TEST_CONTROL

# Click a widget by name (resolved from the live widget tree):
echo '{"op":"click_named","name":"add_workspace"}' | nc -U $NIRIOS_TEST_CONTROL

# Click at raw coordinates:
echo '{"op":"click_at","x":400,"y":300}' | nc -U $NIRIOS_TEST_CONTROL

# Drag from one point to another (default 8 intermediate motions):
echo '{"op":"drag","x_from":100,"y_from":100,"x_to":300,"y_to":200,"steps":12}' | nc -U $NIRIOS_TEST_CONTROL

# Set modifiers, then send a key:
echo '{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}' | nc -U $NIRIOS_TEST_CONTROL
echo '{"op":"key","key":"k"}' | nc -U $NIRIOS_TEST_CONTROL    # Ctrl+K opens the palette
echo '{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}' | nc -U $NIRIOS_TEST_CONTROL

# Type a string (one press+release per character):
echo '{"op":"type","text":"hello"}' | nc -U $NIRIOS_TEST_CONTROL

# Capture a PNG of the current state:
echo '{"op":"snapshot","path":"/tmp/state.png"}' | nc -U $NIRIOS_TEST_CONTROL

# Get the live workspace tree + chrome state as JSON:
echo '{"op":"state_dump"}' | nc -U $NIRIOS_TEST_CONTROL
```

Full command reference: `quick-shell/src/compositor/test_control.rs` (the `Command` enum is the source of truth; comments document each variant).

### Response shapes

```
Successful query (list_targets):
  {"targets":[{"name":"add_workspace","rect":[1234,56,32,32]}, ...]}

Successful action (click_*, drag, key, type, modifiers, snapshot):
  {"ok":{"frame":0}}

Error:
  {"err":{"code":"unknown_target","message":"no addressable widget named ..."}}

Error codes:
  parse_error      — malformed JSON or unknown command
  unknown_target   — click_named / drag with a name not in the registry
  invalid_path     — snapshot path is empty
  not_implemented  — command parsed but execution deferred to a later phase
```

### Widget registry — how `click_named` finds things

Every widget in the tree exposes a name through either:
- `Widget::test_id()` (opt-in) — currently registered: `command_palette_input`, `wheel_disc`, more landing per the quick-ui plan
- Path-based fallback: `Parent/Child[index]` — e.g. `OverlayHost/TopBarChrome/Button[0]`

`list_targets` returns every widget with a non-zero rect. The bare name (`Parent/Child`) is registered when there's exactly one sibling of that name; an indexed form (`Parent/Child[N]`) always works.

To make a widget more addressable, override `Widget::test_id()` in `quick-ui` to return `Some("my_stable_name")`. See `command_palette_input` in `quick-ui/src/widget/command_palette.rs` for the pattern.

## Layer 3: `--backend headless` — display-free live binary

Best for: running the full compositor in CI, on a remote shell, or in distrobox without forwarding a display. Same code paths as the real binary; just paints into a wgpu off-screen texture instead of a window.

```bash
# Spawn headless. Note: no DISPLAY or WAYLAND_DISPLAY needed.
env -u DISPLAY -u WAYLAND_DISPLAY \
    NIRIOS_TEST_CONTROL=/tmp/control.sock \
    cargo run --bin quick-desktop --quiet -- --backend headless &

# Click + snapshot:
echo '{"op":"click_named","name":"add_workspace"}' | nc -U /tmp/control.sock
echo '{"op":"snapshot","path":"/tmp/after-add-workspace.png"}' | nc -U /tmp/control.sock

# Kill when done:
kill %1
```

Configure the headless viewport via env var (defaults to 1600×900):
```bash
NIRIOS_HEADLESS_SIZE=1920x1080 cargo run --bin quick-desktop -- --backend headless
```

## End-to-end recipe — "click, snapshot, look"

**The fastest path: run the example script.**

```bash
./scripts/headless-examples/full-loop.py
# spawns compositor, opens palette, types "Files", snapshots two PNGs,
# tears down. ~5 seconds. Output: /tmp/full-loop-palette.png + /tmp/full-loop-final.png
```

The script is ~100 lines of Python with stdlib only (`socket` + `json`). Read it as the canonical template for "spawn → drive → snapshot → exit." Copy-paste into your own automation; nothing in it is repo-specific except the binary path.

For exploratory / partial flows, see `scripts/headless-examples/` (full walkthrough in `scripts/headless-examples/README.md`):
- `list-targets.py` — enumerate clickable widgets
- `click-and-snapshot.py` — click named widget + PNG
- `keyboard-flow.py` — modifier+key+type combos
- `drag-demo.py` — press-motions-release
- `full-loop.py` — the end-to-end above
- `spawn-headless.sh` — shell helper to start the compositor + wait for socket

If you want to do it by hand without Python, use `socat` (Ncat doesn't have `-q`):

```bash
# 1. Start the compositor (no display required):
export NIRIOS_TEST_CONTROL=/tmp/nirios-e2e.sock
env -u DISPLAY -u WAYLAND_DISPLAY \
    cargo run --bin quick-desktop --quiet -- --backend headless &
COMP_PID=$!

# 2. Wait for the socket to be ready:
until [ -S $NIRIOS_TEST_CONTROL ]; do sleep 0.1; done

# 3. Drive the UI (socat closes the connection cleanly after EOF, unlike Ncat):
echo '{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}' | socat - UNIX-CONNECT:$NIRIOS_TEST_CONTROL
echo '{"op":"key","key":"k"}'                                                          | socat - UNIX-CONNECT:$NIRIOS_TEST_CONTROL
echo '{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}'          | socat - UNIX-CONNECT:$NIRIOS_TEST_CONTROL
echo '{"op":"type","text":"terminal"}'                                                 | socat - UNIX-CONNECT:$NIRIOS_TEST_CONTROL
echo '{"op":"snapshot","path":"/tmp/palette-with-query.png"}'                          | socat - UNIX-CONNECT:$NIRIOS_TEST_CONTROL

# 4. Inspect the PNG:
xdg-open /tmp/palette-with-query.png

# 5. Tear down:
kill $COMP_PID
rm -f $NIRIOS_TEST_CONTROL
```

## Where to look in source

| What | Where |
|---|---|
| `shots` binary | `quick-shell/src/bin/shots/` |
| Scenario catalogue | `quick-shell/src/bin/shots/scenario.rs::catalogue()` |
| Offscreen renderer | `quick-ui/src/offscreen.rs` |
| `NIRIOS_TEST_CONTROL` protocol | `quick-shell/src/compositor/test_control.rs` |
| Widget registry walk | `quick-shell/src/compositor/frame.rs::walk_widget` |
| Backend trait | `quick-shell/src/compositor/backend/mod.rs` |
| Headless backend impl | `quick-shell/src/compositor/backend/headless.rs` |
| Spec | `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md` |

## Per-app screenshots

`apps/terminal` and `apps/file-explorer` accept `--shots PATH` and write a 1200×720 RGBA PNG of the cold-start UI to `PATH` without opening a window:

```bash
./target/debug/quick-terminal --shots /tmp/terminal.png
./target/debug/file-explorer  --shots /tmp/files.png
```

Both flow through `quick_ui::offscreen::OffscreenRenderer` and run a single `sync → build_layout → compute_layout → apply_layout → draw` cycle — no event loop. Smoke tests live in each app's `tests/shots_smoke.rs` and assert the output starts with the PNG magic header.

## What's still missing

- **`quick-browser --shots`.** The browser uses `iced` 0.13 directly plus a Servo engine; iced 0.13 has no straightforward headless rendering path (the runtime owns the winit event loop). A future shot harness would need to drive `iced_wgpu::Renderer` against an offscreen wgpu texture with a hand-rolled layout pass, or wait for upstream iced to expose a headless surface. The same gap applies if any future system app picks iced.

## CI integration

The `shots --mode offscreen` flow runs cleanly in CI (no display, no GPU forwarding) as long as the runner has a recent Mesa + a software wgpu fallback. The compositor's `--backend headless` does the same thing for live-binary tests.

In distrobox locally: `distrobox enter my-distrobox -- bash -c 'cd /home/user/Development/quickdesktop && cargo test -p quick-desktop --test control_socket_test -- --ignored --test-threads 1'` — runs the full integration suite under `--ignored` (those tests spawn the binary themselves; opt-in via `--ignored` because they currently require a Wayland session for the legacy winit backend, but will work fully headless once the `--backend headless` worker lands).
