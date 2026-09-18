#!/usr/bin/env python3
"""Destructive only to an explicitly named *fresh private pilot* repository.

Uses the configured fgj identity. No production repository is accepted. This
probe qualifies API merge semantics, not independent reviewers or status issuers.
"""
import argparse
import base64
import json
import re
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"qos/ci-workflow-pilot-[a-z0-9-]+", args.repo):
        parser.error("only qos/ci-workflow-pilot-* repositories are allowed")
    args.out.mkdir(parents=True, exist_ok=False)
    evidence = []

    def api(method, path, body=None, *, expect_success=True):
        result = subprocess.run(["fgj", "--hostname", "code.podesta.ai", "api", "-i", "-X", method,
                                 f"repos/{args.repo}" + path],
                                input=json.dumps(body) if body is not None else None,
                                capture_output=True, text=True)
        row = {"method": method, "path": path, "exit_code": result.returncode,
               "request": body}
        status = re.search(r"HTTP/\S+ (\d+)", result.stdout)
        row["http_status"] = int(status[1]) if status else None
        payload = result.stdout.split("\n\n", 1)[-1]
        try:
            value = json.loads(payload) if payload.strip() else None
        except ValueError:
            value = None
        if result.returncode:
            row["error"] = value if value is not None else result.stderr[:1600]
        evidence.append(row)
        (args.out / "requests.json").write_text(json.dumps(evidence, indent=2) + "\n")
        if expect_success and result.returncode:
            raise RuntimeError(f"{method} {path}: {result.stderr}")
        return result.returncode, value

    _, repo = api("GET", "")
    assert repo["private"] and repo["default_branch"] == "main"
    _, branches = api("GET", "/branches")
    assert [b["name"] for b in branches] == ["main"], "pilot must be fresh; inspect existing evidence before retrying"
    base = branches[0]["commit"]["id"]
    api("PATCH", "", {"allow_fast_forward_only_merge": True})
    api("POST", "/branch_protections", {
        "rule_name": "main", "enable_push": False, "apply_to_admins": True,
        "enable_status_check": True, "status_check_contexts": ["pilot/ready"],
        "block_on_outdated_branch": True, "dismiss_stale_approvals": True,
        "required_approvals": 0,
    })
    probes = {}
    for name in ("a", "b"):
        branch = "probe-" + name
        api("POST", "/branches", {"new_branch_name": branch, "old_ref_name": base})
        _, change = api("POST", f"/contents/{name}.txt", {
            "branch": branch, "message": f"Pilot candidate {name}",
            "content": base64.b64encode((name + "\n").encode()).decode(),
        })
        _, pr = api("POST", "/pulls", {"base": "main", "head": branch,
                    "title": f"Isolated merge semantics probe {name}"})
        probes[name] = {"sha": change["commit"]["sha"], "number": pr["number"]}

    def merge(probe, sha=None):
        return api("POST", f"/pulls/{probe['number']}/merge", {
            "Do": "fast-forward-only", "head_commit_id": sha or probe["sha"],
            "force_merge": False, "merge_when_checks_succeed": False,
        }, expect_success=False)[0]

    def rejected(probe, status, reason, sha=None):
        code = merge(probe, sha)
        if code == 0:
            return False
        response = evidence[-1]
        if response["http_status"] != status or reason not in str(response.get("error")):
            raise RuntimeError(f"inconclusive rejection: expected HTTP {status} / {reason}; got {response}")
        return True

    result = {"repository": args.repo, "base": base, "probes": probes,
              "scope": "single-admin-identity API semantics; no independent review or runner qualification"}
    result["missing_check_rejected"] = rejected(probes["a"], 405, "Not all required status checks successful")
    for probe in probes.values():
        api("POST", f"/statuses/{probe['sha']}", {
            "context": "pilot/ready", "state": "success", "description": "Fixture status, not build evidence",
        })
    result["wrong_head_rejected"] = rejected(probes["a"], 409, "head out of date", "0" * 40)
    # Status propagation/mergeability is asynchronous. A temporary refusal is
    # not evidence of stale-base enforcement. Retain every response and require
    # a successful first merge before probing the competing stale candidate.
    result["first_candidate_merged"] = False
    for attempt in range(5):
        if merge(probes["a"]) == 0:
            result["first_candidate_merged"] = True
            break
        # An interrupted response could conceal a successful merge. Reconcile
        # its durable state before retrying the same expected-head request.
        _, observed = api("GET", f"/pulls/{probes['a']['number']}")
        if observed["merged"]:
            result["first_candidate_merged"] = observed["merge_commit_sha"] == probes["a"]["sha"]
            break
        time.sleep(2)
    if not result["first_candidate_merged"]:
        (args.out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        raise RuntimeError("first candidate did not merge; stale-candidate probe not run")
    result["stale_candidate_rejected"] = rejected(probes["b"], 405, "head branch is behind the base branch")
    _, final = api("GET", "/branches/main")
    result["final_main"] = final["commit"]["id"]
    result["main_is_exact_tested_head"] = result["final_main"] == probes["a"]["sha"]
    (args.out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    assert all(result[key] for key in ("missing_check_rejected", "wrong_head_rejected",
               "first_candidate_merged", "stale_candidate_rejected", "main_is_exact_tested_head"))


if __name__ == "__main__":
    main()
