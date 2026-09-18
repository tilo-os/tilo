"""Private-fixture landing command; never a production merge authority.

Default dry-run. Trusted operator supplies policy and, only with --execute, the
existing Kubernetes Secret envelope over stdin. No candidate code is executed.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.error
import urllib.request

import ci_integration_readiness as audit

HOST = "code.podesta.ai"
REPO = "qos/ci-workflow-pilot-20260918-9245-repeat"
INTEGRATOR = "qos-integrator"


def save(path, value, *, exclusive=False):
    text = json.dumps(value, indent=2) + "\n"
    if exclusive:
        with path.open("x", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
    else:
        temp = path.with_suffix(path.suffix + ".tmp")
        with temp.open("w", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        temp.replace(path)


def protections_errors(rule, reviewers):
    errors = []
    expected = {"enable_push": False, "apply_to_admins": True,
                "enable_merge_whitelist": True, "merge_whitelist_usernames": [INTEGRATOR],
                "merge_whitelist_teams": [], "dismiss_stale_approvals": True,
                "block_on_outdated_branch": True, "block_on_rejected_reviews": True,
                "enable_approvals_whitelist": True, "approvals_whitelist_teams": [],
                "enable_status_check": True, "status_check_contexts": ["pilot/ready"]}
    for key, value in expected.items():
        if rule.get(key) != value:
            errors.append("pilot protection mismatch: " + key)
    if rule.get("required_approvals", 0) < 1:
        errors.append("pilot must require approval")
    if sorted(rule.get("approvals_whitelist_username", [])) != sorted(reviewers):
        errors.append("reviewer whitelist mismatch")
    return errors


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, "redirect denied", headers, fp)


def merge_request(number, head, credential):
    # Credential binding comes from the existing operator-owned Secret, not an
    # invented /user claim: this repository-scoped token has no read:user scope.
    if (credential.get("namespace"), credential.get("name"), credential.get("username")) != (
            "forgejo", "qos-integrator-pilot", INTEGRATOR):
        raise ValueError("wrong trusted credential envelope")
    token = credential.get("token")
    if not isinstance(token, str) or not token:
        raise ValueError("missing pilot token")
    body = {"Do": "fast-forward-only", "head_commit_id": head,
            "force_merge": False, "merge_when_checks_succeed": False}
    request = urllib.request.Request(
        f"https://{HOST}/api/v1/repos/{REPO}/pulls/{number}/merge",
        data=json.dumps(body).encode(), method="POST",
        headers={"Authorization": "token " + token, "Content-Type": "application/json"})
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
            return response.status
    except urllib.error.HTTPError as error:
        # Do not serialize arbitrary server text or credentials into receipts.
        status = error.code
        error.close()
        return status


def reconcile(journal, read):
    pr = read(HOST, REPO, f"/pulls/{journal['pr']}")
    main = read(HOST, REPO, "/branches/main")["commit"]["id"]
    journal["readback"] = {"merged": pr.get("merged"), "head": pr.get("head", {}).get("sha"),
                           "merge_commit_sha": pr.get("merge_commit_sha"),
                           "merged_by": (pr.get("merged_by") or {}).get("login"), "main": main}
    if (pr.get("merged") is True and pr.get("merge_commit_sha") == journal["head"]
            and journal["readback"]["merged_by"] == INTEGRATOR):
        journal["state"] = "merged_verified"
    else:
        # Even an unmerged readback after timeout does not authorize a retry:
        # the first request may still be pending in the server.
        journal["state"] = "not_verified_do_not_retry"
    return journal["state"] == "merged_verified"


def land(repo, number, root, policy, path, *, execute=False, credential=None,
         read=audit.api, provenance=audit.provenance_errors, merge=merge_request):
    if repo != REPO or number <= 0:
        raise ValueError("only the private pilot repository is allowed")
    journal = {"schema": 1, "repo": repo, "pr": number, "state": "observing",
               "created_at": datetime.now(timezone.utc).isoformat(), "execute": execute,
               "policy_sha256": hashlib.sha256(json.dumps(policy, sort_keys=True).encode()).hexdigest(),
               "limitations": ["pilot only; final-read-to-merge rerun race is not eliminated",
                               "operator must supply complete trusted executable closure",
                               "credential identity is bound by operator-owned Secret and server whitelist"]}
    save(path, journal, exclusive=True)
    errors = audit.policy_errors(policy)
    try:
        if errors:
            raise ValueError("invalid policy: " + "; ".join(errors))
        repository = read(HOST, repo, "")
        if repository.get("private") is not True or repository.get("full_name") != REPO:
            raise ValueError("private pilot repository identity mismatch")
        permission = read(HOST, repo, f"/collaborators/{INTEGRATOR}/permission")
        if (permission.get("permission") != "write" or
                permission.get("user", {}).get("login") != INTEGRATOR or
                permission.get("user", {}).get("id") != 81):
            raise ValueError("dedicated non-admin integrator grant not observed")
        if INTEGRATOR in policy["reviewers"]:
            raise ValueError("integrator must not be an allowed reviewer")
        rule = read(HOST, repo, "/branch_protections/main")
        errors += protections_errors(rule, policy["reviewers"])
        first = audit.collect(HOST, repo, number, "main", policy, read=read)
        errors += audit.observation_errors(first, repo, number, "main", policy)
        head, base = first["pr"]["head"]["sha"], first["base"]["commit"]["id"]
        journal.update(head=head, base=base)
        if first["pr"]["user"]["login"] == INTEGRATOR:
            errors.append("integrator cannot author the candidate")
        errors += provenance(root, base, head, policy)
        second = audit.collect(HOST, repo, number, "main", policy, read=read)
        errors += audit.observation_errors(second, repo, number, "main", policy)
        if audit.fingerprint(first) != audit.fingerprint(second):
            errors.append("head/base/review/run changed during fresh audit")
        if rule != read(HOST, repo, "/branch_protections/main"):
            errors.append("protection changed during audit")
        journal["evidence"] = audit.fingerprint(second)
        if errors:
            journal.update(state="rejected", errors=errors)
            save(path, journal)
            return 2
        if not execute:
            journal["state"] = "dry_run_ready"
            save(path, journal)
            return 0
        # Persist uncertainty BEFORE sending. A crash from here onward must be
        # reconciled by GET only, never by replaying this journal's POST.
        journal["state"] = "merge_intent_do_not_retry"
        save(path, journal)
        try:
            journal["response_status"] = merge(number, head, credential)
        except (OSError, ValueError) as error:
            journal["response_error"] = type(error).__name__
        save(path, journal)
        success = reconcile(journal, read)
        save(path, journal)
        return 0 if success else 3
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # Preserve intent if any possible mutation occurred; never call it a
        # clean rejection when its outcome is not known.
        if journal["state"] == "observing":
            journal["state"] = "rejected"
        journal["observation_error"] = type(error).__name__
        save(path, journal)
        return 3 if journal["state"] == "merge_intent_do_not_retry" else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--pr", required=True, type=int)
    parser.add_argument("--git-root", required=True, type=Path)
    parser.add_argument("--policy", required=True, type=Path)
    parser.add_argument("--journal", required=True, type=Path)
    parser.add_argument("--execute", action="store_true", help="read trusted credential envelope from stdin and land once")
    parser.add_argument("--reconcile", action="store_true", help="GET-only reconciliation of an existing intent")
    args = parser.parse_args()
    if args.repo != REPO or args.pr <= 0 or (args.execute and args.reconcile):
        parser.error("only private pilot operations are accepted")
    if args.reconcile:
        journal = json.loads(args.journal.read_text(encoding="utf-8"))
        if journal.get("repo") != REPO or journal.get("pr") != args.pr or not audit.SHA.fullmatch(journal.get("head", "")):
            parser.error("journal identity mismatch")
        result = reconcile(journal, audit.api)
        save(args.journal, journal)
        return 0 if result else 3
    credential = json.load(sys.stdin) if args.execute else None
    return land(args.repo, args.pr, args.git_root, json.loads(args.policy.read_text(encoding="utf-8")),
                args.journal, execute=args.execute, credential=credential)


if __name__ == "__main__":
    raise SystemExit(main())
