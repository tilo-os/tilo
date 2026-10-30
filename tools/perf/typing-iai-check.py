#!/usr/bin/env python3
"""Reject typing profiles that did not observe the real input path."""

import json
from pathlib import Path
import sys

EVENTS = "Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw".split()
CASES = {
    (function, size)
    for function in ("single_character_to_scene", "single_key_to_scene")
    for size in ("small", "large", "large_at_end")
}


def check_part(path):
    """Reconcile exclusive events and identify charged production input work."""
    totals = [0] * len(EVENTS)
    summary = None
    function = ""
    called = False
    input_observed = False
    positions_seen = events_seen = False
    with path.open() as stream:
        for line in stream:
            if line.startswith("positions:"):
                if line.split()[1:] != ["line"]:
                    raise ValueError(f"{path}: unsupported cost positions")
                positions_seen = True
            elif line.startswith("events:"):
                if line.split()[1:] != EVENTS:
                    raise ValueError(f"{path}: wrong cache event configuration")
                events_seen = True
            elif line.startswith("summary:"):
                summary = list(map(int, line.split()[1:]))
                summary += [0] * (len(EVENTS) - len(summary))
            elif line.startswith("fn="):
                function = line[3:].strip()
            elif line.startswith("calls="):
                called = True
            elif line and (line[0].isdigit() or line[0] in "+-*"):
                costs = list(map(int, line.split()[1:]))
                costs += [0] * (len(EVENTS) - len(costs))
                if len(costs) != len(EVENTS):
                    raise ValueError(f"{path}: malformed cost record")
                if not called:
                    totals = [a + b for a, b in zip(totals, costs)]
                if "<tilo_ui::headless::FrameHarness>::key" == function and costs[0] > 0:
                    input_observed = True
                called = False
    if not positions_seen or not events_seen:
        raise ValueError(f"{path}: missing cost configuration")
    if summary is None or totals != summary:
        raise ValueError(f"{path}: exclusive events do not reconcile with its summary")
    return input_observed, totals


def check_profiles(root):
    found = set()
    for path in root.rglob("summary.json"):
        document = json.loads(path.read_text())
        case = (document["function_name"], document["id"])
        if case not in CASES or case in found:
            raise ValueError(f"{path}: unexpected or duplicate typing case {case}")
        found.add(case)
        observed = False
        for profile in document["profiles"]:
            if profile["tool"] != "Callgrind":
                continue
            if not profile["out_paths"]:
                raise ValueError(f"{path}: no raw Callgrind parts")
            totals = [0] * len(EVENTS)
            for output in profile["out_paths"]:
                # Stored paths refer to the execution container; downloaded evidence
                # retains the same filenames next to this summary.
                charged, costs = check_part(path.parent / Path(output).name)
                observed = observed or charged
                totals = [a + b for a, b in zip(totals, costs)]
            metrics = profile["summaries"]["total"]["summary"]["Callgrind"]
            for event, total in zip(EVENTS, totals):
                pair = metrics[event]["metrics"]
                current = pair["Left"] if "Left" in pair else pair["Both"][0]
                if current["Int"] != total:
                    raise ValueError(f"{path}: {event} JSON total differs from raw parts")
        if not observed:
            raise ValueError(f"{path}: profile did not observe FrameHarness::key")
    if found != CASES:
        raise ValueError(f"missing typing cases: {sorted(CASES - found)}")
    return len(found)


if __name__ == "__main__":
    count = check_profiles(Path(sys.argv[1]))
    print(f"{count} typing cases observed real key work; all nine raw event sums reconcile")
