#!/usr/bin/env python3
"""Check retained private-pilot dispatch requests against server/job identities."""
import json
from pathlib import Path

ROOT = Path(__file__).parent / "results/dispatch-ref-20260919"


def read(name):
    return json.loads((ROOT / name).read_text(encoding="utf-8-sig"))


def main():
    setup = read("setup.json")
    queued = {r["id"]: r for r in read("after-advance-runs.json")}
    final = {r["id"]: r for r in read("final-runs.json")}
    jobs = {j["run_id"]: j for j in read("jobs-initial.json")}
    assert len(queued) == len(final) == len(jobs) == 2
    a, b, c = (setup[key] for key in (
        "immutable_requested_sha", "branch_head_when_dispatching", "branch_head_after_dispatch"))
    assert len({a, b, c}) == 3
    for kind, sha, ref in (("sha", a, a), ("branch", b, "refs/heads/" + setup["branch"])):
        dispatch = setup[kind + "_dispatch"]
        assert dispatch["http_status"] == 201
        assert dispatch["request"]["return_run_info"] is True
        assert dispatch["request"]["ref"] == (a if kind == "sha" else setup["branch"])
        run_id = dispatch["response"]["id"]
        assert queued[run_id]["commit_sha"] == final[run_id]["commit_sha"] == sha
        assert final[run_id]["status"] == jobs[run_id]["status"] == "success"
        assert final[run_id]["event"] == "" and final[run_id]["trigger_event"] == "workflow_dispatch"
        log = (ROOT / f"job-{jobs[run_id]['id']}-selected-logs.txt").read_text(encoding="utf-8-sig")
        lines = [line for line in log.splitlines() if "DISPATCH_IDENTITY=" in line]
        assert len(lines) == 1
        identity = json.loads(lines[0].split("DISPATCH_IDENTITY=", 1)[1])
        assert identity == {"PROBE_SHA": sha, "PROBE_REF": ref}
    print("PASS: immutable SHA and resolved branch heads match both completed job contexts")


if __name__ == "__main__":
    main()
