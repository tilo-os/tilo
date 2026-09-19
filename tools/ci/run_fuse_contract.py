#!/usr/bin/env python3
"""Dispatch the exact PR head to the exclusive FUSE lane and propagate its result."""
import json
import os
import re
import sys
import time
import uuid
from urllib import error, request


WORKFLOW = "fs-fuse-executor.yml"
PENDING = {"waiting", "queued", "running", "blocked"}
FAILED = {"failure", "cancelled", "skipped"}


class Forgejo:
    def __init__(self, base, token):
        self.base = base.rstrip("/")
        self.token = token

    def call(self, method, path, payload=None):
        data = None if payload is None else json.dumps(payload).encode()
        req = request.Request(
            f"{self.base}/{path.lstrip('/')}", data=data, method=method,
            headers={"Authorization": f"token {self.token}",
                     "Content-Type": "application/json"},
        )
        try:
            with request.urlopen(req, timeout=30) as response:
                body = response.read()
                return {} if not body else json.loads(body)
        except error.HTTPError as exc:
            detail = exc.read().decode(errors="replace")
            raise RuntimeError(f"Forgejo {method} {path} failed ({exc.code}): {detail}") from exc


def runs(api, repository):
    result = api("GET", f"repos/{repository}/actions/runs?page=1&limit=100")
    rows = result.get("workflow_runs")
    if not isinstance(rows, list):
        raise RuntimeError("Forgejo returned no workflow run list")
    return rows


def dispatched_inputs(row):
    payload = row.get("event_payload")
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except json.JSONDecodeError:
            return {}
    if not isinstance(payload, dict):
        return {}
    inputs = payload.get("inputs")
    return inputs if isinstance(inputs, dict) else {}


def execute(api, repository, source_ref, source_sha, request_id=None,
            sleep=time.sleep, attempts=1080):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha or ""):
        raise ValueError("SOURCE_SHA must be an immutable full commit ID")
    if not source_ref or source_ref.startswith("-") or any(c.isspace() for c in source_ref):
        raise ValueError("SOURCE_REF is invalid")
    request_id = str(uuid.uuid4()) if request_id is None else request_id
    if not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", request_id):
        raise ValueError("REQUEST_ID is invalid")

    before = runs(api, repository)
    baseline = max((row.get("id", 0) for row in before if type(row.get("id")) is int), default=0)
    api("POST", f"repos/{repository}/actions/workflows/{WORKFLOW}/dispatches", {
        "ref": source_ref,
        "inputs": {"source_sha": source_sha, "request_id": request_id},
    })

    observed = None
    for _ in range(attempts):
        candidates = [
            row for row in runs(api, repository)
            if row.get("workflow_id") == WORKFLOW
            and row.get("event") == "workflow_dispatch"
            and type(row.get("id")) is int and row["id"] > baseline
        ]
        correlated = [row for row in candidates
                      if dispatched_inputs(row).get("request_id") == request_id]
        exact = [row for row in correlated
                 if row.get("commit_sha") == source_sha
                 and dispatched_inputs(row).get("source_sha") == source_sha]
        if exact:
            observed = min(exact, key=lambda row: row["id"])
            status = observed.get("status")
            if status == "success":
                print(f"exclusive filesystem contract passed: run {observed['id']}")
                return observed
            if status in FAILED:
                raise RuntimeError(
                    f"exclusive filesystem contract run {observed['id']} ended {status}"
                )
            if status not in PENDING:
                raise RuntimeError(
                    f"exclusive filesystem contract run {observed['id']} has unknown status {status!r}"
                )
        elif correlated:
            raise RuntimeError("correlated filesystem dispatch did not bind the exact source")
        sleep(10)
    suffix = f"; last run {observed['id']} was {observed.get('status')}" if observed else ""
    raise RuntimeError(f"timed out waiting for exclusive filesystem contract{suffix}")


def main():
    try:
        server = os.environ["GITHUB_SERVER_URL"].rstrip("/") + "/api/v1"
        repository = os.environ["GITHUB_REPOSITORY"]
        token = os.environ["FORGEJO_TOKEN"]
        if not token:
            raise ValueError("FORGEJO_TOKEN is empty")
        execute(Forgejo(server, token).call, repository,
                os.environ["SOURCE_REF"], os.environ["SOURCE_SHA"],
                os.environ.get("REQUEST_ID"))
    except (KeyError, OSError, TypeError, ValueError, RuntimeError, json.JSONDecodeError) as exc:
        print(f"Cannot run exclusive filesystem contract: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
