# Headless testing: screenshots, recordings, remote control

This guide covers everything you need to test the tilo shell (and individual apps in a follow-up) **without opening a window**. Every command below writes PNG / H.264 / JSON to disk; nothing requires a display server attached to your terminal.

Three layers, each useful on its own:

1. **`shots` — scenarios → PNG / H.264** (run pre-written interactions, capture per-step output)
2. **`TILO_TEST_CONTROL` socket — JSON RPC over a unix socket** (drive a running binary programmatically: click, drag, type, snapshot)
3. **`--backend headless` — live compositor without a window** (the same binary that boots on a real display, running through the same code paths, but rendering into an off-screen wgpu texture)

Native application publication also has compositor-free integration fixtures:

```bash
cargo test --locked -p shell --lib shell::in_process::tests::authorized_
cargo test --locked -p tilo-action-broker --lib collab_broker_capnp::tests::certified_join_rejects_other_workspace_revoked_and_unenrolled_device -- --exact
```

The Browser and Store fixtures provision a disposable identity and run the actual
identity service, collaboration broker, original application factory and directory
RPC. Browser retains its primary document, browsing history and address book. All
three use the certified person's own room, so publication must wait for their
initial joins to become ready. Store must publish `inspect-catalogue` through the
same authorized context. No application action is invoked.

This qualifies matching-room publication. It does not qualify an application
spanning a foreground workspace and a separate person room: a certification for
one room cannot authorize the other. The broker's cross-workspace rejection stays
covered independently. Installed control-socket timeouts require separate runtime
evidence; an empty query alone does not establish absent publication.

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

Scenarios are defined in `crates/session/shell/src/bin/shots/scenario.rs`. Each is a `Vec<Action>` of `ClickNamed("add_workspace")`, `Drag {...}`, `Key(...)`, `Type(...)` steps. The engine resolves names against the live widget tree (see Layer 2's widget registry).

**No display server required.** `shots --mode offscreen` and `--mode recording` use `quick_ui::offscreen::OffscreenRenderer` — wgpu only, no window.

## Owner control with an existing identity

For semantic actions on an existing owner's headless session, launch
`shell --host-desktop-session --backend headless --headless-owner-control`.
The normal first-login, device-owner, administrator-group, lock and caller
checks apply. No development control variable is needed. The endpoint is
`$XDG_RUNTIME_DIR/tilo-session/headless-owner-control.sock`, separate from the
real seat's `admin-control.sock`; an existing live listener is never replaced.
Headless sessions without the explicit flag retain their existing behavior.

Use the normal action interface against that endpoint:

```bash
tilo-control --socket "$XDG_RUNTIME_DIR/tilo-session/headless-owner-control.sock" actions list
```

This endpoint accepts semantic owner commands, not the development input and
capture protocol described below. A locked session still requires its normal
unlock action. Physical cursor and display acceptance remains a real-seat test.

## Layer 2: `TILO_TEST_CONTROL` — drive the live binary via a unix socket

Best for: ad-hoc interactive testing from a script or an AI agent, exploratory "click here, snapshot that" flows, integration tests where you want to assert against the live compositor's state.

### Enable the socket

```bash
# Pick a socket path in your private runtime directory. Not /tmp: another
# account could bind a shared name first and receive every command you send.
# The compositor creates a missing parent 0700 and makes the socket 0600.
export TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/test-control.sock

# Spawn the compositor. The env var enables the socket; nothing else changes.
cargo run --bin shell -- --backend headless &
# (`--backend headless` runs without a window — see Layer 3. For interactive
# development you can also use the default winit backend; the socket works
# either way.)

# Confirm the socket bound:
ls -la $TILO_TEST_CONTROL
```

The compositor prints `tilo compositor test_control listening on "<path>"` to stderr once the socket is ready to accept.

### A headless session never touches the host's power

A headless (or nested winit) session runs beside the desktop that owns the
machine, and it reaches the same system logind. So only a `--backend drm`
session starts the power service: headless and winit sessions do not take
logind's lid/power-key delegation, never request suspend on idle whatever
`power.auto_suspend` says, and neither offer nor admit Suspend, Restart,
Shut down or Switch user from the power menu, the palette or
`session.*` actions (`tilo::power` logs `session does not own the seat` at
startup). Lock stays: it locks only the compositor's own scene. Test idle
suspend through `SessionManager` and `TILO_IDLE_SUSPEND_SECS`, which exercise
the state machine without an actuator.

### Put `TMPDIR` somewhere with room

`/tmp` is a 16 GB tmpfs shared with everything else on the box, and a
few concurrent `rustc` processes will fill it. When they do, anything
that asks for a temporary file fails — harnesses here allocate session
roots through `tempfile` — and the error surfaces from inside that call
as `Quota exceeded`, a bare exit 1, or simply empty output. It reads
like a bug in whatever you were testing, which is the expensive part.

Point it at real disk before a run that spawns sessions:

```bash
TMPDIR=$HOME/tmp ./scripts/headless-examples/peers-cross-both-ways.py
```

Keep session roots short (`/tmp/qs-xxxx`-shaped): a unix socket path has
a hard ~108-byte limit, and a deep temp directory silently pushes the
control socket past it.

### Easiest path: the `tilo-control` CLI

A wrapper binary ships in the same workspace. Subcommands take human-friendly arguments instead of raw JSON:

```bash
cargo build -p tilo-control                 # lands in target/debug; images ship /usr/bin/tilo-control
alias qc='cargo run -q --bin tilo-control --' # or: cargo install --locked --path crates/session/control-cli

qc actions list Calculator --json           # inspect search results without opening the palette
qc actions invoke applications dev.tilo.calculator # invoke the exact discovered identity
qc actions list --json                      # inspect the running apps' exported actions
qc list-targets                             # PNAME (X, Y, W, H) per line
qc click add_workspace                      # click_named
qc click-at 400 300                         # click_at
qc drag 100 100 300 200 12                  # drag with 12 motion steps
qc key Enter                                # named key OR single char
qc type "hello"                             # one press+release per character
qc modifiers ctrl,shift                     # comma-separated set; empty clears all
qc snapshot /tmp/state.png                  # PNG to disk
qc windows                                  # open windows: workspace/strip, group, tab, app
qc state-dump --pretty                      # live WorkspaceTree + Chrome as JSON, without history
qc state-dump --history                     # ... with effect receipts and audit trails
qc raw '{"op":"list_targets"}'              # escape hatch for unwrapped commands
```

Default socket: `$TILO_TEST_CONTROL` env var, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` (an error, not a `/tmp` fallback, when `XDG_RUNTIME_DIR` is unset). Override with `--socket /path`. Errors from the compositor surface as non-zero exit + structured `code: message` on stderr.

### Select search results by identity

Use `actions list QUERY --json` to inspect the same providers used by the palette.
Enumeration starts asynchronous index refreshes; a cold query can be incomplete.
Retry within a bounded deadline when the expected app is missing, and report it
as unavailable if the deadline expires. Opening the palette is unnecessary.

Titles are display text, not identities. For example, two results can both say
“Open Calculator”:

| Provider | ID | Meaning |
|---|---|---|
| `applications` | `dev.tilo.calculator` | Native Tilo Calculator |
| `host-applications` | `org.gnome.Calculator` | Host GNOME Calculator |

Inspect `kind`, `subtitle`, and the `(provider, id)` pair before choosing. Keep the
selected pair and pass both to `actions invoke`; never substitute the first
same-named result when the intended app is absent. `score` ranks results but does
not establish application identity.

After launch, enumerate again and inspect the app actions' `application`,
`resource`, `operation`, `params`, and `contract` fields. Invoke the returned
provider/ID with its declared arguments, adding `--wait` so the answer is the
result and not the admission. To follow a pending receipt, use
`actions status INVOCATION_ID --wait`, never a sleep-and-retry loop (see
`docs/agent-action-api.md`). Then read back the affected resource or desktop state to verify the
effect. A descriptor alone does not prove execution; a visible tile alone does
not prove the app has rendered. `raw '{"op":"foreign_frames"}'` exposes the
presented app identity, surface, frame count, and rendering errors for foreign
Wayland clients. It does not observe apps mounted inside the compositor:
`actions:window.new.terminal` mounts Terminal in-process, with `AppPlaceholder`
as its persisted tile kind. Neither that kind nor an empty `foreign_frames`
reply proves the hosted app failed to render. Verify its exported
`inspect-session` operation to completion, its laid-out `terminal` target and
a private headless snapshot. Process startup additionally requires an open
session with input available; rendering a missing-runtime explanation is not
a working PTY. The separately discovered `applications:so.tilo.terminal`
launches the packaged app and should be checked with foreign-frame counters.

`scripts/headless-examples/action-api-demo.py` exercises discovery and semantic
invocation against an isolated compositor. `app-launch-proof.py` selects native
applications by provider/ID and checks that a new surface actually paints.

### Wire protocol (when you can't use the CLI)

The socket accepts newline-delimited JSON. Every command returns one JSON response line.

```bash
# List every clickable widget by name + rect:
echo '{"op":"list_targets"}' | nc -U $TILO_TEST_CONTROL

# Click a widget by name (resolved from the live widget tree):
echo '{"op":"click_named","name":"add_workspace"}' | nc -U $TILO_TEST_CONTROL

# Click at raw coordinates:
echo '{"op":"click_at","x":400,"y":300}' | nc -U $TILO_TEST_CONTROL

# Drag from one point to another (default 8 intermediate motions):
echo '{"op":"drag","x_from":100,"y_from":100,"x_to":300,"y_to":200,"steps":12}' | nc -U $TILO_TEST_CONTROL

# Set modifiers, then send a key:
echo '{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}' | nc -U $TILO_TEST_CONTROL
echo '{"op":"key","key":"k"}' | nc -U $TILO_TEST_CONTROL    # Ctrl+K opens the palette
echo '{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}' | nc -U $TILO_TEST_CONTROL

# Type a string (one press+release per character):
echo '{"op":"type","text":"hello"}' | nc -U $TILO_TEST_CONTROL

# Capture a PNG of the current state:
echo '{"op":"snapshot","path":"/tmp/state.png"}' | nc -U $TILO_TEST_CONTROL

# Get the live workspace tree + chrome state as JSON. A workspace's
# append-only history (effect receipts, task audit trails, ownership-transfer
# records) is left out; add "include_history":true to get it:
echo '{"op":"state_dump"}' | nc -U $TILO_TEST_CONTROL

# Just the open windows, one row per tab:
echo '{"op":"windows"}' | nc -U $TILO_TEST_CONTROL

# Copy and paste as the compositor's own in-process widgets do. Both call
# straight into `ui::text_selection`, the same functions the terminal, the
# file explorer, notes, the editor and every piece of chrome call, so this
# drives the compositor's side of a cross-process transfer through the
# production path rather than around it. `primary` targets the middle-click
# selection instead of the clipboard.
echo '{"op":"clipboard_copy","text":"hello","primary":false}' | nc -U $TILO_TEST_CONTROL
echo '{"op":"clipboard_paste","primary":false}' | nc -U $TILO_TEST_CONTROL
# → {"clipboard":{"text":"hello"}}   (text is null when nothing is copied)

# Pointer press and release as separate events. `click_at` bundles them,
# which cannot express a drag-and-drop: the source client has to be *holding*
# the button when it calls start_drag, and the drop happens on a release that
# arrives after the pointer has travelled to another window.
echo '{"op":"pointer_move","x":260,"y":140}' | nc -U $TILO_TEST_CONTROL
echo '{"op":"pointer_button","pressed":true}' | nc -U $TILO_TEST_CONTROL
echo '{"op":"pointer_button","pressed":false}' | nc -U $TILO_TEST_CONTROL

# Open the next window of an app in its own window group rather than as a tab
# in the focused one, via the launcher's normal pending-placement note. Two
# client windows are only on screen at the same time when they are in
# separate groups — the precondition for dragging anything between them.
echo '{"op":"place_next_window","app_id":"org.example.App"}' | nc -U $TILO_TEST_CONTROL
```

Full command reference: `crates/session/shell/src/compositor/test_control.rs` (the `Command` enum is the source of truth; comments document each variant).

### Response shapes

```
Successful query (list_targets):
  {"targets":[{"name":"add_workspace","rect":[1234,56,32,32]}, ...]}

Successful action (click_*, drag, key, type, modifiers, snapshot,
                   pointer_move, pointer_button, clipboard_copy,
                   place_next_window):
  {"ok":{"frame":0}}

Clipboard read (clipboard_paste):
  {"clipboard":{"text":"hello"}}      // null text = nothing is copied

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
- `Widget::test_id()` (opt-in) — currently registered: `command_palette_input`, `wheel_disc`, more landing per the ui plan
- Path-based fallback: `Parent/Child[index]` — e.g. `OverlayHost/TopBarChrome/Button[0]`

`list_targets` returns every widget with a non-zero rect. The bare name (`Parent/Child`) is registered when there's exactly one sibling of that name; an indexed form (`Parent/Child[N]`) always works.

To make a widget more addressable, override `Widget::test_id()` in `ui` to return `Some("my_stable_name")`. See `command_palette_input` in `crates/sdk/ui-core/src/widget/command_palette.rs` for the pattern.

## Layer 3: `--backend headless` — display-free live binary

Best for: running the full compositor in CI, on a remote shell, or in distrobox without forwarding a display. Same code paths as the real binary; just paints into a wgpu off-screen texture instead of a window.

```bash
# Spawn headless. Note: no DISPLAY or WAYLAND_DISPLAY needed.
env -u DISPLAY -u WAYLAND_DISPLAY \
    TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/control.sock \
    cargo run --bin shell --quiet -- --backend headless &

# Click + snapshot:
echo '{"op":"click_named","name":"add_workspace"}' | nc -U $XDG_RUNTIME_DIR/tilo-session/control.sock
echo '{"op":"snapshot","path":"/tmp/after-add-workspace.png"}' | nc -U $XDG_RUNTIME_DIR/tilo-session/control.sock

# Kill when done:
kill %1
```

To time what an agent waits for on this socket, run
`scripts/perf/agent_rtt.py`. It measures actions list, state dump, a no-op
invoke (accepted, and completed through `wait_ms`), throughput under
concurrent clients and the same no-op through the MCP server. Give it
`--socket PATH` for a running desktop, or `--spawn SHELL` to start a
compositor through `spawn-headless.sh`. A spawned compositor is removed on
exit. `spawn-headless.sh` runs `$TILO_HEADLESS_BINARY` when that is set. A
release build serves the socket only with `--features dev-overrides`.
`scripts/perf/agent_task.py` times a whole task the same way: a scripted,
streamed fake model creates a note in Notes, writes its body and finds it by
search, and the record splits the time between Tilo, the model and the
agent loop. Its `--spawn` desktop is seeded by the `agent_task_desktop`
example of `shell`.

Configure the headless viewport via env var (defaults to 1600×900):
```bash
TILO_HEADLESS_SIZE=1920x1080 cargo run --bin shell -- --backend headless
```

For multi-monitor behaviour, replace the one output with a real arrangement
instead (the two variables are mutually exclusive):
```bash
# A named fixture: docked-laptop, vertical-stack, portrait-plus-4k, mixed-scale-offset
TILO_HEADLESS_OUTPUTS=docked-laptop cargo run --bin shell -- --backend headless
# Or a spec: WxH mode, then optional @SCALE, ~HZ, rDEGREES, +X+Y per output
TILO_HEADLESS_OUTPUTS=2560x1600@2,1920x1080@1+2560+0 cargo run --bin shell -- --backend headless
```
Each output gets its own `wl_output`; snapshots cover the combined extent.
`shots --outputs <fixture|spec>` renders the shell chrome across the same
arrangement, and `tests/multi_output.rs` reads what clients see of it.

Nobody looks at a headless session's frames by default, so it lays them out
and answers clients' frame callbacks but does not rasterize them; `snapshot`
renders on demand. A viewer that streams frames attaches with
`{"op":"watch","watching":true}` (`tilo-control watch on`), or starts the
session with `TILO_HEADLESS_WATCH=1`.

While watched, headless requests the producer's retained-damage proof. A matching
empty damage list first validates the previous image with the existing bounded
screenshot renderer; later unchanged frames reuse that completed RGBA buffer
without a render or readback. First and changed frames keep the dense path.
Missing predecessors, unversioned scene edits, viewer detach/reattach and material
or HDR frames redraw. A replacement backend starts uncached at its new size.
Presentation remains synchronous; snapshots cannot race queued renderer work.
This reduces watched headless work, not physical display input-to-photon latency.

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

Alongside those, the `*-proof.py` scripts in the same directory stand up
**two or three real compositors** with separate identities and homes, put
them in one room, and assert across them. Reach for one of those when the
question is about collaboration rather than about one session's chrome —
`peers-cross-both-ways.py` is the presence regression net (each session
must paint the other's cursor, under the name the room gave it), and
`named-peer-proof.py` covers naming through a running app.

If you want to do it by hand without Python, use `socat` (Ncat doesn't have `-q`):

```bash
# 1. Start the compositor (no display required):
export TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/e2e.sock
env -u DISPLAY -u WAYLAND_DISPLAY \
    cargo run --bin shell --quiet -- --backend headless &
COMP_PID=$!

# 2. Wait for the socket to be ready:
until [ -S $TILO_TEST_CONTROL ]; do sleep 0.1; done

# 3. Drive the UI (socat closes the connection cleanly after EOF, unlike Ncat):
echo '{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}' | socat - UNIX-CONNECT:$TILO_TEST_CONTROL
echo '{"op":"key","key":"k"}'                                                          | socat - UNIX-CONNECT:$TILO_TEST_CONTROL
echo '{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}'          | socat - UNIX-CONNECT:$TILO_TEST_CONTROL
echo '{"op":"type","text":"terminal"}'                                                 | socat - UNIX-CONNECT:$TILO_TEST_CONTROL
echo '{"op":"snapshot","path":"/tmp/palette-with-query.png"}'                          | socat - UNIX-CONNECT:$TILO_TEST_CONTROL

# 4. Inspect the PNG:
xdg-open /tmp/palette-with-query.png

# 5. Tear down:
kill $COMP_PID
rm -f $TILO_TEST_CONTROL
```

## Measuring the frame loop

The compositor carries per-tick instrumentation — cadence, per-phase
durations, input-to-present latency — behind env vars, and a harness that
drives a measured run:

```bash
# 8s idle run on the headless backend; prints the frame-time distribution.
scripts/perf/frame-loop-bench.py --seconds 8

# Under load from a client that commits a new buffer every frame,
# with a click injected every 50 ms so latency is measured.
scripts/perf/frame-loop-bench.py --seconds 8 --load fullscreen --input-hz 20

# The nested-Wayland backend, inside a headless `cage` so no window
# appears on your desktop. This is the only local way to exercise the
# render thread — the headless backend deliberately presents inline.
scripts/perf/frame-loop-bench.py --seconds 8 --backend winit --nested

# Force a non-60 Hz cadence when no such panel is attached.
scripts/perf/frame-loop-bench.py --seconds 8 --refresh-hz 144

# Acceptance gate for the failure a user actually sees: a 144 Hz desktop is
# driven with synthetic pointer motion while the native Chat app launches.
# It fails on a launch/first-frame freeze, <120 painted fps, missing input
# samples, or input-to-present p99 above 25 ms. Point the three store/tool
# flags at an existing dev install; no RPM/image rebuild is involved.
scripts/perf/frame-loop-bench.py --seconds 12 --refresh-hz 144 \
  --input-hz 30 --launch-app dev.tilo.chat --gate \
  --shell-bin target/release/shell \
  --launcher-bin target/release/tilo-launcher \
  --apps-data-dir "$HOME/.local/share/tilo-apps" \
  --runtime-bundle-dir "$HOME/.local/share/tilo-runtime"

# Diff two runs, or re-read a recorded trace after changing the warmup.
scripts/perf/frame-loop-bench.py --compare before.stats.json after.stats.json
scripts/perf/frame-loop-bench.py --resummarize target/frame-bench/run.csv
```

| Env var | Effect |
|---|---|
| `TILO_FRAME_TRACE=/path.csv` | one row per loop tick |
| `TILO_FRAME_CPU_TRACE=/private/path.csv` | numeric companion for painted ticks: phase CPU and bracketed clock anchors |
| `TILO_FRAME_SUMMARY=/path.json` | percentile summary on clean exit |
| `TILO_FRAME_WARMUP_MS` | samples excluded from the summary (default 1500) |
| `TILO_RUN_SECONDS` | exit cleanly after N seconds, flushing both |
| `TILO_REFRESH_HZ` | force the presentation rate (diagnostic) |
| `TILO_PAINT_STATS=1` | 2-second painted-vs-idled counter on stderr |
| `TILO_PAINT_STATS=ticks` | the same, plus a `paint-tick: painted=… woke=…` line per loop iteration |
| `TILO_LOOP_TIMING=1` | log any tick over 100 ms with a phase breakdown |

`TILO_RUN_SECONDS` matters for a scripted run: the trace and summary are
written by the normal exit path, so a `SIGKILL`ed compositor loses them.

For a source-matched performance fixture, opt into `TILO_FRAME_CPU_TRACE` through
its supported launch configuration. Use a new, separate file in the fixture
owner's private directory; an existing file is refused instead of overwritten.
This leaves the original frame CSV and its parsers unchanged;
join the companion's `seq` and `start_us` to the original rows. It records no
application payload, titles, paths or identities. No extra clock reads or writes
occur when this option is absent.

The companion measures calling-thread and aggregate process CPU separately for
layout, scene construction, submission and the layout-start-to-submit-return
interval. The layout label includes the existing focus/output/accessibility
reconciliation. Process
CPU includes parallel renderer **and other background threads**. A low thread CPU
value alone cannot identify GPU waiting, host scheduling or TCG overhead. Compare
the paired guest/host CPU and scheduling counters as well. `present_async=1`
means submit timings cover enqueueing, rather than the render worker's completion.
Subtract the labelled wall phases from `paint_wall_us` to retain visibility of
capture and control work between them; never call that residual GPU time.

`anchor_unix_us` is bracketed by guest `CLOCK_MONOTONIC` readings;
`anchor_offset_us` measures tick-start-to-`Instant` sampled inside that bracket.
Subtract that offset from both monotonic endpoints to bound the tick start; retain
rounding uncertainty as well as the bracket width.
Correlate UTC with the collector's paired host/guest clock samples and retain
their uncertainty. Guest and host monotonic epochs differ, and UTC can step.
Unavailable CPU or monotonic clocks are blank, not zero. The companion flushes
its first painted row and then once per second, retains no sample history, and
stops after 200,000 painted rows with a diagnostic. Its row formatting allocates
no heap memory; only explicitly enabled tracing incurs CPU clock and I/O costs.

## Idle wakeups

An idle desktop should leave the CPU asleep. `idle_wakeups::` in the shell's
`it` target holds it to a budget: it starts the production binary headless with
a signed-in desktop, restores a Notes tab, opens a Terminal through
`window.new.terminal`, waits out startup work and 5 s more, then samples every thread of the
compositor's process tree from `/proc` for 10 s and fails above
`IDLE_WAKEUP_BUDGET` wakeups per second (voluntary context switches: how often
a thread blocked again), above `IDLE_CPU_BUDGET` percent of a core, or if the
compositor painted while nobody touched it. It also holds the in-process apps'
collaboration shovel and the identity service's client thread to
`KEY_ASK_BUDGET` together, so a route asking for its channel keys on a 1 s
timer again fails by name.

```bash
cargo test --locked -p shell --test it idle_wakeups:: -- --nocapture
```

It prints a table, busiest first:

```text
idle wakeups over 10.0 s: 60.27/s and 2.1 % of a core across 1 processes
  wakeups/s  preempt/s   cpu %  threads  process / thread
      20.79       1.20     0.6        2  shell / tilo-collab-bro
      19.89       0.00     0.4        1  shell / tilo-audio-outp
       ...
compositor frame loop: 0.0 ticks/s (0 ticks, woken by: ), 0 frames painted (after: ), 0 clock minute(s) turned; its paint-stats in the window:
```

`wakeups/s` counts every context switch (voluntary: the thread blocked again;
involuntary: it was preempted), `preempt/s` the involuntary share, `cpu %` its
CPU time, `threads` how many threads of that name were seen (names are the
kernel's 15-byte truncation). A loop that spins without blocking shows in
`cpu %` and in the frame loop's tick rate rather than in wakeups, so the gate
also holds the loop to `LOOP_TICK_BUDGET` and to no painted frames but the top
bar's clock repaint when a minute turns in the window. It counts the
compositor's `paint-tick` lines (`TILO_PAINT_STATS=ticks`), one per loop
iteration with what woke it, read inside the window: a 2-second `paint-stats`
summary counts every tick since the previous one, so the first one printed in
the window can carry startup's ticks. Pull requests run it whenever
`shell` changes; the scheduled lane (`kpis=wakeups`) runs it with a 60 s window
(`TILO_IDLE_WAKEUP_WINDOW_SECS`) and records `wakeups.*`. When a change
removes a waker, lower `IDLE_WAKEUP_BUDGET` in the same pull request to the new
measurement plus the same margin.

Headless covers what headless runs. DRM-only services (power, hotplug, the
rotation sensor) and out-of-process apps are held by per-thread probes in
their own crates: `tilo_test_env::idle::{spawned_thread, assert_stays_asleep}`
for one service thread of a unit test, `idle_wakeups(pid, settle, window)` for
a process tree, and `sample_process` with `Wakeups::between` for a stretch the
test drives itself (`shell::network::idle_quiet_tests` uses it for the
collaboration plane of two converged desktops).

A gate that runs inside the pull-request test shard shares the runner with
other tests, and preemptions grow with that load. `Wakeups::blocking_per_second`
counts only voluntary switches, one per wakeup. Both gates in
`idle_quiet_tests`, the lone desktop and the converged pair, use it, each
together with a CPU bound that catches a thread spinning without blocking
.

## Idle memory

An idle desktop should hold little memory. `idle_memory::` in the shell's `it`
target starts the production binary headless with a signed-in, empty desktop,
watched so frames rasterise, waits past the trim that returns startup's freed
heap (`heap_trim::STARTUP_SETTLED`), and fails when the compositor's anonymous
RSS is above `IDLE_ANON_BUDGET_MIB`.

```bash
cargo test --locked -p shell --test it idle_memory:: -- --nocapture
```

It prints two breakdowns. The compositor's own, `raw '{"op":"process_memory"}'`
on the control socket, sums `mincore` over `/proc/self/maps` per kind of
mapping (`heap_main`, `heap_arenas`, `thread_stacks`, `anon_other`,
`gpu_memfd` for lavapipe's device memory, `fonts`, `binary`, `libraries`, …),
with glibc's in-use and free bytes and the threads by name. It is the one that
works on Tilo's kernel, which has no smaps (`CONFIG_PROC_PAGE_MONITOR=n`), so
vm-acceptance R01 records the same report (`memory.shell_parts_mib.*`). The
test then reads `/proc/<pid>/smaps` beside it for RSS, PSS and private dirty
pages per mapping, which CI's kernel has.

## Where to look in source

| What | Where |
|---|---|
| `shots` binary | `crates/session/shell/src/bin/shots/` |
| Frame-loop metrics | `crates/session/shell/src/compositor/metrics.rs` |
| Presentation clock | `crates/session/shell/src/compositor/pacing.rs` |
| Render thread | `crates/session/shell/src/compositor/present.rs` |
| Load-generator client | `crates/session/shell/src/bin/tilo-loadclient.rs` |
| Scenario catalogue | `crates/session/shell/src/bin/shots/scenario.rs::catalogue()` |
| Offscreen renderer | `crates/sdk/ui-core/src/offscreen.rs` |
| `TILO_TEST_CONTROL` protocol | `crates/session/shell/src/compositor/test_control.rs` |
| Widget registry walk | `crates/session/shell/src/compositor/frame.rs::walk_widget` |
| Backend trait | `crates/session/shell/src/compositor/backend/mod.rs` |
| Headless backend impl | `crates/session/shell/src/compositor/backend/headless.rs` |

## Per-app screenshots

First-party apps using the shared launcher accept `--shots PATH` (and an
optional `--viewport WIDTHxHEIGHT`) and write a PNG without opening a window.
For example:

```bash
./target/debug/terminal --shots /tmp/terminal.png
./target/debug/file-explorer  --shots /tmp/files.png
./target/debug/browser --shots /tmp/browser.png
```

They flow through `quick_ui::offscreen::OffscreenRenderer` and run a single
`sync → build_layout → compute_layout → apply_layout → draw` cycle — no event
loop. Browser also offers focused deterministic chrome states and, with the
Chromium engine runtime installed, `--shots-live` for the real engine-to-frame
path.

## Per-app interaction: `--headless-drive`

`--shots` renders one cold-start frame and delivers no input, so it cannot
answer "does what the user did survive a restart?". Every `ui` app also
accepts `--headless-drive '<json>'`, which stands the real widget tree up over
a real Taffy tree and feeds it the same `WindowEvent` values winit would —
through the same `Widget::handle_event` a live click goes through — with no
window and no GPU.

```bash
# What can I address? (test_id, else the Parent/Child[index] path fallback)
./target/debug/monitor --headless-drive '{"targets":"/tmp/targets.json"}'

# Click two of them, then write the app's CRDT document out as JSON.
./target/debug/monitor --headless-drive '{
  "actions":[{"click_named":"Toolbar/Button[1]"},{"type":"cpu"}],
  "dump":"/tmp/after.json"}'
```

Actions: `click_named`, `click_at` (`[x, y]`), `key` (a named key or one
character), `type`, `modifiers`, and `invoke`. Optional `viewport` (`[w, h]`),
`dump` and `targets` paths. Exit codes: `0` all actions ran, `1` an action
failed (the reason is on stderr — a missing target is an error, not a silent
no-op), `3` persistence was unavailable.

`invoke` performs one action the app publishes, by address — no coordinates —
through the same resolution, availability, confirmation and strict argument
binding the live host applies:

```json
{"invoke": {"resource": "installer.review", "action": "begin",
            "confirmation_phrase": "/dev/vdb"}}
{"invoke": {"resource": "installer", "action": "next",
            "expect_error": "action_unavailable"}}
```

`args` is the typed argument object; `confirm: true` satisfies a `Confirm`
contract and `confirmation_phrase` a `Phrase` one. `expect_error` makes the
step pass only when the door refuses with that code (`unknown_action`,
`invalid_arguments`, `action_unavailable`, `action_confirmation_required`,
`action_failed`), so a scenario can prove a refusal as well as a success.
An optional `"results":"target/drive-results.json"` retains the actual return
from each explicit `invoke`, in order. The version 1 JSON envelope contains
`author` and `entries`; each entry carries its resource/action `address` and
one `status`: `completed` with the original typed `output` (`value` and
`resources`), `accepted`, or `refused` with the door's `code` and `message`.
`accepted` records admission, not background completion. An expected refusal
still appears as `refused` when its scenario step passes. Arguments and
confirmation phrases are not copied into this envelope; returned data and
error messages may themselves be sensitive, so retention is opt-in.

Retention admits at most 4,096 entries and 16 MiB of JSON. Exceeding a limit
fails the scenario and records `retention_error` with the retained prefix;
the last operation may already have run, and its omitted result must not be
interpreted as a refusal. The journal stays failed for subsequent invocations.
The file records invocations, not overall scenario success; check the exit status
and audit report as well. Without `results`, the existing behavior is unchanged.

For a framework resource's existing `render` action, the returned content
address identifies the actual offscreen PNG in the ordinary ContentStore.
Run with an explicit owned `--data-dir target/drive-profile` to keep those
bytes after exit (`target/drive-profile/content/<first-two-hash-characters>/<hash>`).
The default temporary profile is removed on exit. This is a render of the
imported state, distinct from a seeded `--shots` fixture. The normal desktop's
`tilo-control` already preserves returned action payloads; this option brings
the standalone scenario driver to the same semantic result contract.

From Rust, `Driver::invoke_action` is the same call, and
`Driver::deliver_external` delivers a `UserEvent::External` payload the way
the live host does. `crates/session/installer/tests/headless_wizard.rs` runs
the whole installer wizard this way against a recording install backend.

The run persists through the app's real store (`ui::persist`), so pointing
`XDG_DATA_HOME` and `TILO_PACKAGE_ID` at a scratch directory gives a complete
restart test.

## Persistence sweep

`scripts/persistence-sweep.py` does exactly that, for every app:

```bash
scripts/persistence-sweep.py                 # all apps
scripts/persistence-sweep.py ebook monitor   # named apps
scripts/persistence-sweep.py --json out.json
```

Each app is run three times against a scratch data directory — cold (no
input), driven (clicks/keystrokes, then exit), relaunched — and two things are
asserted: the input actually changed the document (otherwise a passing restart
proves nothing), and the document that came back off disk matches the one the
app had when it exited. Apps that need specific actions get them from
`scripts/persistence-scenarios.json`; otherwise the harness picks addressable
widgets out of the app's own live tree, skipping anything whose name suggests
a destructive operation.

## Undo, through the driver

`Driver` records undo boundaries the way the real host does, so a headless
scenario can drive an edit and then assert it is undoable — including an edit
made through an accessibility verb rather than a click.

This did not always work, and the way it failed is worth knowing. An app that
dispatches its edits through `tilo_actions` reports them via
`Widget::take_committed_action`; the driver never drained it, so no boundary was
recorded and any headless undo test was impossible to write honestly. (One was
written and deleted for exactly that reason — it could not observe its subject.)
Two further holes sat behind it: the boundary was recorded with no author while
undo is author-scoped, so a user's own Ctrl+Z **skipped** their button-driven
edit and reverted an older one instead; and only the root widget was drained,
because no container forwards that call.

All three are fixed in one place — `ui::undo::record_dispatched_action`, called
by both `App` and `Driver` — so the two hosts cannot drift. If you simulate the
host yourself in a unit test, use `record_committed_action_authored` (or that
function) rather than the anonymous `record_committed_action`: the anonymous
form produces an entry production no longer creates, so a test built on it
passes while proving nothing.

## Opening an app the machine does not have

`scripts/demo/install-on-open.py` drives the one flow that cannot be
exercised from an installed package, because its subject is a package
that is not installed: typing the name of an app the configured
registries offer, pressing Enter, and getting a tab.

```bash
cargo build -p shell -p sheets -p install -p launcher
cargo build -p app-cli --bins --examples

scripts/demo/install-on-open.py --out target/install-on-open
scripts/demo/install-on-open.py --backend winit --stage-only   # drive it yourself
```

It packs the real Sheets binary into a signed `.tapp`, serves it from a
loopback registry, points a headless compositor at an empty install
root, and then checks four things off the compositor's own state: that
the app is offered and absent, that its palette row is worded
identically to an installed app's, that `Enter` activates it, and that
the resulting tab presents frames of its own rather than the
first-frame placeholder. Three PNGs land in `--out`.

Two things it pins that are easy to get wrong by hand:

- The socket takes **winit** key names. `Enter` works; `Return` is an
  xkb name and comes back as a `parse_error`.
- **`Super+Space`**, not `Ctrl+K`. `Ctrl+K` scopes to the focused app, and
  only widens to global when nothing is focused — so it appears to work
  on an empty workspace and stops working once there is a tab in front
  of you. The script measures both scopes after the app is up, where
  the difference is visible.

## What's still missing

There is no known first-party app without the shared deterministic `--shots`
path. Hardware- and service-backed states still need separate installed-image
or live-provider evidence; a deterministic screenshot is visual evidence, not
proof that an external device, network service, or codec works.

## CI integration

The `shots --mode offscreen` flow runs cleanly in CI (no display, no GPU
forwarding) as long as the runner has a recent Mesa plus a software wgpu
fallback. The compositor's `--backend headless` does the same thing for
live-binary tests.

The control-socket integration test is ignored by default because its native
compositor opens a winit window. Run it from the repository checkout inside a
desktop session (or enter the development container first):

```bash
cargo build -p shell
cargo test --locked -p shell --test it control_socket_test:: -- --ignored --test-threads 1
```

The test spawns the binary itself. It is intentionally separate from the
offscreen lanes until the native compositor can run without a host display.
`control_socket_test` is a module inside the shared `it` test binary, not a
separate Cargo target. Keep the module filter when opting into ignored tests.

## Desktop UX journeys

The [UX testing bench](ux-testing-bench.md) runs native macOS desktop journeys with interaction budgets, layout/contrast checks, scroll evidence and HTML reports.
