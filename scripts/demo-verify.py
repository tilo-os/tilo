#!/usr/bin/env python3
"""Read a recorded demo back — the pixels, not the assertions.

Either rig writes a run directory and composes a film out of it into
`films/`. Everything in that run is a claim until someone looks at the
frames, and a claim that passes every byte-level check can still be a
peer's caret hovering over an empty page. This script is the looking: it
finds where each section of the film lives in the finished video and cuts
the frames out of it so they can be read.

It changes nothing and it re-derives everything from artifacts:

  track.txt      which source frame is on screen, and for how long
  frames/*.png   the frames themselves, and their capture times
  captions/*.txt every string the band draws, verbatim
  the .mp4       what actually shipped

**Both rigs, and they divide the film differently.** `yc-demo.py --record`
prints numbered beat banners, so a frame can be attributed to beat 7 and
the sections are beats. `yc-demo-live.py` is driven by a person and has no
step list to print, so it prints no banners — and this script used to die
on that with "was it stamped?", which is a true statement about a log that
was stamped and misdiagnoses the run somebody is most likely to be
verifying. With no banners the sections are the camera spans instead, off
the `[ on camera: … ]` lines both rigs print, and the transcript says
which of the two it did.

The transcript supplies the clock. Run the rig through a stamper so each
line carries the wall-clock second it was printed:

    python3 -u scripts/yc-demo-live.py 2>&1 \
      | python3 -c 'import sys,time
for line in sys.stdin:
    sys.stdout.write(f"{time.time():.3f} {line}"); sys.stdout.flush()' \
      | tee run.log

then

    scripts/demo-verify.py <record-dir> --log run.log --out frames-out

Section windows come from matching those stamps against frame mtimes, so a
frame attributed to a beat is one that was captured while that beat was
running — not one that a hand-written timestamp guessed at.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

BAND_H = 66            # must match yc-demo.py
VIDEO_W, VIDEO_H = 1920, 1080

BEAT_RE = re.compile(r"^\s*([1-9])\.\s{2}(.+?)\s*$")
CUT_RE = re.compile(r"^\s*\[ on camera: (\w+) \]\s*$")
STAMP_RE = re.compile(r"^(\d+\.\d+) (.*)$")


def read_track(directory: str) -> list[tuple[str, float]]:
    """`(relative frame path, seconds on screen)` in composition order."""
    path = os.path.join(directory, "track.txt")
    entries: list[tuple[str, float]] = []
    pending: str | None = None
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if line.startswith("file "):
                pending = line[5:].strip().strip("'\"")
            elif line.startswith("duration ") and pending is not None:
                entries.append((pending, float(line[9:])))
                pending = None
    if not entries:
        raise SystemExit(f"{path} lists no frames")
    return entries


def frame_clock(directory: str, track: list[tuple[str, float]]) -> list[dict]:
    """Each track entry, with when it was captured and when it shows.

    Capture time is the frame file's mtime: the recorder writes each PNG
    as it takes it, so the file's own timestamp is the moment the
    session was sampled. Video time is the running sum of the
    durations the track asks for.
    """
    out: list[dict] = []
    at = 0.0
    for rel, seconds in track:
        full = os.path.join(directory, rel)
        out.append({
            "file": rel,
            "captured": os.path.getmtime(full),
            "video_t": at,
            "shown": seconds,
        })
        at += seconds
    return out


def read_log(path: str) -> tuple[list[dict], list[dict]]:
    """Beat banners and camera cuts, each with the second it was printed."""
    beats: list[dict] = []
    cuts: list[dict] = []
    with open(path, errors="replace") as handle:
        for raw in handle:
            stamped = STAMP_RE.match(raw.rstrip("\n"))
            if not stamped:
                continue
            when, text = float(stamped.group(1)), stamped.group(2)
            beat = BEAT_RE.match(text)
            if beat:
                beats.append({"n": int(beat.group(1)),
                              "title": beat.group(2), "at": when})
                continue
            cut = CUT_RE.match(text)
            if cut:
                cuts.append({"session": cut.group(1), "at": when})
    return beats, cuts


def windows(beats: list[dict], frames: list[dict]) -> list[dict]:
    """Each beat's span in video time, via the frames captured inside it.

    An empty list of beats is an empty list of windows, not an error: a
    live take prints no banners, and the caller says so and falls back to
    the camera spans. Refusing here reported a stamped log as unstamped
    and killed the verification of the one take somebody actually films.
    """
    out: list[dict] = []
    for i, beat in enumerate(beats):
        start = beat["at"]
        end = beats[i + 1]["at"] if i + 1 < len(beats) else float("inf")
        inside = [f for f in frames if start <= f["captured"] < end]
        out.append({
            **beat,
            "frames": inside,
            "from": inside[0]["video_t"] if inside else None,
            "to": (inside[-1]["video_t"] + inside[-1]["shown"]
                   if inside else None),
        })
    return out


def find_video(directory: str) -> str | None:
    """The film this run directory produced, if it can be found without asking.

    `<directory>/qos-demo.mp4` was the old place and is still honoured.
    Both rigs now write a timestamped file into `films/` at the top of the
    checkout instead, so that is where this looks next — newest first, and
    skipping the boot bumper and anything already joined to one, because a
    bumper on the front shifts every timestamp in this file by its own
    length and every window below would be wrong by that much.

    Returns None rather than guessing wildly; `--video` is the answer when
    there is more than one candidate and this picked the wrong one.
    """
    legacy = os.path.join(directory, "qos-demo.mp4")
    if os.path.isfile(legacy):
        return legacy
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    films = os.path.join(repo, "films")
    if not os.path.isdir(films):
        return None
    best, best_at = None, 0.0
    for name in os.listdir(films):
        if not name.endswith(".mp4"):
            continue
        if name.startswith("qos-boot-bumper") or "-with-bumper" in name:
            continue
        path = os.path.join(films, name)
        at = os.path.getmtime(path)
        if at > best_at:
            best, best_at = path, at
    return best


def cut_spans(cuts: list[dict], frames: list[dict]) -> list[dict]:
    """When each session was on camera, in video time.

    Derived from the frames themselves rather than from when a cut was
    ordered: a cut takes effect at the next capture, and trimming moves
    everything after it.
    """
    spans: list[dict] = []
    for i, cut in enumerate(cuts):
        start = cut["at"]
        end = cuts[i + 1]["at"] if i + 1 < len(cuts) else float("inf")
        inside = [f for f in frames if start <= f["captured"] < end]
        if not inside:
            continue
        first = inside[0]["video_t"]
        last = inside[-1]["video_t"] + inside[-1]["shown"]
        # Cutting to whoever is already on camera is a no-op in the
        # recorder, and the transcript prints it either way. Merged here
        # so a span is a stretch of one session's pixels rather than a
        # stretch of one `cut()` call.
        if spans and spans[-1]["session"] == cut["session"]:
            spans[-1]["to"] = last
            spans[-1]["frames"] += len(inside)
            continue
        spans.append({
            "session": cut["session"],
            "from": first,
            "to": last,
            "frames": len(inside),
        })
    return spans


def extract(video: str, at: float, out_path: str) -> bool:
    done = subprocess.run(
        ["ffmpeg", "-y", "-v", "error", "-ss", f"{at:.3f}", "-i", video,
         "-frames:v", "1", out_path],
        capture_output=True, text=True)
    return done.returncode == 0 and os.path.exists(out_path)


def still_runs(video: str) -> dict:
    """How long the finished video ever holds one unchanging picture.

    Decoded small and greyscale — enough to tell one screen from
    another, cheap enough to hash every frame. A long run is not by
    itself dishonest (a session waiting really does render the same
    screen), but a run longer than the recorder's own cap means
    something padded the runtime, and that is worth knowing.
    """
    w, h = 160, 90
    proc = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-i", video,
         "-vf", f"scale={w}:{h}", "-pix_fmt", "gray",
         "-f", "rawvideo", "-"],
        stdout=subprocess.PIPE)
    size = w * h
    digests: list[str] = []
    assert proc.stdout is not None
    while True:
        buf = proc.stdout.read(size)
        if len(buf) < size:
            break
        digests.append(hashlib.blake2b(buf, digest_size=8).hexdigest())
    proc.wait()
    longest, run, start, best_start = 0, 0, 0, 0
    for i, digest in enumerate(digests):
        if i and digest == digests[i - 1]:
            run += 1
        else:
            start, run = i, 1
        if run > longest:
            longest, best_start = run, start
    return {"frames": len(digests), "longest_run": longest,
            "run_starts_at_frame": best_start,
            "distinct": len(set(digests))}


def band_report(directory: str) -> dict:
    """Every string the lower band draws, read off the caption files.

    The band is the one thing composed rather than rendered, so what it
    says is not visible in any session's own frames. drawtext reads
    these files verbatim, so the set of them *is* the set of strings on
    screen — no OCR needed to know whether a claim line slipped in.
    """
    captions = os.path.join(directory, "captions")
    if not os.path.isdir(captions):
        return {"files": 0, "texts": []}
    texts = []
    for name in sorted(os.listdir(captions)):
        with open(os.path.join(captions, name)) as handle:
            texts.append(handle.read())
    return {"files": len(texts), "texts": sorted(set(texts))}


def probe(video: str) -> dict:
    done = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
         "-show_entries",
         "stream=width,height,nb_read_frames,r_frame_rate,duration,codec_name",
         "-of", "json", video],
        capture_output=True, text=True, check=True)
    return json.loads(done.stdout)["streams"][0]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("directory", help="the --record directory")
    ap.add_argument("--log", required=True,
                    help="stamped transcript of the run (see the docstring)")
    ap.add_argument("--out", default=None,
                    help="where to write the extracted frames "
                         "(default <directory>/verify)")
    ap.add_argument("--video", default=None,
                    help="the composed file (default: <directory>/qos-demo.mp4 "
                         "if it is there, else the newest take in films/)")
    ap.add_argument("--per-beat", type=int, default=4,
                    help="frames to cut out of each section (default 4)")
    args = ap.parse_args()

    video = args.video or find_video(args.directory)
    if not video or not os.path.isfile(video):
        raise SystemExit(
            "no composed film to read back. Both rigs write a timestamped "
            "file into films/ at the top of the checkout; name it with "
            "--video.")
    if not args.video:
        print(f"film     {video} (found; --video overrides)")
    out = args.out or os.path.join(args.directory, "verify")
    os.makedirs(out, exist_ok=True)

    track = read_track(args.directory)
    frames = frame_clock(args.directory, track)
    beats, cuts = read_log(args.log)
    beat_windows = windows(beats, frames)
    spans = cut_spans(cuts, frames)

    stream = probe(video)
    print(f"video    {stream['width']}x{stream['height']} "
          f"{stream['codec_name']} {stream['r_frame_rate']} "
          f"{float(stream['duration']):.2f}s "
          f"{stream['nb_read_frames']} frames")
    if (int(stream["width"]), int(stream["height"])) != (VIDEO_W, VIDEO_H):
        print(f"  ! not {VIDEO_W}x{VIDEO_H}")

    unique = len({rel for rel, _ in track})
    held = max(seconds for _, seconds in track)
    print(f"track    {len(track)} source frames, {unique} distinct, "
          f"longest single frame held {held:.2f}s")
    if unique != len(track):
        print("  ! a source frame appears more than once in the track")

    band = band_report(args.directory)
    print(f"band     {band['files']} caption(s), "
          f"{len(band['texts'])} distinct string(s):")
    for text in band["texts"]:
        print(f"           {text!r}")

    runs = still_runs(video)
    print(f"stills   longest unchanging run {runs['longest_run']} frames "
          f"(from frame {runs['run_starts_at_frame']}), "
          f"{runs['distinct']} distinct pictures in {runs['frames']}")

    print("\ncamera:")
    for span in spans:
        print(f"  {span['from']:6.2f}–{span['to']:6.2f}s  {span['session']:8} "
              f"{span['frames']:4d} frames")

    # How the film gets divided up for the extraction below.
    #
    # A scripted take prints numbered banners, so a section is a beat and
    # a frame belongs to the beat that was running when it was captured.
    # A live take has no step list to print -- it is driven by a person --
    # so its sections are the stretches one session was on camera, off the
    # `[ on camera: ... ]` lines both rigs print. Saying which of the two
    # this is, out loud, is the point: this used to exit here with "was it
    # stamped?", which is a false statement about a stamped log and
    # stopped the verification of the take somebody actually films.
    if beat_windows:
        sections = [{"label": f"{b['n']}. {b['title']}",
                     "slug": f"beat{b['n']}",
                     "from": b["from"], "to": b["to"],
                     "frames": len(b["frames"]),
                     "beat": b["n"], "title": b["title"]}
                    for b in beat_windows]
        print(f"\nbeats:   {len(sections)} numbered banners in the log")
    else:
        sections = [{"label": f"on {c['session']}'s screen",
                     "slug": f"cam{i + 1}-{c['session']}",
                     "from": c["from"], "to": c["to"],
                     "frames": c["frames"],
                     "beat": None, "title": c["session"]}
                    for i, c in enumerate(spans)]
        print("\nbeats:   none. This log has no numbered banners, which is "
              "what a take driven\n         by a person looks like -- "
              "yc-demo-live.py has no fixed step list\n         to print. "
              "Everything above is checked either way; the frames\n"
              "         below are cut off the camera spans instead of off "
              "beats.")
        if not sections:
            print("         ! and no camera cuts either, so there is nothing "
                  "to cut frames out\n           of. Was the log the rig's "
                  "own stdout, and was it stamped?")

    print("\nsections:")
    index: list[dict] = []
    n = max(1, args.per_beat)
    for section in sections:
        if section["from"] is None:
            print(f"  {section['label']}\n"
                  f"     ! no frames captured here")
            continue
        print(f"  {section['label']}\n"
              f"     {section['from']:.2f}–{section['to']:.2f}s "
              f"({section['frames']} frames)")
        span = section["to"] - section["from"]
        cut_paths = []
        for i in range(n):
            # Inside the section rather than on its edges: the first
            # moment of one is usually still the previous screen.
            at = section["from"] + span * (i + 1) / (n + 1)
            path = os.path.join(out, f"{section['slug']}-{i + 1}-t{at:.1f}.png")
            if extract(video, at, path):
                cut_paths.append(path)
                print(f"     {path}")
            else:
                print(f"     ! could not extract at {at:.2f}s")
        index.append({"beat": section["beat"], "title": section["title"],
                      "label": section["label"],
                      "from": section["from"], "to": section["to"],
                      "frames": cut_paths})

    with open(os.path.join(out, "index.json"), "w") as handle:
        json.dump({"video": video, "stream": stream,
                   "divided_by": "beats" if beat_windows else "camera",
                   "beats": index, "sections": index,
                   "camera": spans, "band": band, "stills": runs},
                  handle, indent=2)
    print(f"\nindex: {os.path.join(out, 'index.json')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
