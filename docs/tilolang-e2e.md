# tilolang apps end to end

One command builds the tilolang example apps, installs them into a headless
nested Tilo session and checks them as real, independent Tilo apps:

```sh
scripts/headless-examples/tilolang-e2e.py            # builds what it needs
scripts/headless-examples/tilolang-e2e.py --no-build # reuses target/nested-session
```

Without `--no-build` it builds the nested session through
`dist/dev/nested-session.sh`
and the `tilolang` tool, `tilo-collab-probe` and tilolang-host's `components`
example (which names the test ids of an app's components) with cargo.
`--no-build` needs `target/nested-session`, `target/debug/tilolang`,
`target/debug/tilo-collab-probe` and `target/debug/examples/components` to
exist already.

The session is headless and runs with `--software`: the shell, its render
service and every app render on lavapipe, and the shell probes no GPU. It
never opens a window on the desktop it runs from and never captures that
desktop. The session's state is thrown away: it lives in
`target/tilolang-e2e/state`, and the script stops the session, its compositor
and its runtime directory at the end. `--keep` leaves it running.

## What it checks

`tilolang build apps/tl/today --search apps/tl` builds Notes, Chat, Tasks and
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
- **hosts**: each app's window is its window alone (`TILO_TILOLANG_HOSTED`): its
  instance runs in a tilolang host, and the apps of one publisher in one
  workspace share one host process, which was handed every window's instance.
- **actions**: each app publishes its actions; a tilolang app's are its
  host's instance's, with the history verbs, `read` and "Show this window"
  every app publishes. Invoking "New note", "New channel"
  and "New task" each adds an entry to that app's history.
- **hosted-here**: one PNG of the session per app as first opened, with that app
  in front (`first-*.png`). The session's workspace is one this device holds
  (`held_workspaces` in `state-dump` names it), so its apps wait on no other
  host, and each shows its own state at once, as native Notes does.
- **snapshots**: one PNG of the session per app, with that app in front.
- **native**: native Notes (`so.tilo.notes`) opens beside them, presents a
  frame and publishes its actions. Both apps are called Notes; they are told
  apart by id.
- **activation**: "Show this window" answers for tilolang Notes exactly as it
  does for native Notes.
- **read**: the door's `read` of tilolang Notes (what `tilo_read` calls) lists
  the workspace's records with their ids and kinds; a read of the notebook lists the
  note "New note" made, by the id a read takes.
- **idle**: each app left untouched for 10 s, native Notes among them, and the
  tilolang host that runs the apps' instances: every thread of them sleeps, and
  the check fails on one context switch, naming the threads that woke. Nothing
  animates here (each app's workspace is one this device holds) and every
  window's frame count (`windows` reports `frames`, what its client committed)
  stands still, so one that woke woke for work nobody asked for.
- **today**: Today's card lists the tasks typed into Tasks' own window, which it
  reads from Tasks' record. One of them is marked done by its box, a checkbox of
  Tasks' window named "Mark <task> done" and pressed by name, and the change stands
  in Tasks' history (the workspace took it). Its "Morning cleanup" is a control the desk names for
  the window — a tool or a screen reader presses it by name — and pressed with one
  task done in Tasks it runs: either it files "Morning: 1 done" in Notes and clears
  the done task from Tasks, or it asks the person's leave first, which a headless
  session has nobody to give (the request is refused unasked, as every app's change
  of another app is).
- **routes**: a second start of the same session installs the collaboration
  probe in place of Today (whose surface declares that it reads Notes' kinds)
  and Tasks (which declares no read). The probe runs as each app's own launched
  process (its entrypoint names the probe's runtime, not tilolang's), with that
  app's launch custody. Tasks' join is refused as it declares no read; Today's
  is refused before its declared read is read, because this session's broker
  keeps what it admits and a tilolang host holds an app's route. The check
  reports that refusal.

Each app opens once in the session's own workspace; the checks above open it there.

## Output

Everything goes to `target/tilolang-e2e` (`--out DIR` changes it):

- `summary.json` holds every check's result and the list of failed checks. The
  exit status is non-zero when a check fails.
- `notes.png`, `chat.png`, `tasks.png` and `today.png` are the snapshots, and
  `native-notes.png` native Notes. Look at them: each should show only native
  window chrome around the app. `first-notes.png` and the others are the apps
  as first opened in the session's own workspace.
- `session-apps.log` and `session-routes.log` are the session script's own
  output.
- `shell-apps.log` is the shell's log from the first start, with each app's
  output, and `shell-routes.log` from the routes start.

To drive a kept session by hand, use the `drive it:` line in the session log.
It sets `XDG_RUNTIME_DIR` and `TILO_TEST_CONTROL`.
