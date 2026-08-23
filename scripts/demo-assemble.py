#!/usr/bin/env python3
"""Join the demo's separately-recorded pieces into one file.

`yc-demo.py --record` composes the nine beats from a frame timeline, so it
can only ever emit what it captured itself. Two pieces of the finished
video are not captured by it:

  * the machine booting, recorded from QEMU by `os/dev/record-boot.py`
  * a beat driven by a person rather than a script, recorded by
    `yc-demo-insert.py`

This joins them. It re-encodes every input to one format rather than
stream-copying, because inputs that merely *look* alike — same nominal
size, different pixel format or frame rate — concatenate into a file that
plays correctly in the tool that wrote it and stutters everywhere else.

It refuses rather than improvises. A missing clip is an error, not a
silently shorter video: the whole point of the piece being separate is
that nobody notices when it goes missing.

Usage:
  ./demo-assemble.py --out final.mp4 boot.mp4 qos-demo.mp4
  ./demo-assemble.py --out final.mp4 --dry-run boot.mp4 qos-demo.mp4
"""
from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

#: What every segment is normalised to. The nine-beat cut is authored at
#: this size and rate, so matching it means that clip passes through
#: unchanged in everything but container framing.
WIDTH, HEIGHT, FPS = 1920, 1080, 30


def probe(path: pathlib.Path) -> dict:
    """Duration, size and frame rate of `path`, or raise with what ffprobe said."""
    out = subprocess.run(
        [
            "ffprobe", "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height,r_frame_rate,pix_fmt",
            "-show_entries", "format=duration",
            "-of", "json", str(path),
        ],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        raise SystemExit(f"ffprobe could not read {path}:\n{out.stderr.strip()}")
    d = json.loads(out.stdout)
    if not d.get("streams"):
        raise SystemExit(f"{path} has no video stream")
    s = d["streams"][0]
    num, _, den = s["r_frame_rate"].partition("/")
    return {
        "width": s["width"],
        "height": s["height"],
        "pix_fmt": s.get("pix_fmt", "?"),
        "fps": (float(num) / float(den)) if float(den) else 0.0,
        "duration": float(d["format"]["duration"]),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("clips", nargs="+", help="segments, in the order they play")
    ap.add_argument("--out", required=True, help="where to write the joined file")
    ap.add_argument("--dry-run", action="store_true",
                    help="report what each segment is and what the total would be, write nothing")
    args = ap.parse_args()

    if not shutil.which("ffmpeg") or not shutil.which("ffprobe"):
        print("ffmpeg and ffprobe are required", file=sys.stderr)
        return 1

    clips = [pathlib.Path(c) for c in args.clips]
    missing = [c for c in clips if not c.is_file()]
    if missing:
        for m in missing:
            print(f"MISSING  {m}", file=sys.stderr)
        print("\nRefusing to assemble. A segment that is absent would leave a shorter\n"
              "video that looks complete, which is the failure this check exists for.",
              file=sys.stderr)
        return 1

    total = 0.0
    print(f"{'segment':<44} {'size':>11} {'fps':>6} {'seconds':>8}")
    for c in clips:
        i = probe(c)
        total += i["duration"]
        note = "" if (i["width"], i["height"]) == (WIDTH, HEIGHT) else "  → rescaled"
        print(f"{c.name:<44} {i['width']}x{i['height']:<5} {i['fps']:>6.1f} "
              f"{i['duration']:>8.2f}{note}")
    print(f"{'':<44} {'':>11} {'':>6} {total:>8.2f}  total")

    if args.dry_run:
        return 0

    # Normalise each segment separately, then concatenate the results. Doing
    # it in one filter_complex would work too, but a per-segment pass means a
    # segment that fails to encode names itself instead of collapsing the
    # whole graph into one opaque error.
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="qos-assemble-"))
    try:
        parts: list[pathlib.Path] = []
        for n, c in enumerate(clips):
            part = tmp / f"{n:02d}.mp4"
            # `setsar=1` matters: a segment with a non-square sample aspect
            # concatenates without complaint and then plays stretched.
            vf = (f"scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,"
                  f"pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2:black,"
                  f"setsar=1,fps={FPS}")
            r = subprocess.run(
                ["ffmpeg", "-v", "error", "-i", str(c), "-vf", vf,
                 "-c:v", "libx264", "-preset", "medium", "-crf", "18",
                 "-pix_fmt", "yuv420p", "-an", str(part), "-y"],
                capture_output=True, text=True,
            )
            if r.returncode != 0:
                print(f"could not normalise {c}:\n{r.stderr.strip()}", file=sys.stderr)
                return 1
            parts.append(part)
            print(f"  normalised {c.name}")

        listing = tmp / "parts.txt"
        listing.write_text("".join(f"file '{p.name}'\n" for p in parts), encoding="utf-8")
        # Absolute: the concat runs with cwd set to the temp directory so
        # the listing can name its parts relatively, and a relative output
        # path would be written there — or, as happened, fail to open at
        # all because its parent does not exist from in there.
        out = pathlib.Path(args.out).resolve()
        out.parent.mkdir(parents=True, exist_ok=True)
        r = subprocess.run(
            ["ffmpeg", "-v", "error", "-f", "concat", "-safe", "0",
             "-i", str(listing), "-c", "copy", str(out), "-y"],
            capture_output=True, text=True, cwd=tmp,
        )
        if r.returncode != 0:
            print(f"could not join the segments:\n{r.stderr.strip()}", file=sys.stderr)
            return 1

        got = probe(out)
        print(f"\n{out}  {got['width']}x{got['height']}  {got['duration']:.2f}s")
        # Re-encoding shifts durations by a frame or two; a larger gap means a
        # segment was dropped or truncated, which is worth saying out loud.
        if abs(got["duration"] - total) > 1.0:
            print(f"WARNING: expected about {total:.2f}s from the segments but the "
                  f"joined file is {got['duration']:.2f}s — a segment may be short.",
                  file=sys.stderr)
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
