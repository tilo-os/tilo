# Headless testing: captures, control, and a desktop with no display

This guide covers testing Tilo **without opening a window**: a session on the offscreen backend, the control socket that drives it, the Python harnesses that put the tree's own apps on its desk, and the ways to measure what the frame loop does. Every command below writes PNG and JSON to disk; nothing needs a display server attached to your terminal.

Four layers, each useful on its own:

1. **`tilo-session --backend headless`** — the real session binary, painting into an off-screen texture instead of a display, publishing the same sockets and files a login session does.
2. **`tilo-control` / `TILO_TEST_CONTROL`** — JSON over a unix socket: click, drag, type, invoke an action, snapshot, read the desk.
3. **`scripts/headless-examples/`** — short Python harnesses that spawn a session, drive it and put it down.
4. **`tilo-app dev` and `scripts/agent-loop/session.py`** — an app rebuilt on every source change, in a session of its own.

The session binary is `tilo-session` (`crates/session/session`, bin `tilo-session`). There is no `shell` crate: its desktop is the session's, its scenarios are `crates/session/session/tests/`, and what it published for the login is now published by the compositor and the session.

## What a session publishes

A session binds its Wayland socket, its doors and its files under `$XDG_RUNTIME_DIR/tilo-session/`:

| Path | Written by | Read by |
|---|---|---|
| `wayland-display` | the compositor, when it binds its socket (`Compositor::listen`) | everything that starts a program beside the session: `tilo-app dev`, `scripts/agent-loop/session.py`, `spawn-headless.sh` |
| `workspace-id` | the session, on every workspace switch | `tilo-app dev`, which joins that workspace; `scripts/agent-loop/session.py`, which waits for it |
| `agent.sock` | the session's agent door, for a session that owns the machine | `tilo-runtime-mcp --connect`, `tilolang repl --door` |
| `test-control.sock` | a development build, or `--test-control PATH` | `tilo-control`, every harness here |
| `admin-control.sock` | a session that owns the machine, for the device's owner | `tilo-control --socket …` (semantic ops only) |
| `headless-owner-control.sock` | a headless session started with `--headless-owner-control` | the same, for an owner's scripts |

`wayland-display` and `workspace-id` belong to the session the login's programs reach: a second session in the same runtime directory (a nested one beside a live desktop) publishes neither, and is reached through the socket name it printed on stdout. The display file, the workspace file and the doors go when the session does.

The session prints what it bound, so a harness reads the answer rather than guessing a path:

```
WAYLAND_DISPLAY=wayland-1
TILO_AGENT_DOOR=/run/user/1000/tilo-session/agent.sock
TILO_TEST_CONTROL=/run/user/1000/tilo-session/test-control.sock
TILO_ADMIN_CONTROL=/run/user/1000/tilo-session/admin-control.sock
```

A session started as a real login also exports `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP=tilo:GNOME`, `XDG_SESSION_DESKTOP=tilo` and `XDG_SESSION_TYPE=wayland` to the systemd user manager and D-Bus activation (`crates/session/session/src/env.rs`). A nested or headless run exports nothing: it must never repoint the surrounding desktop's user manager at its own socket.

## Layer 1: a session with no display

The same binary and the same code paths as a login; the backend paints into a wgpu off-screen texture. Nothing appears on your desktop, and no `DISPLAY` or `WAYLAND_DISPLAY` is needed.

```bash
# A session of its own: a scratch home, XDG tree, runtime directory and
# identity, plus the socket it answers on.
scripts/headless-examples/spawn-headless.sh &
SESSION=$!
trap "kill $SESSION" EXIT

# From here on, tilo-control finds the socket by itself:
target/debug/tilo-control windows
```

`spawn-headless.sh` takes its runtime directory from `dist/dev/nested-session.sh --print-runtime`, so the rule that picks a short private one lives in one place. It refuses a socket a live session still answers on, sets `TILO_TEST_CONTROL`, and prints `READY <socket>` once usable. Its knobs: `TILO_HEADLESS_BINARY` (default `target/debug/tilo-session`), `TILO_HEADLESS_PROFILE` (keep the scratch profile across runs), `TILO_HEADLESS_TIMEOUT`.

By hand, one session is three lines:

```bash
export TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/test-control.sock
cursor=$(mktemp -d)          # never /tmp itself: the runtime directory is the session's
export XDG_RUNTIME_DIR=$cursor/run HOME=$cursor/home
mkdir -p "$XDG_RUNTIME_DIR"
target/debug/tilo-session --backend headless &
```

The session's own flags, as `tilo-session --help`'s header documents them: `--backend drm|headless|nested`, `--size WxH`, `--output [NAME=]WxH[@X][,][+X,Y]` (repeatable, for a multi-output arrangement), `--scale S` or `--panel-mm WxH`, `--connector NAME`, `--shell screen`, `--session-bus ADDRESS`, `--system-bus ADDRESS`, `--no-x11`, `--test-control PATH`, `--admin-control PATH`, `--headless-owner-control`, `--admit-repl PATH`, `--seed-apps`, and `-- COMMAND…` to run a program under it (the session then lives as long as that command).

`--test-control` and `--seed-apps` are honoured by development builds only; a release build serves the control socket only when built with `--features dev-overrides`, which the dev image's session carries. Every harness here is a development build, and says so when it is not.

Headless lays frames out and answers clients' frame callbacks but does not rasterize them: nobody is looking. `{"op":"snapshot"}` renders on demand. A viewer that streams frames attaches with `{"op":"watch","watching":true}` (`tilo-control watch on`), or starts the session with `TILO_HEADLESS_WATCH=1`.

A headless session owns no seat. It starts no power service: no lid or power-key delegation, no suspend on idle, and neither offers nor admits Suspend, Restart or Shut down from the power menu, the palette or `session.*` actions. Lock stays, because it locks the session's own scene. Test idle suspend through `SessionManager` and `TILO_IDLE_SUSPEND_SECS`, which drive the state machine without an actuator.

### Put `TMPDIR` somewhere with room

`/tmp` is a tmpfs shared with everything else on the box, and a few concurrent `rustc` processes fill it. When they do, anything that asks for a temporary file fails — harnesses here allocate session roots through `tempfile` — and the error surfaces from inside that call as `Quota exceeded`, a bare exit 1, or empty output. It reads like a bug in whatever you were testing, which is the expensive part. Point it at real disk before a run that spawns sessions:

```bash
TMPDIR=$HOME/tmp scripts/headless-examples/full-loop.py
```

Keep session roots short (`/tmp/qs-xxxx`-shaped): a unix socket path has a hard ~108-byte limit, and a deep temporary directory silently pushes the control socket past it.

### Owner control with an existing identity

For semantic actions on an existing owner's headless session, start
`tilo-session --backend headless --headless-owner-control`. The normal
first-login, device-owner, administrator-group, lock and caller checks apply, and
no development control variable is needed. The endpoint is
`$XDG_RUNTIME_DIR/tilo-session/headless-owner-control.sock`, separate from the
real seat's `admin-control.sock`; an existing live listener is never replaced.

That door serves the semantic half of the protocol only — `actions list`,
`actions invoke`, `actions status`, `state-dump`, `windows`, `record-read`, the
profile ops — and never synthetic input, snapshots or the test identity.

```bash
tilo-control --socket "$XDG_RUNTIME_DIR/tilo-session/headless-owner-control.sock" actions list
```

A locked session still requires its normal unlock action, and physical cursor and display acceptance stay a real-seat test.

## Layer 2: `TILO_TEST_CONTROL` — drive a running session

Best for ad-hoc interactive testing from a script or an agent, exploratory "click here, snapshot that" flows, and integration tests that assert against the live session.

### Enable the socket

```bash
export TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/test-control.sock
target/debug/tilo-session --backend headless &
ls -la "$TILO_TEST_CONTROL"        # bound once it is there
```

The socket is 0600 in a 0700 directory the session creates. `--test-control PATH` names another path; `TILO_TEST_CONTROL` is what the tools here read.

### `tilo-control`

```bash
cargo build -p tilo-control          # lands in target/debug; images ship /usr/bin/tilo-control
alias qc='cargo run -q --bin tilo-control --'

qc actions list Calculator --json            # inspect search results without opening the palette
qc actions invoke applications dev.tilo.calculator
qc actions status INVOCATION_ID --wait       # follow a pending receipt to its result
qc list-targets                              # NAME (X, Y, W, H) per line
qc click add_workspace                       # click_named
qc click-at 400 300                          # click_at
qc drag 100 100 300 200 12                   # drag with 12 motion steps
qc key Enter                                 # a named key OR one character
qc type "hello"                              # one press+release per character
qc modifiers ctrl,shift                      # comma-separated set; empty clears all
qc snapshot /tmp/state.png                   # PNG to disk
qc windows                                   # open windows: workspace/strip, group, tab, app
qc state-dump --pretty                       # the live desk + chrome as JSON, without history
qc state-dump --history                      # … with effect receipts and audit trails
qc watch on                                  # stream frames, for a headless session drawing nothing
qc raw '{"op":"list_targets"}'               # escape hatch for unwrapped commands
```

Default socket: `$TILO_TEST_CONTROL`, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` (an error, not a `/tmp` fallback, when `XDG_RUNTIME_DIR` is unset). Override with `--socket /path`. Errors surface as a non-zero exit and a structured `code: message` on stderr.

### Wire protocol

Newline-delimited JSON; every command answers one line.

```bash
# List every clickable widget by name + rect:
echo '{"op":"list_targets"}' | nc -U "$TILO_TEST_CONTROL"

# Click a widget by name (resolved from the live widget tree):
echo '{"op":"click_named","name":"add_workspace"}' | nc -U "$TILO_TEST_CONTROL"

# Click at raw coordinates:
echo '{"op":"click_at","x":400,"y":300}' | nc -U "$TILO_TEST_CONTROL"

# Drag from one point to another (default 8 intermediate motions):
echo '{"op":"drag","x_from":100,"y_from":100,"x_to":300,"y_to":200,"steps":12}' | nc -U "$TILO_TEST_CONTROL"

# Set modifiers, send a key, clear them:
echo '{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"key","key":"k"}' | nc -U "$TILO_TEST_CONTROL"    # Ctrl+K opens the palette
echo '{"op":"key_down","key":"a"}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"key_up","key":"a"}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}' | nc -U "$TILO_TEST_CONTROL"

# Type a string (one press+release per character):
echo '{"op":"type","text":"hello"}' | nc -U "$TILO_TEST_CONTROL"

# Wheel and touchpad motion, as a device would send it (a notch up is a
# positive `dy`), and the touch gestures a session's seat understands:
echo '{"op":"scroll","dx":0,"dy":-30}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"swipe","fingers":3,"dx":0,"dy":-300}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"pinch","fingers":2,"scale":1.25}' | nc -U "$TILO_TEST_CONTROL"

# Capture a PNG of the current state:
echo '{"op":"snapshot","path":"/tmp/state.png"}' | nc -U "$TILO_TEST_CONTROL"

# The live desk + chrome as JSON. A workspace's append-only history (effect
# receipts, task audit trails) is left out; add "include_history":true to get
# it. `peers` is who is here: this session's own row, and one row per
# workspace it takes part in with everyone present there, as the roster every
# surface draws is read.
echo '{"op":"state_dump"}' | nc -U "$TILO_TEST_CONTROL"

# Just the open windows, one row per tab:
echo '{"op":"windows"}' | nc -U "$TILO_TEST_CONTROL"

# Pointer press and release as separate events. `click_at` bundles them,
# which cannot express a drag-and-drop: the source client has to be *holding*
# the button when it calls start_drag, and the drop happens on a release that
# arrives after the pointer has travelled to another window.
echo '{"op":"pointer_move","x":260,"y":140}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"pointer_button","pressed":true}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"pointer_button","pressed":false}' | nc -U "$TILO_TEST_CONTROL"

# An action by address, no coordinates, through the same resolution,
# availability, confirmation and argument binding the live host applies:
echo '{"op":"actions_invoke","resource":"notes.notebook","action":"new-note","arguments":{"title":"Trip"}}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"actions_enumerate","query":"notes"}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"actions_status","id":"INVOCATION_ID"}' | nc -U "$TILO_TEST_CONTROL"
echo '{"op":"actions_cancel","id":"INVOCATION_ID"}' | nc -U "$TILO_TEST_CONTROL"

# Records an app publishes, read through the record door as this person reads
# them, answered once they differ from `after` (the `revision` an earlier answer
# gave) or `wait_ms` passes:
echo '{"op":"record_read","app":"notes","records":["notebook"]}' | nc -U "$TILO_TEST_CONTROL"

# The compositor's own memory report (see "Idle memory"):
echo '{"op":"process_memory"}' | nc -U "$TILO_TEST_CONTROL"

# Mint the throwaway identity a development login opens its desk with:
echo '{"op":"provision_test_identity"}' | nc -U "$TILO_TEST_CONTROL"
```

The full set is [`Op::ALL`] in `crates/session/session/src/test_control.rs`, where the `Op` enum and its comments are the source of truth:
`actions_cancel`, `actions_enumerate`, `actions_invoke`, `actions_status`,
`click_at`, `click_named`, `drag`, `key`, `key_down`, `key_up`, `list_targets`,
`modifiers`, `pinch`, `pointer_button`, `pointer_move`, `process_memory`,
`profile_start`, `profile_status`, `profile_stop`, `provision_test_identity`,
`record_read`, `scroll`, `search_rows`, `snapshot`, `state_dump`, `swipe`,
`type`, `unlock`, `watch`, `windows`.

### Response shapes

```
Successful query (list_targets):
  {"targets":[{"name":"add_workspace","rect":[1234,56,32,32]}, ...]}

Successful action (click_*, drag, key, type, modifiers, snapshot,
                   pointer_*, scroll, swipe, pinch):
  {"ok":{"frame":0}}

Error:
  {"err":{"code":"unknown_target","message":"no addressable widget named ..."}}
```

Error codes: `parse_error` (malformed JSON or unknown command), `unknown_target`, `unavailable`, `unknown_action`, `unreadable`, `input_failed`, `stale_cursor`, `identity_exists`, `dev_image_only`, `provision_failed`.

A press is proven ready for the target it names and the widgets that target sits in, never for the desktop at large: a widget elsewhere asking for frames for its own reasons does not hold an unrelated target back. The desktop as a whole must still be settled — setup closed, no workspace, strip or window-group switch underway — because a rectangle read then is one the caller never saw.

### Widget targets — how `click_named` finds things

Every widget in the tree exposes a name through either:

- `Widget::test_id()` (opt-in): a stable name, e.g. `command_palette_input`, `strip_switcher`;
- a path fallback: `Parent/Child[index]`, e.g. `OverlayHost/TopBarChrome/Button[0]`.

`list_targets` returns every widget with a non-zero rect. The bare path is registered when there is exactly one sibling of that name; the indexed form always works. To make a widget addressable, override `Widget::test_id()` in `ui` to return `Some("my_stable_name")` — see `command_palette_input` in `crates/sdk/ui-core/src/action_palette.rs`.

## Layer 3: the example harnesses

`scripts/headless-examples/` holds short, stdlib-only scripts. Read them as templates; copy them into your own automation.

```bash
./scripts/headless-examples/full-loop.py
# spawns a session, opens the palette, types "Files", writes two PNGs and
# tears the session down.
```

- `spawn-headless.sh` — start a session on a scratch profile and wait for its socket
- `_control.py`, `tilo_client.py` — the two-line client every script uses
- `reaper.py` — takes a session down, process group and all
- `list-targets.py`, `click-and-snapshot.py`, `keyboard-flow.py`, `drag-demo.py`, `full-loop.py` — one op each
- `action-api-demo.py` — discovery and semantic invocation by `(provider, id)`
- `first-frame.py`, `startup-frame-timeline.py` — what a session's first frame costs
- `session-e2e.py`, `canvas-e2e.py`, `tilolang-e2e.py`, `files-clipboard-e2e.py`, `agent-chat-e2e.py` — whole flows, each on a session of its own
- `mcp-live-proof.py`, `agent-seats-proof.py`, `session-lock-proof.py` — the proofs `tools/ci/session-proofs.sh` runs
- `notes-persist-proof.py`, `notes-caret-proof.py`, `desk-restore-proof.py`, `audio-absent-proof.py` — proofs that need the tree's apps built

`scripts/headless-examples/README.md` says what each proof asserts.

### Select search results by identity

Use `actions list QUERY --json` to inspect the same providers the palette uses. Enumeration starts asynchronous index refreshes; a cold query can be incomplete. Retry within a bounded deadline when the expected app is missing, and report it as unavailable if the deadline expires. Opening the palette is unnecessary.

Titles are display text, not identities. Two results can both say "Open Calculator":

| Provider | ID | Meaning |
|---|---|---|
| `applications` | `dev.tilo.calculator` | Native Tilo Calculator |
| `applications` | `org.gnome.Calculator` | Host GNOME Calculator |

Inspect `kind`, `subtitle` and the `(provider, id)` pair before choosing, keep the pair, and pass both to `actions invoke`; never substitute the first same-named result when the intended app is absent. `score` ranks results but does not establish identity.

After launch, enumerate again and inspect the app actions' `application`, `resource`, `operation`, `params` and `contract` fields. Invoke the returned pair with its declared arguments, adding `--wait` so the answer is the result and not the admission. To follow a pending receipt, use `actions status ID --wait`, never a sleep-and-retry loop. Then read back the affected resource or the desk's state to verify the effect. A descriptor alone does not prove execution, and a visible tile alone does not prove the app has rendered. A packaged app is up when a process of it runs from the session's store — `/var/tilo-apps/<application id>/`, the only place its binary sits inside the sandbox — and the desk holds its window (`windows`); what it draws is proved by a control it offers (`list-targets`) pressed, and what that changes. `real_apps.Session.answering` waits for the first two and leaves the last to the check.

## A session with the tree's own apps

The recipes above start the session alone: it lists no applications, so a click on Notes in the desk finds nothing to start. Showing a real app's window with real content needs a session that has the apps *installed* and starts them through `tilo-launcher` and its sandbox, as an image does. One harness does that, `scripts/headless-examples/real_apps.py`; the checks that drive apps (`tilolang-e2e.py`, `canvas-e2e.py`) are built on it.

```bash
# Once per tree: the session's programs, the apps, and the runtime they link.
# Through a remote-exec prefix when this machine has no toolchain for it:
export TILO_NESTED_SESSION_BUILD_EXEC="…"   # optional
python3 scripts/headless-examples/real_apps.py --start --build --open notes --shot /tmp/notes.png

# Afterwards every start reuses that build (seconds) and installs nothing twice:
python3 scripts/headless-examples/real_apps.py --start --open notes --shot /tmp/notes.png
python3 scripts/headless-examples/real_apps.py --start --open file-explorer --open chat --keep
```

The build takes the app list from `TILO_NESTED_APPS`. Through a remote-exec prefix, pass it in the program's own environment — `<prefix> env TILO_NESTED_APPS="notes …" dist/dev/nested-session-build.sh`, which is what the harness does — because a builder runs with its environment, not this machine's.

`--start` starts `dist/dev/nested-session.sh` headless on lavapipe alone (`--software`: it never opens this machine's GPU) on a private runtime directory and a throwaway identity. The session also starts its own surfaces — the lock screen, the quick settings panel and the on-screen keyboard, beside its executable — and its own sound server: `pipewire`, `pipewire-pulse`, `wireplumber` and `pw-cli` must be on `PATH`, and it refuses to start without them rather than start as half a desktop. That sound is the session's own: its WirePlumber runs without the audio and Bluetooth monitors, so the only output is the session's null sink and the machine's cards and Bluetooth stay the host's. The apps it installs are `--apps` (default: Notes, Files, Chat, Settings, Terminal), signed with the session's own key and installed through `tilo-install` into the session's store — the same path the image's `/usr/share/tilo-apps` is built through. `--open APP` launches it through the desktop's own launcher (`applications so.tilo.<app>`) and waits until a process of it runs from the session's store with its window on the desk; `--shot` writes a PNG of the whole desktop. `--keep` leaves the session running and prints its ready JSON — the control socket, the runtime directory and the state directory — so `tilo-control` drives it from anywhere:

```bash
TILO_TEST_CONTROL=<control from the ready line> <build>/bin/tilo-control actions list
kill -TERM -<pid>   # the session's own process group: its apps, services and bus too
```

Never start an app binary by hand and never run `tilo-launcher` outside a session: an app launched that way has no window to show (`no runtime host found`), and the launcher refuses one that is not in a session's scope. From Python, mount the same harness instead of a second one:

```python
from real_apps import Session, prepare

prepare(state, apps="notes", built=False)          # idempotent
with Session(state) as session:                    # fresh desktop, headless
    session.start()
    session.launch("notes")                        # through the launcher
    session.answering("notes")                     # its process runs, its window is on the desk
    session.snapshot(out / "notes.png")
```

`session.click_named(name)` presses an app's own control, `session.wait_target(name)` waits for one to be laid out, `session.windows()` reads the desk, `session.type(text)` and `session.key(...)` type, `session.actions()` and `session.invoke(provider, id)` read and invoke what the apps publish. There is no `foreign_frames` op and `list_targets` names dedupe across surfaces, so readiness is the process plus the window, never a frame op.

## An app under development: `tilo-app dev`

`tilo-app dev APP_DIR` builds an app, installs it into a private dev session and restarts it on every source change, so an agent (or a person) can drive the app it is writing. It joins the workspace the session publishes in `workspace-id`, on the display from `wayland-display`.

`scripts/agent-loop/session.py` wraps that for an agent: `start` puts a headless session and a `tilo-app dev` on a private home, runtime profile and identity; `mcp` is the MCP endpoint for that session on stdio; `usage` and `report` say what each connection cost and what the agent made, from the app's own `history`, `inspect` and `look` actions.

```bash
scripts/agent-loop/session.py start apps/canvas
scripts/agent-loop/session.py mcp canvas     # in the model harness's MCP config
scripts/agent-loop/session.py usage canvas --calls
scripts/agent-loop/session.py stop canvas
```

The agent's only door is the ordinary one: `tilo-runtime-mcp` over the app's published actions, and the app's window as a person uses it. Everything lives under `target/agent-loop/APP` (state, logs) and a private directory in `$XDG_RUNTIME_DIR`; the real home, settings, identity and workspaces are never read or written.

## Per-app screenshots and drives

Apps built on the shared `ui` toolkit accept `--shots PATH` (and an optional `--viewport WIDTHxHEIGHT`) and write a PNG without opening a window:

```bash
./target/debug/notes --shots /tmp/notes.png
./target/debug/file-explorer --shots /tmp/files.png
```

They run a single `sync → build_layout → compute_layout → apply_layout → draw` cycle through `ui::offscreen` — no event loop.

`--shots` renders one cold-start frame and delivers no input, so it cannot answer "does what the user did survive a restart?". Every `ui` app also accepts `--headless-drive '<json>'`, which stands the real widget tree up over a real Taffy tree and feeds it the same `WindowEvent` values winit would, through the same `Widget::handle_event` a live click goes through — no window, no GPU.

```bash
# What can I address? (test_id, else the Parent/Child[index] path fallback)
./target/debug/notes --headless-drive '{"targets":"/tmp/targets.json"}'

# Click two of them, then write the app's document out as JSON.
./target/debug/notes --headless-drive '{
  "actions":[{"click_named":"Toolbar/Button[1]"},{"type":"cpu"}],
  "dump":"/tmp/after.json"}'
```

Steps: `click_named`, `click_at` (`[x, y]`), `tap_named`, `tap_at`, `swipe_at`, `key`, `type`, `modifiers`, `perform` (an accessibility verb on a named target: `activate`, `set_value`, `expand`, …), `invoke`, `wait_for_named`, `expect`. Options: `viewport` (`[w, h]`), `dump`, `targets`, `results`, `capture`, `window_bar`, `scenario`, `resources`. The whole grammar is `crates/dev/headless-schema/src/lib.rs`. Exit codes: `0` all steps ran, `1` a step failed (the reason is on stderr — a missing target is an error, not a silent no-op), `3` persistence was unavailable.

`invoke` performs one action the app publishes, by address, through the same resolution, availability, confirmation and strict argument binding the live host applies:

```json
{"invoke": {"resource": "notes.notebook", "action": "new-note", "args": {"title": "Trip"}}}
{"invoke": {"resource": "installer", "action": "next", "expect_error": "action_unavailable"}}
```

`confirm: true` satisfies a `Confirm` contract and `confirmation_phrase` a `Phrase` one; `expect_error` makes the step pass only when the door refuses with that code, so a scenario can prove a refusal as well as a success. `results` retains what each `invoke` returned, in order, within the limits documented in `crates/sdk/ui-core/src/headless/driver.rs`. `bind` keeps an answer for later steps as `{{name.path}}`.

The same driver runs in Rust, which is how app tests are written: `HeadlessApp` (`ui::headless`) mounts a component in one `FrameHarness`, and `drive(&[Step])` runs the same steps `--headless-drive` runs — one language for the harness, the CLI and a shipped scenario. `TestEnv` (`tilo_test_env`) is the machine around it: a home in a temporary directory, a clock fixed at `CAPTURE_LOCAL_TIME`, a network closed to external hosts until `allow_external`, and `with_files` for fixtures.

Assert on the record, not the widget: the body the store holds, the history entry's author, the undo that reaches the starting state. A test that reads internal fields of the widget proves nothing a user, a script or an agent can observe.

`Driver::invoke_action` is the same call from Rust, and `Driver::deliver_external` delivers a `UserEvent::External` payload the way the live host does. `apps/imager/tests/destination_headless_wizard.rs` runs the whole installer wizard this way against a recording install backend.

## Screenshots of every app

`tools/screenshots/run.py` photographs every app of the default collection in realistic situations, set up only through each app's actions. A plan per app, `tools/screenshots/<app>.toml`, lists situations; each is a name, a one-line title and the steps that reach it (`invoke`, `wait_for_named`, `expect`, `perform`; no clicks or keys):

```toml
[[situation]]
name = "week-with-events"
title = "A calendar week with events"

[[situation.steps]]
invoke = { resource = "calendar.calendar", action = "new-event", args = { event = { title = "Design review", start = "${MON+1}T10:00", end = "${MON+1}T11:00" } } }

[[situation.steps]]
invoke = { resource = "calendar.calendar", action = "show-week" }
```

Dates are written relative to the day of the run (`${TODAY}`, `${TODAY+3}`, `${MON+2}`, this week's Wednesday), so a week or an agenda has content whenever the shots are taken.

Each situation runs the app once, headless, in a fresh profile (its own HOME, XDG directories and runtime directory, no display, no session bus), and takes four pictures: dark and light, wide (1280x800) and compact (420x800). An app without a plan is photographed as it opens.

Each app's directory (`<out>/<app>/`, pictures named `<situation>-<theme>-<size>.png`) holds `manifest.json` (its situations, titles and pictures) and `resources.json` (what its plan can invoke); `summary.json` at the top lists every situation that failed, with the end of its log, and `<situation>.log` beside the pictures has the whole log. With a build of the apps at hand (`tools/screenshots/build.sh DIR` makes one), the runner takes one app or all, on the software renderer; on a laptop take the GPU lock:

```bash
flock ~/TiloData/.gpu-lock tools/screenshots/run.py --bin-dir DIR/bin --lib-dir DIR/lib \
    --out target/app-screenshots --apps "notes calendar"
```

## Measuring the frame loop

The session carries per-tick instrumentation — cadence, per-phase durations, input-to-present latency — behind env vars, and a harness that drives a measured run:

```bash
# 8 s idle run on the headless backend; prints the frame-time distribution.
scripts/perf/frame-loop-bench.py --seconds 8

# Under load from a client that commits a new buffer every frame, with a
# click injected every 50 ms so latency is measured.
scripts/perf/frame-loop-bench.py --seconds 8 --load fullscreen --input-hz 20

# The nested-Wayland backend, inside a headless `cage` so no window appears
# on your desktop. The only local way to exercise the render thread — the
# headless backend deliberately presents inline.
scripts/perf/frame-loop-bench.py --seconds 8 --backend nested --nested

# Force a non-60 Hz cadence when no such panel is attached.
scripts/perf/frame-loop-bench.py --seconds 8 --refresh-hz 144

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
| `TILO_PAINT_STATS=ticks` | the same, plus a `paint-tick: painted=… woke=…` line per iteration |
| `TILO_LOOP_TIMING=1` | log any tick over 100 ms with a phase breakdown |

`TILO_RUN_SECONDS` matters for a scripted run: the trace and summary are written by the normal exit path, so a `SIGKILL`ed session loses them.

For a source-matched fixture, opt into `TILO_FRAME_CPU_TRACE` through its supported launch configuration, with a new, separate file in the fixture owner's private directory; an existing file is refused instead of overwritten. Join the companion's `seq` and `start_us` to the original rows. It records no application payload, titles, paths or identities. The companion measures calling-thread and aggregate process CPU separately for layout, scene construction, submission and the layout-start-to-submit-return interval; process CPU includes parallel renderer and other background threads, so a low thread CPU value alone cannot identify GPU waiting, host scheduling or TCG overhead. `present_async=1` means submit timings cover enqueueing, not the render worker's completion. `anchor_unix_us` is bracketed by guest `CLOCK_MONOTONIC` readings; `anchor_offset_us` measures tick-start-to-`Instant` sampled inside that bracket. Unavailable CPU or monotonic clocks are blank, not zero.

### What an agent waits for

`scripts/perf/agent_rtt.py` times the control socket: `actions list`, `state dump`, a no-op invoke, throughput under concurrent clients and the same no-op through the MCP server. `--socket PATH` drives a running session; `--spawn SHELL` starts one through `spawn-headless.sh` and removes it on exit.

`scripts/perf/agent_task.py` times a whole task the way an agent in the login runs it: a scripted, streamed fake model drives `tilo-runtime-mcp --connect` through the session's agent door, creates a note in Notes, writes its body, finds it by title and reads it back, and the record splits the time between Tilo, the model and the agent loop. It needs a session where apps start, with Notes installed.

## Idle wakeups and idle memory

An idle desktop should leave the CPU asleep and hold little memory.

`scripts/vm-acceptance/run.py` measures the installed desktop: an idle session's compositor, its apps and the shared workspace's broker and shovels are sampled from `/proc` over a window, and `idle.wakeups_per_s.{shell,shell_threads,tilo-ush.four_apps}` record context switches per second beside `idle.frames` and `idle.loop_ticks_per_s`, which say the loop parked and painted nothing. Its budgets are in the same file (`BUDGETS`).

Headless covers what headless runs. Services and out-of-process apps are held by per-thread probes in their own crates: `tilo_test_env::idle::{spawned_thread, assert_stays_asleep}` for one service thread of a unit test, `idle_wakeups(pid, settle, window)` for a process tree, and `sample_process` with `Wakeups::between` for a stretch the test drives itself. `Wakeups::blocking_per_second` counts only voluntary switches, one per wakeup, together with a CPU bound that catches a thread spinning without blocking; `assert_stays_asleep` counts the times the thread was scheduled in, not its context switches.

The session's memory is read from its own socket: `raw '{"op":"process_memory"}'` sums `mincore` over `/proc/self/maps` per kind of mapping (`heap_main`, `heap_arenas`, `thread_stacks`, `anon_other`, `gpu_memfd` for lavapipe's device memory, `fonts`, `binary`, `libraries`, …), with glibc's in-use and free bytes and the threads by name (`crates/session/compositor/src/memory_report.rs`). It is the one that works on Tilo's kernel, which has no smaps (`CONFIG_PROC_PAGE_MONITOR=n`), so vm-acceptance R01 records the same report. Where a test runs on a kernel with smaps, `/proc/<pid>/smaps` beside it gives RSS, PSS and private dirty pages per mapping.

### Correlating app startup attempts

The session's `tilo::launch_latency` events carry `launch_id` and `launch_generation`. The generation is a diagnostic timestamp fixed at that process's first launch observation; it changes on a session restart and is not a VM layer generation or an authority token. Group events by both fields, not by app name: several windows can launch the same app concurrently.

With `tilo::launch_latency=info` in the session's `RUST_LOG` filter, each attempt reports request, spawn, map, first presented frame or retirement with its elapsed time and whether it spawned or mapped. Retirement can be placement cleanup and does not establish a process failure. The always-recorded `app-spawn:` and `app-exit:` lines carry the same pair plus the launcher PID; `launch_id=None` means there was no staged window timing record. Flight-recorder replay time and a saved timeout card do not establish the timestamp or cause of a fresh failed launch. This telemetry records no document, workspace, credential or grant contents.

## Liveness: is the loop turning?

A dev-build session also binds `<control socket>.live`, answered on its own threads, so it still answers when the main loop is deadlocked — which the control socket cannot:

```bash
printf '{"op":"heartbeat","budget_ms":500}\n' | socat - UNIX-CONNECT:"$TILO_TEST_CONTROL.live"
# {"alive":{"ticks":812,"latency_ms":0.2}}  or  {"stalled":{"budget_ms":500,"threads":[...]}}
printf '{"op":"threads"}\n' | socat - UNIX-CONNECT:"$TILO_TEST_CONTROL.live"
```

A heartbeat asks the loop, through a calloop ping source, to acknowledge a wake. One it does not acknowledge within the budget is a stall, and the answer carries every thread's state and kernel wait channel; when the main thread is blocked it also carries the backtraces of the threads blocked in a system call. A main thread still working is not interrupted, and neither is a thread that is running, faulting in a page or inside the allocator, because a backtrace is captured by a signal handler that allocates.

Every build, release included, also runs the main-loop watchdog (`crates/session/liveness/src/watchdog.rs`). It needs no client: a thread of its own notices a session thread that has slept without running once, outside the loop's wait, for longer than any wait it makes is bounded at (`LONGEST_BOUNDED_WAIT`, plus one sample), writes the same thread dump to stderr and the journal, and exits with status 70, which hands DRM master and the VT back and ends the session at the greeter. A loop that is working, starved of CPU, idle, stopped, under a debugger or in the kernel's uninterruptible sleep is never ended, nor one sampled across a suspend. `main_thread_reads` (`crates/session/session/tests/main_thread_reads.rs`) is the audit that keeps the loop's own threads off paths that read the desk.

## Where to look in source

| What | Where |
|---|---|
| Session binary and its flags | `crates/session/session/src/main.rs` |
| What the session publishes | `crates/session/compositor/src/lib.rs` (`listen`), `crates/session/session/src/published_workspace.rs`, `crates/session/session/src/env.rs` |
| `TILO_TEST_CONTROL` protocol | `crates/session/session/src/test_control.rs` |
| `tilo-control` | `crates/session/control-cli/` |
| Liveness door and watchdog | `crates/session/liveness/src/` |
| Headless backend | `crates/session/compositor/src/backend/headless.rs` |
| Frame-loop metrics | `crates/session/frame-metrics/src/lib.rs` |
| Memory report | `crates/session/compositor/src/memory_report.rs` |
| App headless drive | `crates/sdk/ui-core/src/headless/` |
| Offscreen renderer | `crates/sdk/ui-core/src/offscreen.rs` |
| Example harnesses | `scripts/headless-examples/` |

## What is still missing

There is no known first-party app without the shared deterministic `--shots` path. Hardware- and service-backed states still need separate installed-image or live-provider evidence: a deterministic screenshot is visual evidence, not proof that an external device, network service or codec works.
