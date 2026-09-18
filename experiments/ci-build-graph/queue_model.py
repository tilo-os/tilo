#!/usr/bin/env python3
"""Illustrative deterministic packaging model, NOT a Forgejo load benchmark.

One non-preemptible build slot, no failures, identical build duration. Compare
queue-every-commit, cancel-active, and finish-active/replace-pending semantics.
The arrival trace is hypothetical and applies to either PRs or direct pushes.
"""
import argparse
from collections import deque
import json


def simulate(arrivals, duration, policy):
    pending = deque()
    active = None
    now = busy = wasted = 0
    peak_pending = 0
    completed = []
    for revision, arrival in enumerate(arrivals):
        while active and active[2] <= arrival:
            rev, start, finish = active
            completed.append({"revision": rev, "finished": finish,
                              "age": finish - arrivals[rev]})
            busy += finish - start
            now = finish
            active = (pending.popleft(), now, now + duration) if pending else None
        if not active:
            active = (revision, arrival, arrival + duration)
        elif policy == "cancel-active":
            spent = arrival - active[1]
            busy += spent
            wasted += spent
            active = (revision, arrival, arrival + duration)
        elif policy == "latest-pending":
            pending.clear()
            pending.append(revision)
        else:
            pending.append(revision)
        peak_pending = max(peak_pending, len(pending))
    while active:
        rev, start, finish = active
        busy += finish - start
        completed.append({"revision": rev, "finished": finish,
                          "age": finish - arrivals[rev]})
        active = (pending.popleft(), finish, finish + duration) if pending else None
    return {"builds_completed": len(completed), "busy_minutes": busy,
            "discarded_active_minutes": wasted, "peak_pending": peak_pending,
            "latest_revision_ready_at": completed[-1]["finished"],
            "latest_revision_age": completed[-1]["age"], "completions": completed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arrivals", type=int, default=40)
    parser.add_argument("--interval", type=int, default=2)
    parser.add_argument("--duration", type=int, default=12)
    args = parser.parse_args()
    assert min(args.arrivals, args.interval, args.duration) > 0
    trace = [i * args.interval for i in range(args.arrivals)]
    result = {"kind": "hypothetical model, not measured CI", "parameters": vars(args),
              "policies": {policy: simulate(trace, args.duration, policy)
                           for policy in ["fifo", "cancel-active", "latest-pending"]}}
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
