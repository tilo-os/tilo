# Demo video — what is left

Status read from live runs and from watched frames, not from assertions.
Updated 2026-08-22, late.

## What changed today

Someone finally watched the recording frame by frame instead of reading the
harness output. Six of nine beats did not show what they claimed. Three of
those were storyboard problems rather than bugs — a beat cannot be filmed if
it has no on-screen surface, however well it works underneath.

`scripts/demo-verify.py` is the tool that caught it: it locates a beat inside
a finished video and pulls frames from that span. No cut ships again without
it.

## Blocking the video

**1. Beat 4 — peer cursors on the desktop.** *In progress.*

Filmed, this beat is twenty-eight seconds of empty desktop.
`RemoteCursorOverlay::new()` is called only in
`shell/tests/remote_cursors_test.rs`; the compositor never mounts it. Presence
does cross the network and does render *inside an app window* — the desktop
layer simply has nothing drawing it.

**2. Beat 6 — a replicated window must be drawn.** *In progress.*

The assertion passes and the screen is blank. The room document carries the
window, the peer's own tree holds it, and the peer's compositor renders
nothing. This one is the most dangerous of the set, because green reads as
proof.

**3. Beat 3 — inviting an agent has no on-screen surface.** *Undecided.*

The grant is real and goes through the capability model, but it happens over
the control socket, so nothing appears. On camera the only visible artifact is
beat 2's leftover invite card, which a viewer will read as the agent invite.
Either give the grant a visible affordance, or fold it into beat 2 and stop
claiming it as a separate beat.

**4. Peers need names.** *In progress.*

A caret labelled `peer@5537458103208628978` does not say who is there. Names
come from the inviter and live in the room document beside the capability
grant — never self-declared, which would be spoofable.

**5. The word processor films in German.** *Handed to the recorder.*

`apps/word-processor/src/i18n.rs:88` reads `LC_ALL`/`LANGUAGE`/`LANG`, the
host is `de_DE`, and bwrap does not clear the environment. The shell chrome
renders English above a German toolbar. A real qOS install sets its own
locale; this is a nested-development artifact. Fix is `LC_ALL=en_US.UTF-8` in
the session environment.

**6. Re-shoot, then watch it.** Beats 3, 4, 6, 8 and 9 all need re-shooting
once the above land. Watching is not optional and is not the same as reading
assertions.

## Blocking the opening shot

**7. OS boot footage.** *Blocked on one manual step.*

`podman login code.sandbox.example` — the image cannot be pulled without it. The
recorder (`os/dev/record-boot.py`) is proven against real firmware: 180
frames, 12.0s. A CI image build is in flight; the missing `COPY` for
`qos-identity.service` that had broken it for six days is fixed.

## Settled

- **Beat 7 converges.** Two real packaged word processors. The op-stream
  reader was discarding every unsigned frame, including the snapshot two
  independent documents need to find each other.
- **Beat 9 was blocked on beat 7** — the agent's edits were real and attributed
  but only on its own screen. Convergence should carry them; re-shoot and
  confirm rather than assuming.
- **Beat 5 is measured**: median 6.7s keystroke to first frame, spread
  5.0–9.5s, on a 446 MB debug package over a loopback registry. Say "a few
  seconds", never "instant" — a viewer can time it off the recording.
- **Beat 8 films the file chooser, not a drag.** The launched Files is a
  sandboxed Wayland client and neither it nor the toolkit opens a
  `wl_data_source`, so there is no client drag to promote. The chooser is the
  better beat regardless: the app asks, the OS chooses, the app is handed
  bytes rather than a path it has no right to read.
- **The fake "Ada" peer is deleted**, not gated. Any footage from before
  2026-08-22 contains a collaborator who does not exist. Three compositor logs
  and ~40 frames from the current cut are clean.
- **First boot is honest** — one empty workspace, no invented people,
  workspaces, tabs or documents.

## Things that must not be said

- that a beat works because its assertion passed
- "install is instant" — it is a few seconds, and measurable on camera
- that the peer cursor in old footage is a person
