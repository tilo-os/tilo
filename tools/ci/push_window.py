#!/usr/bin/env python3
"""Which commits a push run to main covers, and whether a newer run took over.

Main takes a push every few minutes from many agents and a full run takes about
an hour, so runs that only ever queue never drain. Forgejo's own `concurrency:`
cannot express what this needs (see the `supersede` job in check.yml): with
`cancel-in-progress: false` it neither replaces nor drops a waiting run, and
with `cancel-in-progress: true` it would cancel runs that are already testing.

So before anything expensive starts, a push run asks the Forgejo API two
questions and answers with two outputs:

* `superseded` -- a newer push run on main is still going to run. This run's
  commits are that one's business, so this one skips its jobs: a clean success,
  since nothing was tested here and nothing was lost. Only a run that never
  reached a runner is superseded; a run that started planning always finishes.
* `base` -- the commit this run's plan starts from: the head of the newest
  earlier push run on main whose `select` job ran (`running` or `success`).
  Commits after that head are what this run measures. A run that skipped, was
  cancelled or broke in `select` planned nothing and is not a base, so the next
  run plans back to the last run that did plan. That is why superseding loses no
  coverage: each run covers everything since the last run that really ran.

While those are decided, an older push run on main that is still executing is
waited for (`WAIT_SECONDS`), so main runs one window of commits at a time
instead of a pile that never finishes. The wait is bounded: past the deadline
this run starts anyway, because a late run is still better than none.

The API is the Forgejo one (`github.api_url`), read with the job's token. A
failure never fails this run: the outputs stay empty, and check.yml's select job
then plans from the push's own `before` commit, which is narrower but correct.
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

# The job of this workflow that plans a run. A run whose `select` job is running
# or finished has a window of commits; one whose `select` job was skipped (it
# was superseded) or failed (the plan itself broke) has none.
SELECT_JOB = "select"
PLANNED = ("running", "success")
# A run that has not finished, and has not given its window up, so it holds
# runner time now or is waiting to.
UNFINISHED = ("waiting", "running", "blocked")
GAVE_UP = ("skipped", "cancelled")
# How many runs to inspect: the queue is about a dozen deep, and every run past
# the ones that planned costs a request.
WINDOW = 12


def select_status(jobs):
    """The status of a run's planning job, or None when the list has no such job."""
    for job in jobs:
        if job.get("name") == SELECT_JOB:
            return job.get("status")
    return None


def decide(runs, jobs_of, run_number):
    """(superseding, base, executing) for the run numbered `run_number`.

    `runs` is the API's list of push runs of this workflow on main, and `jobs_of`
    returns one run's jobs. `superseding` is the newer run that covers this one
    (0 when there is none), `base` is where this run's plan starts (empty when no
    earlier run planned), and `executing` lists the earlier runs still holding a
    runner.
    """
    older = sorted((run for run in runs if run["index_in_repo"] < run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)
    newer = sorted((run for run in runs if run["index_in_repo"] > run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)

    # The newest run cannot be superseded, so a chain of skipped runs always ends
    # at one that runs: the first newer run that did not give up covers this one.
    superseding = 0
    for run in newer[:WINDOW]:
        if select_status(jobs_of(run)) in GAVE_UP:
            continue
        superseding = run["index_in_repo"]
        break

    base, executing = "", []
    for run in older[:WINDOW]:
        status = select_status(jobs_of(run))
        if status in GAVE_UP or run.get("status") == "cancelled":
            # Nothing was planned here, or it was stopped part-way through its
            # window: either way its commits are this run's business.
            continue
        if run.get("status") in UNFINISHED:
            executing.append(run["index_in_repo"])
        if not base and status in PLANNED:
            base = run.get("commit_sha") or ""
    return superseding, base, executing


def api_reader(api_url, repo, token):
    """A GET of this repository's API, authorised with the job's token.

    Forgejo serves a public repository's runs without a token, and a token this
    runner cannot use (another scope, an expired one) must not fail the run, so
    an unauthorised answer is retried anonymously.
    """
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"token {token}"

    def get(path, authorised=True):
        request_headers = dict(headers)
        if not authorised:
            request_headers.pop("Authorization", None)
        request = urllib.request.Request(f"{api_url}/repos/{repo}{path}",
                                         headers=request_headers)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if authorised and error.code in (401, 403):
                return get(path, authorised=False)
            raise

    return get


def read_window(get):
    """The newest push runs of this workflow on main."""
    page = get("/actions/runs?event=push&ref=refs/heads/main&workflow_id=check.yml"
               f"&limit={WINDOW + 5}")
    runs = [run for run in page.get("workflow_runs") or []
            if run.get("index_in_repo") is not None and run.get("commit_sha")]
    # The list is newest first; sort anyway so the decisions cannot depend on it.
    return sorted(runs, key=lambda run: run["index_in_repo"], reverse=True)


def notice(message, summary=True):
    """Say what was decided: in the log, and in the run's summary."""
    print(message, flush=True)
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(f"{message}\n")


def write_output(name, value):
    path = os.environ.get("GITHUB_OUTPUT")
    if not path:
        print(f"{name}={value}")
        return
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(f"{name}={value}\n")


def finish(superseded=False, base=""):
    write_output("superseded", "true" if superseded else "false")
    write_output("base", base)
    return 0


def runs_list(executing):
    return ", ".join(f"#{number}" for number in executing)


def main():
    if os.environ.get("EVENT_NAME") != "push":
        # Only pushes to main supersede each other; a dispatch validates the
        # complete workspace and is nobody's duplicate.
        notice("not a push: no superseding, and no plan base from earlier runs")
        return finish()

    try:
        run_number = int(os.environ["RUN_NUMBER"])
    except (KeyError, ValueError):
        notice("::warning::no run number for this run; planning from the push's own "
               "before commit", summary=False)
        notice("no run number: the plan falls back to the push base")
        return finish()
    get = api_reader(os.environ["API_URL"].rstrip("/"), os.environ["REPOSITORY"],
                     os.environ.get("CI_TOKEN", ""))
    poll_seconds = float(os.environ.get("POLL_SECONDS", "30"))
    wait_seconds = float(os.environ.get("WAIT_SECONDS", "5400"))
    deadline = time.monotonic() + wait_seconds

    while True:
        jobs = {}

        def jobs_of(run):
            if run["id"] not in jobs:
                jobs[run["id"]] = get(f"/actions/runs/{run['id']}/jobs")
            return jobs[run["id"]]

        try:
            runs = read_window(get)
            superseding, base, executing = decide(runs, jobs_of, run_number)
        except Exception as error:  # a lookup failure never fails the run
            notice(f"::warning::cannot read earlier runs ({error}); planning from "
                   "the push's own before commit", summary=False)
            notice("earlier runs unavailable: the plan falls back to the push base")
            return finish()

        if superseding:
            notice(f"run #{superseding} is newer and covers these commits: skipping "
                   "this run's jobs, so nothing here is left untested")
            return finish(superseded=True)
        if not executing:
            notice(f"nothing else on main is executing; this run's window starts at "
                   f"{base or 'the push base'}")
            return finish(base=base)
        if time.monotonic() >= deadline:
            notice(f"::warning::waited {int(wait_seconds)}s for run {runs_list(executing)}; "
                   "starting beside it rather than dropping this window", summary=False)
            notice(f"waited {int(wait_seconds)}s for {runs_list(executing)}: "
                   "starting beside it rather than dropping this window")
            return finish(base=base)
        notice(f"waiting for {runs_list(executing)} to finish before planning this "
               "window", summary=False)
        time.sleep(poll_seconds)


if __name__ == "__main__":
    sys.exit(main())
