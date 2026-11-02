#!/usr/bin/env python3
"""Turn a session's own startup marks into a measurement the budget check reads.

The session writes them when it runs with `TILO_STARTUP_METRICS_PATH=<file>:`

    TILO_STARTUP_METRICS_PATH=~/session-startup.json tilo-session --backend headless

`tilo-startup-metrics` records each mark against the process's origin, so the
file is a timeline, not a set of durations. This reads it into the phases a
person waits through, for one device class:

    startup_marks.py FILE --device opi3b [--json OUT]

A mark that did not happen is reported, and a measurement missing its first
frame fails the budget check rather than passing quietly.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

#: Phases in the order a login pays them. Each is named by the mark that ends
#: it and the mark it starts at (`None` is the process origin), with an
#: earlier pair kept for a session that still shows the deleted compositor's
#: desk. A mark that is absent leaves its phase out of the report.
PHASES = [
    ("process", [(None, "main-start")]),
    (
        "setup",
        [
            ("main-start", "desk-build-start"),
            ("main-start", "compositor-native-run-start"),
        ],
    ),
    (
        "compositor",
        [("compositor-native-run-start", "compositor-desktop-build-start")],
    ),
    (
        "desk_build",
        [
            ("desk-build-start", "desk-build-done"),
            ("compositor-desktop-build-start", "compositor-desktop-build-done"),
        ],
    ),
    (
        "window",
        [
            ("desk-build-done", "window-render-context-done"),
            ("compositor-desktop-build-done", "window-render-context-done"),
        ],
    ),
    ("gpu", [("window-render-context-done", "window-vello-init-done")]),
    ("render", [("render-worker-start", "first-frame-presented")]),
]


def marks(path: pathlib.Path) -> dict[str, float]:
    """Mark name -> milliseconds from the process origin."""
    data = json.loads(path.read_text())
    events = data.get("events", [])
    return {event["name"]: event["ns"] / 1e6 for event in events if "name" in event and "ns" in event}


def measure(path: pathlib.Path, device: str) -> dict:
    timeline = marks(path)
    gaps: dict[str, float] = {}
    for name, pairs in PHASES:
        # The first pair whose marks a session wrote names its phase: a
        # session that still shows the deleted compositor's desk has the
        # later pairs' marks only.
        for start, end in pairs:
            if end not in timeline:
                continue
            first = 0.0 if start is None else timeline.get(start)
            if first is None:
                continue
            gaps[name] = timeline[end] - first
            break
    first_frame = timeline.get("first-frame-presented")
    return {
        "kind": "startup_marks",
        "device": device,
        "source": str(path),
        "marks_ms": timeline,
        "phases_ms": gaps,
        "first_frame_ms": first_frame,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("marks", type=pathlib.Path)
    parser.add_argument("--device", required=True, help="budget class, e.g. opi3b or x86")
    parser.add_argument("--json", type=pathlib.Path, help="write the measurement here")
    args = parser.parse_args()

    measurement = measure(args.marks, args.device)
    text = json.dumps(measurement, indent=2)
    if args.json:
        args.json.write_text(text)
    print(f"device {measurement['device']}")
    for name, ms in measurement["phases_ms"].items():
        print(f"  {name:<20} {ms:>8.1f} ms")
    first = measurement["first_frame_ms"]
    if first is None:
        print("no first-frame-presented mark: the session never presented", file=sys.stderr)
        return 1
    print(f"  {'first frame':<20} {first:>8.1f} ms")
    return 0


if __name__ == "__main__":
    sys.exit(main())
