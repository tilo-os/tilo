#!/usr/bin/env python3
"""Cut the boot bumper out of a raw boot recording.

`docs/boot-bumper-cut.md` describes the cut in prose and has done since the
first one was made by hand. Prose was not enough: the bumper on disk when this
script was written was 7.5 s of a documented 3.2 s cut, and it contained two
stretches of pure black, a frozen console at one geometry and a live one at
another, and a greeter that never moved for four seconds. Every one of those
is a rule the document already forbids. A cut nobody can re-run is a cut that
drifts from its own description, so this runs it.

    ./scripts/cut-boot-bumper.py films/qos-boot-greeter-XXXXXXX.mp4 \
        --out films/qos-boot-bumper.mp4

The spans are **found in the recording**, not hard-coded: a new OS build moves
every timestamp, which is the reason the document refuses to state any. What is
fixed is the shape — console, greeter at rest, credentials going in, the filled
form — and the frame budget.

Refuses rather than improvises. If a landmark is not where the shape says it
should be, that is a recording this cut does not fit, and a bumper built from
guesses would look right in a contact sheet and wrong in the film.

## Geometry

The guest boots at 1280x800 and the film is 1920x1080. Padding the one into the
other pillarboxes it, and the demo it is joined to is full-bleed, so the join
became a resize a viewer sees as a stitch. This crops to 16:9 first and then
scales, so the bumper is the film's own geometry and the join is a plain cut.

The crop is centred on the *greeter's own* content rather than on the
framebuffer: the greeter paints about 1280x765 and leaves the rest black, so
centring on the framebuffer would sit the card high and keep a black strip.
Cropping loses screen at the top and bottom of the console, which is a real
cost and the reason the document argued against it — but a second of console is
texture, not text, and a picture that changes size mid-shot costs more.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageChops, ImageStat

#: What the film is.
OUT_W, OUT_H, FPS = 1920, 1080, 30

#: The cut's shape, in output frames. 97 frames is 3.2333 s — see the budget
#: arithmetic in docs/boot-bumper-cut.md.
CONSOLE_FRAMES = 27      # 0.900 s, real speed
IDLE_FRAMES = 21         # 0.700 s, real speed
CREDENTIAL_SLOWDOWN = 2  # setpts factor over the credential burst
HOLD_FRAMES = 9          # 0.300 s of the filled form, cloned from one frame

#: Analysis is done on a downscale: it is looking for black gaps, a static
#: picture and scroll bursts, none of which need pixels.
PROBE_W, PROBE_H = 160, 100

#: A frame this dark is black. The black gaps in a boot recording measure 0.0
#: and the dimmest real picture measures above 9.
BLACK_MEAN = 1.0
#: Frame-to-frame difference that counts as the console scrolling. Between
#: scrolls a console frame is held exactly, so this only has to clear noise.
SCROLL_DIFF = 1.0
#: The greeter idles bit-identical; typing a character moves this much.
TYPING_DIFF = 0.01


def die(msg: str) -> "NoReturn":  # type: ignore[valid-type]
    print(f"cut-boot-bumper: {msg}", file=sys.stderr)
    raise SystemExit(1)


def probe(path: pathlib.Path) -> dict:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height,r_frame_rate,nb_frames",
         "-of", "json", str(path)],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        die(f"ffprobe could not read {path}:\n{out.stderr.strip()}")
    s = json.loads(out.stdout)["streams"][0]
    num, _, den = s["r_frame_rate"].partition("/")
    return {"width": s["width"], "height": s["height"],
            "fps": float(num) / float(den)}


def gray_frames(path: pathlib.Path):
    """Every frame of `path`, downscaled to greyscale, as PIL images."""
    proc = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-i", str(path),
         "-vf", f"scale={PROBE_W}:{PROBE_H}", "-pix_fmt", "gray",
         "-f", "rawvideo", "-"],
        stdout=subprocess.PIPE,
    )
    n = PROBE_W * PROBE_H
    assert proc.stdout is not None
    while True:
        buf = proc.stdout.read(n)
        if len(buf) < n:
            break
        yield Image.frombytes("L", (PROBE_W, PROBE_H), buf)
    proc.stdout.close()
    proc.wait()


def analyse(path: pathlib.Path) -> tuple[list[float], list[float]]:
    """Per-frame mean brightness and difference from the frame before."""
    means: list[float] = []
    diffs: list[float] = []
    prev = None
    for im in gray_frames(path):
        means.append(ImageStat.Stat(im).mean[0])
        diffs.append(0.0 if prev is None
                     else ImageStat.Stat(ImageChops.difference(im, prev)).mean[0])
        prev = im
    if not means:
        die(f"{path} decoded to no frames")
    return means, diffs


def longest_black_run(means: list[float]) -> tuple[int, int]:
    """The longest run of black frames, as [start, end) frame indices."""
    best = (0, 0)
    i = 0
    while i < len(means):
        if means[i] < BLACK_MEAN:
            j = i
            while j < len(means) and means[j] < BLACK_MEAN:
                j += 1
            if j - i > best[1] - best[0]:
                best = (i, j)
            i = j
        else:
            i += 1
    return best


def find_landmarks(means: list[float], diffs: list[float]) -> dict:
    """Where the four beats of the shape actually are in this recording.

    The document's own instruction for a new recording is "do not guess, pull
    frames and look". This looks, at the three things that are unambiguous in
    a boot: black is black, a console scrolls, and a greeter at rest does not
    change at all.
    """
    n = len(means)

    # The greeter arrives after the longest black stretch in the recording —
    # the gap between the console handing over and the compositor painting.
    gap_start, gap_end = longest_black_run(means)
    if gap_end >= n:
        die("the recording ends in black with no greeter after it")
    greeter_start = gap_end

    # It runs until the picture goes black again (the recording's own tail).
    greeter_end = greeter_start
    while greeter_end < n and means[greeter_end] >= BLACK_MEAN:
        greeter_end += 1

    # Submit is the one large change inside the greeter: the button greys and
    # the whole card dims. Everything before it is idle-or-typing.
    submit = None
    for i in range(greeter_start + 1, greeter_end):
        if diffs[i] > 0.5:
            submit = i
            break
    if submit is None:
        die("no submit found inside the greeter — the form is never filled in")
    # The last frame with the form filled and `Log in` still live.
    ending = submit - 1

    # Typing starts at the first frame that differs from the greeter at rest.
    burst_start = None
    for i in range(greeter_start + 1, submit):
        if diffs[i] > TYPING_DIFF:
            burst_start = i
            break
    if burst_start is None:
        die("the greeter never changes before submit — no credentials are typed")

    # The live console is everything before the first black frame that precedes
    # the gap. After that first flash the console is a frozen still, which is
    # the trap the document records: a beat taken from it is a photograph.
    first_black = None
    for i in range(greeter_start):
        if means[i] < BLACK_MEAN and any(m >= BLACK_MEAN for m in means[:i]):
            first_black = i
            break
    if first_black is None:
        die("no black flash before the greeter — this is not a boot recording")
    console_end = first_black          # exclusive

    # …and it starts at the first frame that scrolls. Everything before that is
    # the firmware splash, which is static and which the document rules out on
    # its own account: it is QEMU's logo, and it says "virtual machine" to
    # anyone who recognises it. Anchoring to motion keeps it out of the
    # candidate window without having to recognise a logo.
    console_start = None
    for i in range(console_end):
        if diffs[i] > SCROLL_DIFF and means[i] >= BLACK_MEAN:
            console_start = i
            break
    if console_start is None:
        die("nothing in this recording scrolls before the greeter — there is no "
            "console beat to cut")

    return {
        "console": (console_start, console_end),
        "greeter_start": greeter_start,
        "burst_start": burst_start,
        "ending": ending,
        "submit": submit,
    }


def console_window(diffs: list[float], span: tuple[int, int]) -> int:
    """Where to start the console beat.

    The beat has to read as a machine starting rather than as a log, and the
    only part of that this can measure is motion: a window where the console
    scrolls several times reads as a machine doing something, and a window
    where it does not is a photograph of a wall of text. Ties go to the later
    window, since the last of the boot is the part that leads into the greeter.

    Which second *says* "starting" is a judgement about words on a screen, and
    this cannot read them — hence --console-start, and hence the contact sheet.
    """
    lo, hi = span
    events = [i for i in range(lo, hi) if diffs[i] > SCROLL_DIFF]
    best = (-1, lo)
    for s in range(lo, max(lo + 1, hi - CONSOLE_FRAMES + 1)):
        c = sum(1 for e in events if s <= e < s + CONSOLE_FRAMES)
        if c >= best[0]:
            best = (c, s)
    return best[1]


def greeter_crop(path: pathlib.Path, greeter_start: int, src: dict) -> tuple[int, int]:
    """Crop height and top offset that put the greeter's own picture in 16:9.

    Measured off a greeter frame rather than assumed, because the greeter does
    not fill the framebuffer: it paints down to about row 765 of 800 and leaves
    a black strip under it. Centring the crop on the framebuffer keeps part of
    that strip and sits the card high.
    """
    w, h = src["width"], src["height"]
    crop_h = min(h, round(w * OUT_H / OUT_W))
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path),
         "-vf", f"select='eq(n\\,{greeter_start + 4})'", "-vsync", "0",
         "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"],
        capture_output=True,
    )
    if out.returncode != 0 or not out.stdout:
        die("could not pull a greeter frame to measure the crop against")
    import io
    im = Image.open(io.BytesIO(out.stdout)).convert("L")
    px = im.load()
    last = 0
    for y in range(im.size[1] - 1, -1, -1):
        if max(px[x, y] for x in range(0, im.size[0], 4)) > 6:
            last = y
            break
    top = max(0, min(h - crop_h, round((last + 1) / 2) - crop_h // 2))
    return crop_h, top


def run(cmd: list[str]) -> None:
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        die(f"{' '.join(cmd[:6])} … failed:\n{r.stderr.strip()}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("recording", help="the raw boot recording from os/dev/record-boot.py")
    ap.add_argument("--out", required=True, help="where to write the bumper")
    ap.add_argument("--console-start", type=int, default=None,
                    help="first frame of the console beat, overriding the "
                         "densest-motion default (0-based, in the recording)")
    ap.add_argument("--contact-sheet", default=None,
                    help="write the first, middle and last frame of each beat "
                         "here, to be looked at before the cut is believed")
    ap.add_argument("--dry-run", action="store_true",
                    help="report the spans this would cut and write nothing")
    args = ap.parse_args()

    if not shutil.which("ffmpeg") or not shutil.which("ffprobe"):
        die("ffmpeg and ffprobe are required")
    rec = pathlib.Path(args.recording)
    if not rec.is_file():
        die(f"no such recording: {rec}")

    src = probe(rec)
    if abs(src["fps"] - FPS) > 0.01:
        die(f"{rec} is {src['fps']:.2f} fps; the cut's frame budget assumes {FPS}")

    means, diffs = analyse(rec)
    lm = find_landmarks(means, diffs)

    c_lo, c_hi = lm["console"]
    c_start = args.console_start if args.console_start is not None \
        else console_window(diffs, (c_lo, c_hi))
    if not (c_lo <= c_start <= c_hi - CONSOLE_FRAMES):
        die(f"console beat {c_start}..{c_start + CONSOLE_FRAMES - 1} falls outside "
            f"the live console ({c_lo}..{c_hi - 1})")

    idle_start = lm["burst_start"] - IDLE_FRAMES
    if idle_start < lm["greeter_start"]:
        die(f"the greeter is only {lm['burst_start'] - lm['greeter_start']} frames "
            f"long before typing starts; the cut needs {IDLE_FRAMES}")

    burst_len = lm["ending"] - lm["burst_start"] + 1
    crop_h, crop_top = greeter_crop(rec, lm["greeter_start"], src)

    print(f"recording      {rec.name}  {src['width']}x{src['height']} @ {src['fps']:.0f}fps, "
          f"{len(means)} frames")
    print(f"live console   {c_lo}..{c_hi - 1}")
    print(f"console beat   {c_start}..{c_start + CONSOLE_FRAMES - 1}"
          f"  ({CONSOLE_FRAMES} frames, {CONSOLE_FRAMES / FPS:.3f}s)"
          + ("  [given]" if args.console_start is not None else "  [densest motion]"))
    print(f"greeter idle   {idle_start}..{lm['burst_start'] - 1}"
          f"  ({IDLE_FRAMES} frames, {IDLE_FRAMES / FPS:.3f}s)")
    print(f"credentials    {lm['burst_start']}..{lm['ending']}"
          f"  ({burst_len} source frames → {burst_len * CREDENTIAL_SLOWDOWN} at "
          f"{CREDENTIAL_SLOWDOWN}×, {burst_len * CREDENTIAL_SLOWDOWN / FPS:.3f}s)")
    print(f"filled form    {lm['ending']} held  ({HOLD_FRAMES} frames, "
          f"{HOLD_FRAMES / FPS:.3f}s)   [submit is {lm['submit']}]")
    total = CONSOLE_FRAMES + IDLE_FRAMES + burst_len * CREDENTIAL_SLOWDOWN + HOLD_FRAMES
    print(f"crop           {src['width']}x{crop_h}+0+{crop_top} → {OUT_W}x{OUT_H}, full-bleed")
    print(f"total          {total} frames, {total / FPS:.4f}s")

    if args.contact_sheet:
        sheet = pathlib.Path(args.contact_sheet)
        sheet.mkdir(parents=True, exist_ok=True)
        picks = {
            "1-console-first": c_start,
            "1-console-last": c_start + CONSOLE_FRAMES - 1,
            "2-idle-first": idle_start,
            "3-credentials-first": lm["burst_start"],
            "4-filled-form": lm["ending"],
            "x-submitted-excluded": lm["submit"],
        }
        for name, n in picks.items():
            run(["ffmpeg", "-v", "error", "-i", str(rec),
                 "-vf", f"select='eq(n\\,{n})',crop={src['width']}:{crop_h}:0:{crop_top},"
                        f"scale={OUT_W}:{OUT_H}",
                 "-vsync", "0", "-frames:v", "1",
                 str(sheet / f"{name}-{n:04d}.png"), "-y"])
        print(f"contact sheet  {sheet}")

    if args.dry_run:
        return 0

    geom = (f"crop={src['width']}:{crop_h}:0:{crop_top},"
            f"scale={OUT_W}:{OUT_H}:flags=lanczos,setsar=1,format=yuv420p")
    enc = ["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-an"]

    tmp = pathlib.Path(tempfile.mkdtemp(prefix="qos-bumper-"))
    try:
        parts = []

        # Two passes per segment, as docs/boot-bumper-cut.md prescribes and for
        # a related reason: `select` in the same chain as `fps` stops limiting
        # anything — the span silently runs to the end of the recording, and
        # what comes out is a clip that plays and is wrong. Cut first, retime
        # second.
        def seg(name: str, pick: str, retime: str) -> None:
            raw = tmp / f"{name}-raw.mp4"
            run(["ffmpeg", "-v", "error", "-i", str(rec),
                 "-vf", f"{pick},{geom}", "-fps_mode", "passthrough",
                 "-c:v", "libx264", "-preset", "veryfast", "-crf", "12", "-an",
                 str(raw), "-y"])
            out = tmp / f"{name}.mp4"
            run(["ffmpeg", "-v", "error", "-i", str(raw),
                 "-vf", retime, *enc, str(out), "-y"])
            parts.append(out)

        real = f"fps={FPS}"
        seg("0-console",
            f"select=between(n\\,{c_start}\\,{c_start + CONSOLE_FRAMES - 1})", real)
        seg("1-idle",
            f"select=between(n\\,{idle_start}\\,{lm['burst_start'] - 1})", real)
        # Slowing stretches the clock and lets `fps` duplicate frames to fill
        # it. It never invents one, and it is never applied in reverse: the
        # boot is software-rendered in a VM and therefore already slower than
        # real hardware, so the honest edit is a shorter cut, never a faster
        # one.
        seg("2-credentials",
            f"select=between(n\\,{lm['burst_start']}\\,{lm['ending']})",
            f"setpts={CREDENTIAL_SLOWDOWN}*PTS,fps={FPS}")
        # One frame, repeated. `loop` rather than `tpad` because it counts in
        # frames, and the budget this cut is checked against is in frames.
        seg("3-hold",
            f"select=eq(n\\,{lm['ending']}),"
            f"loop=loop={HOLD_FRAMES - 1}:size=1:start=0,setpts=N/FRAME_RATE/TB",
            real)

        listing = tmp / "parts.txt"
        listing.write_text("".join(f"file '{p.name}'\n" for p in parts), encoding="utf-8")
        out = pathlib.Path(args.out).resolve()
        out.parent.mkdir(parents=True, exist_ok=True)
        run(["ffmpeg", "-v", "error", "-f", "concat", "-safe", "0",
             "-i", str(listing), "-c", "copy", str(out), "-y"])

        got = probe(out)
        cnt = subprocess.run(
            ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
             "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", str(out)],
            capture_output=True, text=True).stdout.strip()
        print(f"\nwrote {out}  {got['width']}x{got['height']}  {cnt} frames")
        if cnt.isdigit() and int(cnt) != total:
            die(f"expected {total} frames, got {cnt} — the cut is not what it says "
                f"it is, and a bumper that is the wrong length is how the last one "
                f"drifted to 7.5s unnoticed")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
