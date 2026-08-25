# The boot bumper — how to cut it

The opening of the demo video is the machine booting. `os/dev/record-boot.py`
records the whole thing; almost none of it belongs on screen.

**This is a cut to apply to a *new* recording, not a description of a file that
exists.** A new OS build shifts every timestamp, so the rules below are
anchored to what is on screen, with the timings from the 2026-08-22 recording
given only as an example of the shape.

## Where it lives

`films/qos-boot-bumper.mp4` — the cut, **3.2s** (97 frames at 30 fps; the
worked arithmetic is at the bottom of this file).
`films/qos-boot-bumper-source.mp4` is the full recording it was cut from, kept
so the cut can be revised without re-recording a boot.

3.2 and not the 7.5 this line said for a while, and not the 15 the script once
budgeted: 7.5 was the length before the cap was worked out against a 96.8s
demo, and every other figure in the rig now agrees on 3.2.

Not under `target/`. It was there once and a routine reclaim deleted it along
with everything else in that directory, which is exactly what `target/` is for.
A recorded boot is the one asset here that cannot be regenerated on demand: it
needs a real machine starting, and every OS build shifts the timings the cut is
anchored to.

The rig does not read this file. The bumper is concatenated in front of the
driven take during assembly, so nothing fails at run time if it is missing —
which is how it went missing unnoticed.

## What the audience should see

Four things, in order, and nothing else:

1. **A few seconds of console** — the *last* few, immediately before the
   greeter. Enough to read as "a real machine is starting", not enough to read
   as "watch a log scroll". Target **3 seconds**.
2. **The greeter**, long enough to actually read: the qOS card, the focused
   username field, the session set to qOS. Target **3 seconds**, slowed if the
   recording is faster than that. The recorder does not pre-fill anything: the
   greeter idles with an *empty* form and types both credentials in one burst
   at the very end.
3. **The filled login form — and end there.** Credentials in, the primary
   action visible, before anything is submitted.

Roughly **5 seconds** total, and **3.2 seconds is the ceiling** if the
whole thing has to come in under 1:40. It is a bumper, not a chapter.

The arithmetic, so the next person does not have to re-derive it: the
demo cut runs **96.8 seconds** (`scripts/yc-demo.py --record`, measured
rather than estimated), and the brief caps the finished piece at a
hundred. That leaves 3.2 seconds, which is enough for the last beat of
console and the filled form and nothing else — cut the console to about
a second and hold the form for two.

If the demo gets shorter, this gets its five seconds back. Take them in
the login form rather than the console: a form somebody can read is
worth more than another second of a log nobody reads.

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
- ~~The seatd error.~~ **Gone as of the 2026-08-23 image** (`fbe59310b`).
  Neither `libseat` line appears on screen or in the serial log any more, so
  there is nothing to cut past. Left here only so nobody reintroduces the
  instruction from an older copy of this file.
- **The black gap between console and greeter.** In the example recording this
  was about 3 seconds of pure black with only a mouse pointer on it — a dead
  hole in the middle of the bumper. Cut it out entirely and let the console
  hand straight over to the greeter.
- **Any tail after the desktop has settled.** The example take had 62 seconds
  of motionless desktop; that is what `qos-boot-full-95s.mp4` is for.
- ~~The duplicated status line.~~ **Gone with the rebuilt greeter.** The
  submitted state on `fbe59310b` is a single greyed `Signing you in…` on the
  button itself, with no stacked second line and no internal session name.
  Ending before it is now a pacing choice, not damage control.

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

## The 2026-08-23 recording, as a worked example

`qos-boot-greeter-fbe59310b.mp4` — 1280x800, 30 fps, 844 frames, 28.13 s, the
image carrying the rebuilt greeter. Spans below are measured frame by frame
(`signalstats` for the boundaries, extracted stills to identify content), and
they correct an earlier pass of this file that had the ending a frame range too
late.

| frames | span | content |
|---|---|---|
| 0-252 | → 8.4 s | TianoCore splash — cut |
| 253-436 | 8.4-14.53 s | console, **actually scrolling** |
| 437-440 | 14.57-14.67 s | black flash on modeset |
| 441-528 | 14.70-17.6 s | console **frozen** — one unchanging frame |
| 529-543 | 17.6-18.1 s | black |
| 544-560 | 18.1-18.7 s | two failure lines alone on black |
| 561-639 | 18.7-21.3 s | black gap — cut entirely |
| 640-744 | 21.3-24.83 s | greeter idle, **username empty**, field focused |
| 745-764 | 24.83-25.47 s | credentials typed, both fields, 0.63 s |
| **764** | **25.467 s** | filled form, still editable — **the ending frame** |
| 765-770 | 25.50-25.67 s | `Signing you in…` — already submitted |
| 771→ | 25.70 s → | black. No desktop is reached at all. |

Two traps here that an earlier reading of this recording walked into:

- **The console freezes at frame 436, not at 17 s.** Everything from 14.70 s on
  is a single held frame, so a console beat taken from 14.0-17.0 is mostly a
  still with a black flash in it. Take the last live second — frames 407-436 —
  and nothing after.
- **25.5 s is already the authenticating state.** The last frame with the form
  filled *and* the `Log in` button still live is **764**. One frame later the
  button is greyed. "Filled form" and "submitted" are 33 ms apart.

**The form cannot be held at real speed.** The credential burst is 0.63 s
end to end, and the filled state exists for two frames before submit. Holding
it for the ~2 s the budget section asks for means slowing that span — this
recording does need slowing, contrary to what this section previously claimed.
`setpts=2.0*PTS` over frames 745-764 plus a short clone-hold on 764 gets there
without the drag that 3× produced on the earlier take.

### The 3.2 s cut, as built

97 frames, 3.2333 s — chosen so that bumper + a 96.767 s demo lands on exactly
100.000 s:

| output frames | source | treatment | length |
|---|---|---|---|
| 0-26 | 410-436 | real speed | 0.900 s console |
| 27-47 | 724-744 | real speed | 0.700 s greeter idle |
| 48-87 | 745-764 | `setpts=2.0*PTS` | 1.333 s credentials going in |
| 88-96 | 764 held | `tpad` clone | 0.300 s filled form |

1280x800 into 1920x1080 is pillarboxed, not cropped — scaled to 1728x1080 and
padded 96 px each side. Cropping to 16:9 would fill the frame but throws away
real screen; and against the black console the bars are invisible anyway, so
the only shot that shows them is the greeter, where a centred card carries it.

At this length the console reads and the form reads, but the greeter gets 0.7 s
of idle before typing starts, which is the beat that suffers. If the budget ever
grows, spend it there first — 1.5 s of idle greeter before the credentials go in
is the difference between "a login screen appeared" and "I saw the login
screen".

Two things this boot showed that are not visible in the film, because the
on-screen console freezes before systemd's summary: `shell-firstboot.service`
and `systemd-growfs-root.service` both fail. Good for the cut, bad for the OS.

## Finding the spans in a new recording

Do not guess. Pull frames and look:

```bash
ffmpeg -i qos-boot.mp4 -vf "fps=4,scale=760:-1" frames/f_%03d.png
```

The greeter is easy to find mechanically — it is the first frame with
meaningful brightness after the black gap. In the example recording, measuring
the fraction of pixels above a low threshold found it at once: the console runs
bright, the gap sits near zero, and the greeter card comes back at about 1%.
