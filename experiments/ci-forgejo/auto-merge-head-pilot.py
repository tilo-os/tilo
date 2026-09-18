#!/usr/bin/env python3
"""Probe native auto-merge after a head change in the existing private fixture."""
import base64
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import time

REPO = "qos/ci-workflow-pilot-20260918-9245-repeat"
BASE = "02d49e600045f5108dbbfb127033fe564d909a9e"
OUT = Path(__file__).parent / "results/auto-merge-head-20260918"


def main():
    OUT.mkdir(parents=True, exist_ok=False)
    requests = []

    def api(method, path, body=None, required=True):
        response = subprocess.run(["fgj", "--hostname", "code.podesta.ai", "api", "-i", "-X", method,
                                   f"repos/{REPO}{path}"], input=json.dumps(body) if body is not None else None,
                                  text=True, capture_output=True)
        match = re.search(r"HTTP/\S+ (\d+)", response.stdout)
        status = int(match[1]) if match else None
        payload = response.stdout.split("\n\n", 1)[-1]
        try:
            value = json.loads(payload) if payload.strip() else None
        except ValueError:
            value = None
        row = {"at": datetime.now(timezone.utc).isoformat(), "method": method, "path": path,
               "request": body, "http_status": status, "exit_code": response.returncode}
        if response.returncode:
            row["error"] = value or response.stderr[:1200]
        requests.append(row)
        (OUT / "requests.json").write_text(json.dumps(requests, indent=2) + "\n")
        if required and response.returncode:
            raise RuntimeError(str(row))
        return status, value

    _, repo = api("GET", "")
    assert repo["private"] and repo["full_name"] == REPO
    _, main_branch = api("GET", "/branches/main")
    assert main_branch["commit"]["id"] == BASE, "pilot has advanced; reconcile before running"
    api("POST", "/branches", {"new_branch_name": "probe-auto-head", "old_ref_name": BASE})
    _, first = api("POST", "/contents/auto-first.txt", {
        "branch": "probe-auto-head", "message": "Initial auto-merge fixture",
        "content": base64.b64encode(b"first\n").decode()})
    original = first["commit"]["sha"]
    _, pr = api("POST", "/pulls", {"base": "main", "head": "probe-auto-head",
                                  "title": "Isolated expected-head auto-merge probe"})
    number = pr["number"]
    path = f"/pulls/{number}"
    result = {"repository": REPO, "base": BASE, "pr": number, "original_head": original,
              "scope": "native auto-merge with fixture status, one admin identity; no independent review"}

    def save():
        (OUT / "result.json").write_text(json.dumps(result, indent=2) + "\n")

    def status(sha, state):
        api("POST", f"/statuses/{sha}", {"context": "pilot/ready", "state": state,
                                            "description": "Synthetic head-identity probe, not a build"})

    def observe(seconds):
        deadline = time.monotonic() + seconds
        while True:
            _, current = api("GET", path)
            result["last_observation"] = {"merged": current["merged"], "head": current["head"]["sha"],
                                          "merge_commit_sha": current["merge_commit_sha"]}
            save()
            if current["merged"] or time.monotonic() >= deadline:
                return current["merged"]
            time.sleep(3)

    save()
    status(original, "pending")
    try:
        accepted, _ = api("POST", path + "/merge", {"Do": "fast-forward-only", "head_commit_id": original,
                         "force_merge": False, "merge_when_checks_succeed": True}, required=False)
        result["queue_response"] = accepted
        save()
        if accepted not in (200, 201, 202):
            raise RuntimeError("native queue request was not accepted")
        _, second = api("POST", "/contents/auto-second.txt", {
            "branch": "probe-auto-head", "message": "New unqueued head fixture",
            "content": base64.b64encode(b"second\n").decode()})
        updated = second["commit"]["sha"]
        result["updated_head"] = updated
        save()
        status(original, "success")
        result["merged_after_old_head_green"] = observe(12)
        if not result["merged_after_old_head_green"]:
            status(updated, "success")
            result["merged_after_new_head_green_without_requeue"] = observe(30)
    except BaseException as error:
        result["probe_error"] = str(error)
        save()
        raise
    finally:
        # Also clean up an accepted request after transport failure/interruption.
        # A bounded absence of a merge is an observation, not proof of rejection.
        try:
            cancel, _ = api("DELETE", path + "/merge", required=False)
            result["cancel_response"] = cancel
            _, current = api("GET", path)
            result["cleanup_confirmed"] = current["merged"] or cancel in (200, 204)
            _, final = api("GET", "/branches/main")
            result["final_main"] = final["commit"]["id"]
            if not result["cleanup_confirmed"]:
                raise RuntimeError("pending auto-merge cancellation unconfirmed; inspect pilot")
        except BaseException as error:
            result["cleanup_error"] = str(error)
            raise
        finally:
            save()
    print(json.dumps(result, indent=2), flush=True)


if __name__ == "__main__":
    main()
