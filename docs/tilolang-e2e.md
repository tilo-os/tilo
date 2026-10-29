# tilolang apps end to end

One command builds the tilolang example apps, installs them into a headless
nested Tilo session and checks them as real, independent Tilo apps:

```sh
scripts/headless-examples/tilolang-e2e.py            # builds what it needs
scripts/headless-examples/tilolang-e2e.py --no-build # reuses target/nested-session
```

Without `--no-build` it builds the nested session through
`dist/dev/nested-session.sh` (on the rcargo builder, or on t without rcargo)
and the `tilolang` tool and `tilo-collab-probe` with cargo. `--no-build` needs
`target/nested-session`, `target/debug/tilolang` and
`target/debug/tilo-collab-probe` to exist already.

The session is headless and runs with `--software`: the shell, its render
service and every app render on lavapipe, and the shell probes no GPU. It
never opens a window on the desktop it runs from and never captures that
desktop. The session's state is thrown away: it lives in
`target/tilolang-e2e/state`, and the script stops the session, its compositor
and its runtime directory at the end. `--keep` leaves it running.

## What it checks

`tilolang build apps/tl/today --search apps/tl` builds Notes, Chat, Todo and
Today. The session installs them (`--install-tapp`) beside native Notes
(`--apps notes`), then:

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
- **hosted-here**: one PNG of the session per app as first opened, with that
  app in front (`first-*.png`). The session's own workspace is a room this
  device governs and hosts itself, so the shell dials no host for it and
  tells its apps nothing about one (`room_sync` in `state-dump` is `null`):
  each shows its own state at once, as native Notes does.
- **snapshots**: one PNG of the session per app, with that app in front.
- **native**: native Notes (`so.tilo.notes`) opens beside them, presents a
  frame and publishes its actions. Both apps are called Notes; they are told
  apart by id.
- **activation**: "Show this window" answers for tilolang Notes exactly as it
  does for native Notes.
- **read**: the door's `read` of tilolang Notes (what `tilo_read` calls) lists
  the room's records with their ids and kinds; a read of the notebook lists the
  note "New note" made, by the id a read takes.
- **idle**: each app left untouched for 10 s, native Notes among them. An app
  that commits no frame is settled, and every thread of it sleeps: the check
  fails on a single context switch. An app that commits frames is animating:
  it may commit one frame per pose of its animation (30 a second) and no
  more. Its context switches per frame are recorded.
- **routes**: a second start of the same session installs the collaboration
  probe in place of Today and Todo. Today's surface declares that it reads
  Notes' kinds; Todo's declares no read. The broker lets Today join Notes'
  route read-only and refuses its write. Todo cannot join at all.

- **host-join**: a local site, as `dist/dev/nested-pair.sh` runs one (the
  room service in persistent mode and its workspace host, `tilo-relay`, with
  the site's Lounge as its home workspace), whose operator lets the tilolang
  apps write in the Lounge (`TILO_ROOMS_AUTHOR_APPS`), and a second session of
  its own whose community it is (`--community`). The session joins the
  Lounge through the site's door and the Lounge reaches `synced`. The time
  from dialling the host to the room's first sync (`connect_to_sync_ms`, from
  the shell's `tilo::room_sync` log line) is recorded.
- **host-again**: the same session started again, holding the Lounge already.
  It syncs again; the time is recorded.
- **spinner**: started a third time with the site's processes stopped (not
  gone: a dial waits for an answer until it gives up). The Lounge is
  `connecting`, and Today, empty and in front, says so with a spinner that
  commits at least one frame and no more than one per pose, until the dial
  gives up and the Lounge is `unreachable`.
- **unsynced**: each app opens in the Lounge whose host cannot be reached.
  Native Notes says so in place of its own empty state and keeps its "New
  note", so the person works on what this device has, and Today commits no
  frame and wakes no thread for 5 s (`unsynced-*.png`).
- **reconnect**: the site's processes go on, and the Lounge syncs by itself.
- **host**: in the synced Lounge every app is told the room is synced, the
  Lounge lets each write, native Notes' "New note" lands in its history, and
  native Chat, opened now, presents its first frame (`synced-*.png`). The time
  from each launch to its first presented frame is recorded.
- **host-edit**: tilolang Notes' "New note" lands in the Lounge as native
  Notes' does; so does it in a second window of the app (the first
  duplicated, as Super+T does), and in the app started again after every
  window of it closed.

Each app opens once in the Lounge: these three starts open none before the
third, so every app is first opened there.

`--host-only` runs only the checks of a workspace with a host (host-join to
host-edit).

## Output

Everything goes to `target/tilolang-e2e` (`--out DIR` changes it):

- `summary.json` holds every check's result and the list of failed checks. The
  exit status is non-zero when a check fails.
- `notes.png`, `chat.png`, `todo.png` and `today.png` are the snapshots, and
  `native-notes.png` native Notes. Look at them: each should show only native
  window chrome around the app. `first-notes.png` and the others are the apps
  as first opened in the session's own workspace, `synced-*.png` in the
  local site's Lounge, and `unsynced-*.png` in the Lounge while its host
  cannot be reached, each saying so.
- `session-apps.log` and `session-routes.log` are the session script's own
  output.
- `shell-apps.log` is the shell's log from the first start, with each app's
  output, and `shell-routes.log` from the routes start. `shell-host-join.log`,
  `shell-host-again.log` and `shell-host.log` are the local-host session's
  three starts; `site/site.log` is the local site's.

To drive a kept session by hand, use the `drive it:` line in the session log.
It sets `XDG_RUNTIME_DIR` and `TILO_TEST_CONTROL`.
