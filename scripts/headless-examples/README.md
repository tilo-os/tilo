# Headless e2e examples

Scripts for controlling the headless compositor through the `TILO_TEST_CONTROL` Unix socket.

For app automation, begin with `real_apps.Session.actions(QUERY)` (the
session's catalogue, as action frames), then `invoke_row` the chosen row. The [search selection guide](../../docs/headless-testing.md#select-search-results-by-identity)
explains how to distinguish same-named native and host apps and verify execution
without opening the palette. Use the input examples below when testing input or
rendering itself.

## Proofs

These proofs start headless sessions of their own, on scratch homes, and read
every claim back off those sessions rather than off the thing that asked for
it. `tools/ci/session-proofs.sh PROOF` runs one on the session build
`dist/dev/nested-session-build.sh` made; t runs each as the check `e2e/PROOF`
(`t test e2e/two-people`, `t test 'e2e/*'` for all five), and
`t outputs e2e/PROOF --to DIR` fetches its verdict, screenshots and logs.

| Proof | What it asserts |
|---|---|
| `tilolang-e2e.py` (`e2e/tilolang`) | The tilolang apps installed and run as real apps: their registry entries, sandboxes, hosts, windows, actions and routes. |
| `hosted-names-proof.py` (`e2e/two-people`) | Two people in one workspace, in two sessions, read each other by name in tilolang Chat: "Ada is typing…", her message under her name, and her own window naming her. |
| `mail-two-members-proof.py` (`e2e/mail-two-members`) | Two members of one workspace, in two sessions, both list a message brought into the workspace's mailbox through the tilolang Mail's own action, and each opens it and presses Reply to a composer answering its sender under "Re:" its subject. |
| `agent-sessions-proof.py` (`e2e/agent-sessions`) | Two agents at the session's MCP server, each admitted as itself through the workspace's governance, each served its own tools, the shell's Stop ending one of their connections alone. |
| `session-lock-proof.py` (`e2e/session-lock`) | The shell's own lock action locks the session, the lock screen's own frame covers the shell, a password that is not the person's is refused, and the person's own password brings the shell back. |

`agent-files-story.py` drives a desktop with the tree's own apps as an agent
does, through the session's MCP server and nothing else: it names Files as the
listing does and starts it by one of its actions, finds the device and folder
Files' changes run in from Files' own read, renames a file with
`files.folder#rename` and reads the new name off the disk, sends the file into
a Chat conversation with Chat's `send`, and turns do not disturb on until the
bar's bell says so. The person only answers the sheets that ask about the agent.

`notes-persist-proof.py` is the same kind of proof for a desktop with the
tree's own apps: it opens Notes, types into a page, stops the session and starts
it again, and reads the text back off the record on disk. It needs the session
and its apps built (`dist/dev/nested-session-build.sh`).

`notes-caret-proof.py` opens Notes the same way, types a page with no input
method running, and checks that the shell holds the body's caret, that it follows
the typing, and that Super+V, pressed the moment the caret moves, opens the
clipboard pane under that caret's line, its first frame as tall as its page.

## First-hour walks

Nothing a person sees may be lost when code is deleted: the Rust shell for the
tilolang one (`apps/tl/shell`), a Rust app for its tilolang port, the session
moving onto the device daemon. Each walk is one thing a person meets in their
first hour — the launcher, the switcher, search, quick settings, an app — and
says what they do and what they must see. `app-walk.py` walks it in every
version its step file names that is still in the tree, one headless session
each, snapshots every step (the marked ones in the dark theme too), and holds
what it found to the walk's baseline from main.

**A lane runs one command, before it pushes:**

```bash
scripts/headless-examples/walk.sh switcher          # red if a step main walked is not walked here
scripts/headless-examples/walk.sh --list            # the walks there are
```

It runs the t check `app-walk/<walk>` on this tree, brings its outputs back to
`target/walk/<walk>/now`, the baseline run's to `target/walk/<walk>/baseline-<run>`,
and writes `target/walk/<walk>/index.md`: every step, main's shots beside this
tree's, and the comparison. The check is red when a step walked on main is not
walked here (a control, a state, a window a person waited for is missing, or a
step went over its `budget_ms`), or takes twice as long and a second more.
Controls drawn on main and gone here are listed for the person reading the shots.

**Names do not matter.** A walk is of the switcher, not of a binary: its step file
names versions (`rust`, `tl`) and, per version, what differs — how the session
starts (`"session_args": ["--shell", "screen"]` for the tilolang shell), the app
it opens (`id`, a native app of the session build or a tilolang `package`) and,
where they differ, the controls. A version whose `source` (by default its
package's directory) is gone from the tree is not walked, and a version the
baseline does not have is held to the version a person got on main (the step
file's `live`). So the lane that deletes `apps/tasks` runs `scripts/headless-examples/walk.sh tasks`
before and after unchanged: after, only the tilolang Tasks is walked, and it is
held to the Rust one's baseline.

**Families share a session.** Walks that name the same `family` (`shell`, `apps`,
`settings`) run in one session per version, one after another, with a reset between
them (windows closed, nothing over the desktop, notifications cleared, the light
theme); `order` puts a walk that locks the screen last. t runs one check per family
and one per walk in none, and each walk reports how long it took (`walk_ms`) beside
the session's start (`session_start_ms`).

**Writing a walk.** One file per walk under `app-walks/`, named in t.toml's
`app-walk/{app}` matrix (a test holds the two together). A step is
`{"id", "said", "ops", "dark"?, "budget_ms"?}`; ops are the methods `op_*` of
`app-walk.py`'s `Walk` — `must` (what has to be drawn: a name, an identity, or a
list of alternatives), `must_not`, `press`, `click`, `type`, `key`, `chord`,
`open` (another app, by directory name), `do` (an action by its address, red when
refused), `read` (an action the app publishes, held to the paths or words a
person made), `act` (an action by resource and operation, its answer kept for a
later step), `serve` (a stand-in, and test pages on this machine's loopback),
`image` (a picture for an app that shows photographs), `windows`, `search`,
`theme`, `wait_for`… `scripts/headless-examples/walk.sh WALK
--discover` keeps every step's controls and the desktop's actions in `walk.json`,
for naming the control that is really there.

**Several devices of one person.** A walk naming `"devices": ["laptop", "phone"]` runs a
headless session per device on the same host, each on its own state and device key. The first
device is the walk's own session; each further one is added the way a person adds a device:
the first shows Add a device, the new device's requester takes the link and prints its code,
the code is approved on the first device, and the new device starts as that person and is
shown the first device's workspace. They find each other on the host's loopback. Every op
runs on the first device unless it says `{"on": "phone", "must": [...]}` or its step says
`"on": "phone"`, and every step is shot on each device. An entry such as `{"name": "laptop",
"session_args": ["--output", "B=1280x720"]}` starts that device differently. `use` runs an
action found by what it acts on (`kind`, `label`, `action`) rather than by an instance's id,
and `keep` holds a value from its answer for a later op on any device (`{revision}`);
`has_file` finds a file a device was sent or synced in its own state.

**The baseline** is `app-walks/baseline/<walk>.json`: per version, per step,
whether main walked it, how long it took and the controls it drew, and the t run
that keeps main's shots. `scripts/headless-examples/walk.sh WALK --write-baseline` on main writes it;
a lane that fixes a step main could not walk refreshes it in the same push.

`app-walk.py WALK --out DIR [--only rust|tl] [--compare [FILE]]` runs a walk
without t, on a session build of this machine (`--build` makes one).

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
performance job compiled) starts it through `real_apps.ScriptedSession`, which
keeps the same rules: a scratch profile, the runtime directory
`dist/dev/nested-session.sh --print-runtime` names for it, and nothing left
behind.

## Examples

Each script drives a running session: the socket path is its first argument, else `$TILO_TEST_CONTROL`, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`. `real_apps.py` is the client they all use (`Session.attach` for a session another tool started, `ScriptedSession` for the tree's own build on a scratch profile, `Agent` for the session's MCP server).

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
