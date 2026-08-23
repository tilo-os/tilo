# The boot bumper — how to cut it

The opening of the demo video is the machine booting. `os/dev/record-boot.py`
records the whole thing; almost none of it belongs on screen.

**This is a cut to apply to a *new* recording, not a description of a file that
exists.** A new OS build shifts every timestamp, so the rules below are
anchored to what is on screen, with the timings from the 2026-08-22 recording
given only as an example of the shape.

## What the audience should see

Four things, in order, and nothing else:

1. **A few seconds of console** — the *last* few, immediately before the
   greeter. Enough to read as "a real machine is starting", not enough to read
   as "watch a log scroll". Target **3 seconds**.
2. **The greeter**, long enough to actually read: the qOS card, `quickuser`
   already in the username field, the password field focused. Target
   **3 seconds**, slowed if the recording is faster than that.
3. **The filled login form — and end there.** Credentials in, the primary
   action visible, before anything is submitted.

Roughly **5 seconds** total. It is a bumper, not a chapter.

**Do not show the desktop at all**, and do not run into the authenticating
state. Two separate reasons, both found by watching the assembled cut rather
than the pieces:

- The demo opens on an empty desktop of its own. A bumper that ends on one
  shows the same picture on both sides of a cut, which reads as a stall — and
  worse, the two desktops come from *different builds*, so the design visibly
  changes across the join. Ending before any desktop means the demo's is the
  first one, and the cut reads as the login having worked.
- The authenticating state is where the old greeter's defects live: the
  doubled status line and the internal session name. Ending on it makes the
  worst frame in the recording the last thing anybody sees.

Once a recording exists from an image carrying the rebuilt greeter, the second
reason goes away and the landing can come back if it earns its seconds. The
first does not: the demo's desktop should still be the first desktop.

## What to remove

- **Everything before the last few seconds of console.** Firmware, the
  TianoCore splash, GRUB, the kernel banner, and the long systemd run. In the
  example recording that was the first ~26 s of a 38 s take.
- **The TianoCore splash specifically**, even if you keep more console than
  the rule above. It is QEMU's firmware logo, and to anyone who recognises it
  the shot says "this is a virtual machine" rather than "this is an operating
  system".
- **The seatd error.** Two lines that read as a failure:

  ```
  [libseat] Could not connect to socket /run/seatd.sock: No such file or directory
  [libseat] Backend 'seatd' failed to open seat, skipping
  ```

  It is harmless — the compositor falls through to another backend and boots —
  but it is text saying `failed` in a film for investors. Cut past it. Better
  still, fix why seatd is not up in the image, and then there is nothing to cut.
- **The black gap between console and greeter.** In the example recording this
  was about 3 seconds of pure black with only a mouse pointer on it — a dead
  hole in the middle of the bumper. Cut it out entirely and let the console
  hand straight over to the greeter.
- **Any tail after the desktop has settled.** The example take had 62 seconds
  of motionless desktop; that is what `qos-boot-full-95s.mp4` is for.
- **The duplicated status line.** The 2026-08-23 greeter renders
  `Authenticating…` twice, stacked, with a spinner between them. It is a
  greeter defect rather than a cut problem — fix it in the image if there is
  time, and until then prefer a cut that does not dwell on it.

## What to slow, and how much

The greeter and the typed login go past in about **one second** — the recorder
types over its monitor connection, far faster than a person. Stretch that span
so it reads, but do not overshoot: 3× turned 2.8 s into 8.4 s, which is
noticeably draggy on the back half.

**Slow only the span that is actually too fast** — the form appearing, the
credential going in, the form leaving. Do not slow the desktop landing with it;
that should arrive at real speed.

Slowing duplicates frames rather than inventing them:

```bash
ffmpeg -i in.mp4 -ss <start> -to <end> -c:v libx264 -crf 18 -an seg.mp4
ffmpeg -i seg.mp4 -vf "setpts=<factor>*PTS" -r 30 -c:v libx264 -crf 18 -an slow.mp4
```

Two passes, because `-to` on the same command clips the stretched output back
to the original length and silently gives you an unslowed clip.

**Never speed anything up.** The boot is already software-rendered in a VM
(llvmpipe, 4 GB, 4 vCPU) and therefore slower than real hardware — the honest
edit is a shorter cut, never a faster one.

## Assembling

Cut each span, then join with `scripts/demo-assemble.py`, which normalises
everything to 1920×1080 / 30 fps and refuses if a segment is missing. The boot
recording is 1280×800, so it pillarboxes — expected.

```bash
scripts/demo-assemble.py --out final.mp4 boot-cut.mp4 qos-demo.mp4
```

## Finding the spans in a new recording

Do not guess. Pull frames and look:

```bash
ffmpeg -i qos-boot.mp4 -vf "fps=4,scale=760:-1" frames/f_%03d.png
```

The greeter is easy to find mechanically — it is the first frame with
meaningful brightness after the black gap. In the example recording, measuring
the fraction of pixels above a low threshold found it at once: the console runs
bright, the gap sits near zero, and the greeter card comes back at about 1%.
