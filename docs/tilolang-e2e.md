# tilolang apps end to end

One command builds the tilolang example apps, installs them into a headless
nested Tilo session and checks them as real, independent Tilo apps:

```sh
scripts/headless-examples/tilolang-e2e.py            # builds what it needs
scripts/headless-examples/tilolang-e2e.py --no-build # reuses target/nested-session
```

Without `--no-build` it builds the nested session on t
(`t outputs nested-session-arm64`, through `dist/dev/nested-session.sh`) and
the `tilolang` tool and `tilo-collab-probe` with cargo. `--no-build` needs
`target/nested-session`, `target/debug/tilolang` and
`target/debug/tilo-collab-probe` to exist already.

The session is headless and runs with `--software`: the shell, its render
service and every app render on lavapipe, and the shell probes no GPU. It
never opens a window on the desktop it runs from and never captures that
desktop. The session's state is thrown away: it lives in
`target/tilolang-e2e/state`, and the script stops the session, its compositor
and its runtime directory at the end. `--keep` leaves it running.

## What it checks

`tilolang build apps/tl/board --search apps/tl` builds Notes, Chat, Todo and
Board. The session installs them (`--install-tapp`), then:

- **registry**: each package is an application under its own id, name and
  category (`actions list`, provider `applications`).
- **launch**: each opens from the applications provider, and the compositor
  admits its window under its own app id, which a sandboxed client gets only
  through verified launch custody. Each window presents a frame.
- **processes**: each app is its own `tilolang-app` process from the tilolang
  runtime bundle, in its own user, mount and pid namespaces. No process loads
  a Vulkan driver except lavapipe.
- **actions**: each app publishes its actions. Invoking "New note", "New
  chat" and "New item" each adds an entry to that app's history.
- **snapshots**: one PNG of the session per app, with that app in front.
- **idle**: context switches of each app's threads over 10 s, untouched.
  Expected: zero.
- **routes**: a second start of the same session installs the collaboration
  probe in place of Board and Todo. Board's surface declares that it reads
  Notes' kinds; Todo's declares no read. The broker lets Board join Notes'
  route read-only and refuses its write. Todo cannot join at all.

## Output

Everything goes to `target/tilolang-e2e` (`--out DIR` changes it):

- `summary.json` holds every check's result and the list of failed checks. The
  exit status is non-zero when a check fails.
- `notes.png`, `chat.png`, `todo.png` and `board.png` are the snapshots. Look
  at them: each should show only native window chrome around the app.
- `session-apps.log` and `session-routes.log` are the session script's own
  output.
- `shell-apps.log` is the shell's log from the first start, with each app's
  output. `state/logs/shell.log` is the shell's log from the routes start.

To drive a kept session by hand, use the `drive it:` line in the session log.
It sets `XDG_RUNTIME_DIR` and `TILO_TEST_CONTROL`.
