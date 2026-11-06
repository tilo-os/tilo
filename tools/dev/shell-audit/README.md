# The everyday-interaction audit

A desktop has thousands of small interactions to get right: a press on a tab, a
right-click on the bar, Escape in a popup, the second app, the clock, the
notification. This tool runs the same catalogue of those interactions against
two desktops and pairs the results, so a difference is a picture and a number
instead of an opinion.

The two sides:

- **old** — the shell of any tree that still has `crates/session/shell`, run
  headless with its apps started as binaries beside it, the way that session
  ran. Default ref: the tree just before the session replaced it.
- **new** — the session of a tree that has `crates/session/session`, run through
  that tree's own `dist/dev/nested-session.sh`: its apps installed as packages
  and opened through its own launcher, on lavapipe, with its own surfaces (lock
  screen, quick settings, on-screen keyboard) and its own sound server.

Both sides start on the same home folder, filled with ordinary files
(`Documents/`, `Downloads/`, `Notes.txt`), so a Files story has something to
show. A dev session that starts without a surface it needs says so at the top of
`index.md` — that is a broken session, not a smaller one.

Each side is started and driven its own way, but a scenario never knows that.
It says what a person does, through a *door* (`sides.py`, `doors.py`); a door a
side does not have is recorded as missing, with what that side offers instead.
Nothing is skipped, and neither side is driven through the other's keys.

## Run it

```sh
python3 tools/dev/shell-audit/audit.py
```

The first run builds what each side does not have yet — the old tree's shell and
app binaries, and the new tree's nested-session — into `target/shell-audit`.
Each build runs `cargo` in that tree, or whatever the caller names in
`SHELL_AUDIT_CARGO`; the new side's build script can also be handed to another
machine through `SHELL_AUDIT_NEW_BUILD_EXEC`. Later runs reuse both.

Captures, manifests and the index land in `target/shell-audit/run` (out of git;
pass `--out` to put them elsewhere):

- `pairs/<scenario>/old.png`, `new.png` — the same story on each side;
- `manifest-old.json`, `manifest-new.json` — per scenario: what each step did,
  how long it took, what the desk answered, and the controls it listed;
- `index.md` — the two sides side by side, per-area totals, the doors each one
  has, and what a side could not do.

Useful arguments:

```sh
python3 tools/dev/shell-audit/audit.py --list                  # the catalogue
python3 tools/dev/shell-audit/audit.py --validate              # steps and doors, no start
python3 tools/dev/shell-audit/audit.py --only windows          # one area
python3 tools/dev/shell-audit/audit.py --only qs-open,bar-clock  # named stories
python3 tools/dev/shell-audit/audit.py --side old              # one side
python3 tools/dev/shell-audit/audit.py --theme light --size 1600x1000
python3 tools/dev/shell-audit/audit.py --keep                  # leave both running
python3 tools/dev/shell-audit/audit.py --index-only            # rebuild index.md
python3 tools/dev/shell-audit/audit.py --side new --only qs-open,bar-clock \
    --from DIR                                                # re-run a few stories
```

`--from` names an earlier run's output directory: the side this run does not
drive is taken from it, rows and pictures, so a run that re-checks a handful of
stories on one side still pairs them against the other side's frames. Give it
again for a run that holds only some of the stories — the first one that has a
story gives it — and the index says where each side was taken from. The
scenarios it lists, per-area totals included, are the ones the run covers.

When something is already built, hand it in rather than rebuilding it:
`--old-bin DIR` (the old shell's `shell`, `notes`, … binaries), `--new-build
DIR` (a `dist/dev/nested-session-build.sh` output), `--old-apps DIR` (installed
app packages for the old side). `--old` takes any git ref of a tree with
`crates/session/shell`; `--new` any ref of this one.

Nothing here touches the live desktop: each side runs on its own profile, its
own private session bus and a throwaway identity, headless.

## The catalogue

`catalogue.json` is the list, as data. One area per everyday subject (launching,
windows, tabs, strips, workspaces, keyboard, pointer, drag and drop, text, bar,
quick settings, notifications, search, lock and power, displays, files, the
dialogs an app asks for a file with, settings, apps, motion), each story a few
steps:

```json
{
 "id": "window-maximize",
 "story": "maximise the window in front",
 "steps": [{"door": "window.launch", "app": "notes"}, {"wait": 0.6},
           {"door": "window.maximize"}, {"wait": 0.7}],
 "capture": "a maximised window",
 "measure": ["changed pixels", "window rect"]
}
```

Steps name a door, a control (`"click": "notes"` finds the first control whose
name holds that text), a pointer position, or input (`key`, `chord`, `type`,
`client_key`, `client_type`, `drag`, `drag_tab`, `scroll`, `double_click`). A
story may also ask for `motion` (frames 16 ms apart, with the times they were
sent), `pointer: "keep"` (a hover story, so the pointer is not parked for the
capture), `desk: "keep"` (windows it opened stay for the next story) or a
`settle` before the capture.

Add a story to the catalogue when a person notices something a desktop should
do. Add a door to `doors.py` when a story needs a surface neither side has yet —
one method, both sides, and a `DoorMissing` with what the side offers instead.

## What the numbers mean

`total_ms` is the whole story including its steps and its capture;
`changed_px` is how much of the desk the story changed from before it started
(a story that ends where it began reads 0); `target_count` is how many named
controls the desk listed at the capture; `residual_px` is how much of the idle
desk was left changed after the tool closed what the story opened — a story
starts on a clean desk, or the tool dismisses again and says so. A surface that
answers an action before it draws gets up to twenty seconds, and the frame that
drew the most is the pair's picture — `index.md` says how long it took
(`late_s`), and a frame that drew too little is kept beside the pair as
`old-late.png` / `new-late.png`.

Timings are wall time on one machine, so compare the two sides within a run, not
across runs.

Each scenario ends in one of four states: `captured` (the story ran and the
frame was taken), `door_missing` (that side has no such surface, with what it
offers instead), `failed` (a surface answered and refused, or a step did not get
what it waited for) and `no_frame` (the side ran the story but would not show
its desk afterwards).

The audit does not judge looks. It records what each side did and leaves the
pictures for a person.
