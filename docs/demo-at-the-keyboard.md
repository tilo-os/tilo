# At the keyboard

The card for the person filming the live take. `docs/demo-script.md` is the
film; this is the **rig as it behaves today**, in the order you meet it. Every
line was read off a run of

```
scripts/demo.sh --no-build -- --narrate --max-hold 0 --no-instruction
```

which leaves both halves to you: your machine is never driven, and xaver's one
gesture is handed back to you part way through. Do **not** add `--self-test`
to it — that flag decides the keyboard layout for both windows, and with it on
the window you type into is pinned to a US map while your hands are on a
German one.

Two flags exist that are **wrong for this card and right for an unattended
take**, and it is worth knowing which is which:

- `--no-windows` films with no window on the desktop at all. Every frame comes
  off each session's own framebuffer rather than off the screen, so the film is
  identical — the windows exist only for somebody watching it happen. Use it
  when nobody is watching; it is incompatible with this card, which needs a
  window to type into.
- `--no-image-editor` leaves the photo editor closed so the agent has to open
  it, which is the claim the film's second half turns on. The tab-bar shot
  survives on chat as the second application. An attended take wants this too.

## Before you press return

- **Put the terminal where you can see it.** Your session opens at the full
  work area and comes to the front by itself. The rig asks you for things in
  the terminal and, after the countdown, sends no desktop notification at all.
- **A stale build is refused.** If anyone has touched `shell/src` or `ui/src`
  since `target/demo/shell` was linked, the run exits 2 with `refusing to
  record: the compositor … is older than N source file(s)`. Drop `--no-build`,
  or add `--allow-stale`.
- **`--narrate` opens a live microphone** on whatever the desktop's sound
  settings select, and fails loudly if it will not open. `--narrate` with a
  non-zero `--max-hold` is refused; `--max-hold 0` is accepted **silently**.

Setup runs about a minute before it wants anything, then prints one page and a
five-second countdown.

## The sequence

**1 · Mint two invitations, back to back.** Files, the collaborators panel,
**Invite** → `xaver` → create; **Invite** again → `Claude` → create.

- Terminal: `he mints an invite — all of this waits on it`, then `an
  invitation exists … made out to 'xaver'`, then `a second invitation exists`.
- The printed page asks for one invitation; the rig wants two. Its clock on
  the second starts when the first token appears and runs 25 s, and xaver's
  redemption spends 10–15 s of that — so mint them together, before anything
  else. Miss it and you get `only one invitation was minted, so the agent
  redeems xaver's`, and the agent's chip reads `peer@<n>` instead of a name.

**2 · Xaver crosses the room.** Nothing to do, and nothing to see: eight
seconds after the token exists his half runs itself, off camera. The film
stays on your screen.

**3 · Claude's session is admitted.** Nothing to do; off camera, a block of
`[PASS]` lines about grants.

**4 · Open the word processor and write a few words in it.** Anything — a
sentence is plenty. Nothing is addressed to the agent and nothing is typed to
it; what the beat shows is that the document is shared. The rig waits here
(`he writes a few words in the document`) and moves the moment your words turn
up in xaver's own copy, which is the frame it cuts on.

- Wrong: `xaver's screen still showed nothing after 12s`, or `his app answered
  … and none of it is` — the cut lands on a page with nothing of yours on it.
  A poor shot, not a lost take.

**5 · The handover.** The camera moves to **xaver's** screen and stays there
for the rest of the film. The rig raises his window and prints, into the
terminal now behind it:

```
  XAVER IS YOURS. His window is in front.
    right-click the image in Files, then write:
      Claude: make this square and put it in the open document
```

Be quick. The camera **never comes back** — so every second you spend finding
the window and typing is filmed as a still of the other machine. In the
measured run this beat took 87% of the finished film.

- Wrong, and it looks exactly like a hang: a workspace on screen, nothing
  happening, the counter stuck on `Claude waits to be spoken to`. That is the
  rig waiting for **you**. Nothing announces it but the terminal.

**6 · It ends itself.** `Claude acted — applied …`, the camera moves to the
document on the screen you are already watching, three seconds for the
network, a six-second hold, then the clip. One Ctrl-C ends the take early and
still writes the clip; a second abandons it.

## What a bad take looks like from the terminal

| You see | It means |
|---|---|
| `refusing to record: the compositor … is older than N source file(s)` | `--no-build` against an edited tree. Rebuild, or `--allow-stale`. |
| `the microphone did not open` and a list of sources | Pass one of the listed names to `--narrate`. |
| stuck on `he mints an invite` | Nothing reached the clipboard or the card. Press Create invite again. |
| stuck on `Claude waits to be spoken to` | Beat 5 was never performed, or what you wrote in the remark does not begin `Claude:`. |
| `the agent answered … was that it could not do what was asked` | It refused on the page. The take is finished and unusable. |
| `frames are resampled to 1920x1048 and letterboxed` | The window manager granted a size the video is not; the film is a rescale, not 1:1. |
| `WARNING: processes survived the scope` | Only trust this on an idle machine. The check is machine-wide, so anybody else's qOS run — or your own `--keep` session — trips it. |

## Known faults, measured on 2026-08-25

- **A narrated take comes out about twice its real length.** `--max-hold 0`
  turns the trim off but not the composer's one-frame-per-tick floor, and the
  camera captures at ~57 fps against `--fps 30`. Measured: 269.8 s of take,
  270 s of narration, **521.7 s** of picture — the voice ends halfway through
  the film and is progressively early before that. Compare `duration` in the
  closing report against `captured over Ns of demo` two lines above it before
  you trust a narrated take.
- The agent is refused when it tries to open Photos (`Claude has not been
  given leave to open Photos in this room`) even though the run reports the
  grants as accepted, so the crop cannot happen and beat 5 lands as a refusal.
- `qos-lv-*.sock` files accumulate in `$XDG_RUNTIME_DIR` from every run that
  did not exit cleanly. Harmless; do not mistake a stale one for a live one.
- Composing a long take is slow and silent: a 522 s film spent nine minutes in
  `ffmpeg` and then several more in the `ffprobe -count_frames` readback, with
  nothing printed. The take is already on disk by then; wait it out.
