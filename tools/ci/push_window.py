#!/usr/bin/env python3
"""Main's push window: which push run is the window, and which runs it replaces.

Main takes a push every couple of minutes from many agents, and a full run needs
five large runner slots for about an hour, so runs that only ever queue never
drain: every waiting run asks for the same slots and the newest head sits behind
the oldest. Forgejo's own `concurrency:` cannot express what this needs (see the
`supersede` job in check.yml): with `cancel-in-progress: false` it neither
replaces nor drops a waiting run, and with `cancel-in-progress: true` it would
cancel runs that are already testing.

So before anything expensive starts, a push run asks the Forgejo API about the
other push runs of this workflow on main, and answers with two outputs:

* `superseded` -- a newer push run on main is going to run. This run's commits
  are that one's business, so this run skips its jobs: a clean success, since
  nothing was tested here and nothing was lost.
* `base` -- where this run's plan starts: the head of the newest earlier push
  run whose window was measured (it finished, successfully or not, and its
  `select` job planned it). A run that skipped, was cancelled, or broke while
  planning measured nothing and is not a base. A window's plan always starts
  below every run that was dropped before it, so the chain of measured windows
  leaves no commit unmeasured: a run that finds nothing to drop plans from the
  newest window that ran, and a run that drops older runs plans from below them.

Being the newest is not enough to be the window; it also has to stop the runs
that will never get a slot. An older run that has not started a single job is
cancelled on the spot -- its commits are inside this run's plan, because the
base moves below the oldest run that is being dropped. An older run that has
started is never cancelled: it may be mid-window, so this run waits for it
(`WAIT_SECONDS`, bounded) and starts beside it when the deadline passes, because
a late window is still better than none. Two windows at once is the most this
can cost.

When the API has no answer, the outputs stay empty and check.yml's select job
plans from the push's own `before` commit, which is narrower but correct.

`--sweep` is the safety net (main-window.yml, every fifteen minutes): it cancels
the push runs on main that never started, so a supersede job that died cannot
let the pile grow back.

`--dry-run` says what would be cancelled and where the plan would start, without
touching a run: the same report the job logs, for reading by hand.
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

# The job of this workflow that plans a run. A run whose `select` job succeeded
# has a window of commits this workflow measured.
SELECT_JOB = "select"
# A job in one of these states has not begun; a run holding only these has not
# started and costs nothing to drop.
UNSTARTED_JOB = ("waiting", "blocked")
# A run in one of these states has finished, whatever it found.
FINISHED_RUN = ("success", "failure")
# A run in one of these states was stopped or never ran.
GAVE_UP_RUN = ("cancelled", "skipped")
# `--dry-run`: report the decision without cancelling anything.
DRY_RUN = "--dry-run" in sys.argv[1:]
# How many runs to inspect backwards and forwards: the queue is about a dozen
# deep, and every run past the ones that matter costs a request.
WINDOW = 20


def select_status(jobs):
    """The status of a run's planning job, or None when the list has no such job."""
    for job in jobs:
        if job.get("name") == SELECT_JOB:
            return job.get("status")
    return None


def started(jobs):
    """Whether any job of the run has left the queue."""
    return any(job.get("status") not in UNSTARTED_JOB for job in jobs)


def measured(run, jobs):
    """Whether this run finished and its window of commits was planned."""
    return run.get("status") in FINISHED_RUN and select_status(jobs) == "success"


def coming(run, jobs):
    """Whether this run still owes main a window: it is executing right now.

    A run that has started something but has not finished is somebody's window:
    its `select` job may be queued behind a large slot or already running, and
    its commits arrive with it. Such a run is waited for, never cancelled.
    """
    if measured(run, jobs) or run.get("status") in FINISHED_RUN + GAVE_UP_RUN:
        return False
    if select_status(jobs) == "skipped":
        # Superseded: it planned nothing and gives up its commits to a newer run.
        return False
    return run.get("status") == "running" or started(jobs)


def cancellable(run, jobs):
    """Whether dropping this run loses nothing but its queue position."""
    if run.get("status") in FINISHED_RUN + GAVE_UP_RUN:
        return False
    return not started(jobs)


def decide(runs, jobs_of, run_number):
    """(superseding, base, executing, cancelling) for the run numbered `run_number`.

    `runs` is the API's list of push runs of this workflow on main, `jobs_of`
    returns one run's jobs, and every value returned is a run number: the newer
    run that covers this one (0 when there is none), the commit this run's plan
    starts from (empty when no earlier run is a base), the older runs that are
    executing and so are waited for, and the older runs this run cancels.
    """
    older = sorted((run for run in runs if run["index_in_repo"] < run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)
    newer = sorted((run for run in runs if run["index_in_repo"] > run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)

    # The newest run cannot be superseded, so a chain of skipped runs always
    # ends at one that runs: the first newer run that did not give up covers
    # this one.
    superseding = 0
    for run in newer[:WINDOW]:
        if run.get("status") in GAVE_UP_RUN or select_status(jobs_of(run)) == "skipped":
            continue
        superseding = run["index_in_repo"]
        break

    executing, cancelling, barrier = [], [], 0
    for run in older[:WINDOW]:
        jobs = jobs_of(run)
        if coming(run, jobs):
            executing.append(run["index_in_repo"])
            continue
        if measured(run, jobs) or not cancellable(run, jobs):
            # Measured, or long since stopped: either way it is not this run's
            # to drop. A run that stopped before a window that came after it was
            # inside that window's plan, which is the chain this rule keeps.
            continue
        # Never started: it may go, and then the plan has to start below it.
        cancelling.append(run["index_in_repo"])
        barrier = run["index_in_repo"]

    base = ""
    for run in older[:WINDOW]:
        if barrier and run["index_in_repo"] >= barrier:
            continue
        if measured(run, jobs_of(run)):
            base = run.get("commit_sha") or ""
            break
    return superseding, base, executing, cancelling


class Api:
    """This repository's Forgejo API, authorised with the job's token.

    Forgejo serves a public repository's runs without a token, and a token this
    runner cannot use (another scope, an expired one) must not fail the run, so
    an unauthorised read is retried anonymously.
    """

    def __init__(self, api_url, repo, token):
        self.base = f"{api_url.rstrip('/')}/repos/{repo}"
        self.token = token

    def get(self, path):
        try:
            return self._send("GET", path, self.token)
        except urllib.error.HTTPError as error:
            if error.code not in (401, 403) or not self.token:
                raise
            return self._send("GET", path, "")

    def cancel(self, run_id, token=None):
        """Stop a run; the API answers with no body."""
        self._send("POST", f"/actions/runs/{run_id}/cancel", token or self.token)

    def _send(self, method, path, token):
        headers = {"Accept": "application/json"}
        if token:
            headers["Authorization"] = f"token {token}"
        request = urllib.request.Request(f"{self.base}{path}", headers=headers,
                                         method=method)
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read()
        return json.loads(body) if body.strip() else {}


def read_window(api):
    """The newest push runs of this workflow on main, newest first."""
    page = api.get("/actions/runs?event=push&ref=refs/heads/main&workflow_id=check.yml"
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


def runs_list(numbers):
    return ", ".join(f"#{number}" for number in numbers)


def cancel_runs(api, runs, numbers, spare_token=""):
    """Cancel the numbered runs; a refusal is reported and never fails the job."""
    by_number = {run["index_in_repo"]: run for run in runs}
    cancelled = []
    for number in numbers:
        run = by_number[number]
        if DRY_RUN:
            notice(f"would cancel run #{number}", summary=False)
            cancelled.append(number)
            continue
        token = api.token
        try:
            api.cancel(run["id"], token)
        except urllib.error.HTTPError as error:
            if error.code not in (401, 403) or not spare_token:
                notice(f"::warning::cannot cancel run #{number} ({error}); "
                       "it keeps its queue position", summary=False)
                notice(f"cannot cancel run #{number} ({error})")
                continue
            token = spare_token
            try:
                api.cancel(run["id"], token)
            except urllib.error.HTTPError as retry:
                notice(f"::warning::cannot cancel run #{number} ({retry})", summary=False)
                notice(f"cannot cancel run #{number} ({retry})")
                continue
        except OSError as error:
            notice(f"::warning::cannot cancel run #{number} ({error})", summary=False)
            notice(f"cannot cancel run #{number} ({error})")
            continue
        cancelled.append(number)
    return cancelled


def api_from_environment():
    try:
        return Api(os.environ["API_URL"], os.environ["REPOSITORY"],
                   os.environ.get("CI_TOKEN", ""))
    except KeyError as error:
        notice(f"::warning::no API endpoint for this run ({error}); planning from "
               "the push's own before commit", summary=False)
        return None


def sweep():
    """Cancel the push runs on main that never started (the safety net)."""
    api = api_from_environment()
    if api is None:
        return 1
    try:
        runs = read_window(api)
    except (urllib.error.HTTPError, OSError) as error:
        notice(f"::warning::cannot read this workflow's runs ({error})", summary=False)
        return 1
    if not runs:
        notice("no push runs on main to sweep")
        return 0
    newest = runs[0]["index_in_repo"]
    stale = []
    for run in runs:
        if run["index_in_repo"] >= newest:
            continue
        if cancellable(run, api.get(f"/actions/runs/{run['id']}/jobs")):
            stale.append(run["index_in_repo"])
    if not stale:
        notice(f"newest push run on main is #{newest}; nothing older is waiting to start")
        return 0
    cancelled = cancel_runs(api, runs, stale, os.environ.get("CI_SPARE_TOKEN", ""))
    notice(f"newest push run on main is #{newest}; cancelled {len(cancelled)} run(s) "
           f"that never started: {runs_list(cancelled) or 'none'}")
    return 0


def main():
    if "--sweep" in sys.argv[1:]:
        return sweep()
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
    api = api_from_environment()
    if api is None:
        return finish()
    spare_token = os.environ.get("CI_SPARE_TOKEN", "")
    poll_seconds = float(os.environ.get("POLL_SECONDS", "30"))
    wait_seconds = float(os.environ.get("WAIT_SECONDS", "2700"))
    deadline = time.monotonic() + wait_seconds

    jobs = {}

    def jobs_of(run):
        if run["id"] not in jobs:
            jobs[run["id"]] = api.get(f"/actions/runs/{run['id']}/jobs")
        return jobs[run["id"]]

    while True:
        try:
            runs = read_window(api)
            # A finished run's jobs never change again, so only the runs that
            # are still going are asked about twice.
            for run in runs:
                if run.get("status") not in FINISHED_RUN + GAVE_UP_RUN:
                    jobs.pop(run["id"], None)
            superseding, base, executing, cancelling = decide(runs, jobs_of, run_number)
        except (urllib.error.HTTPError, OSError) as error:  # a lookup never fails the run
            notice(f"::warning::cannot read this workflow's runs ({error}); planning "
                   "from the push's own before commit", summary=False)
            notice("earlier runs unavailable: the plan falls back to the push base")
            return finish()

        if superseding:
            notice(f"run #{superseding} is newer and covers these commits: skipping "
                   "this run's jobs, so nothing here is left untested")
            return finish(superseded=True)
        if cancelling:
            cancelled = cancel_runs(api, runs, cancelling, spare_token)
            if cancelled:
                notice(f"this run is the window: cancelled {len(cancelled)} older run(s) "
                       f"that never started ({runs_list(cancelled)})")
        if not executing:
            notice(f"nothing else on main is executing; this run's window starts at "
                   f"{base or 'the push base'}")
            return finish(base=base)
        if time.monotonic() >= deadline:
            notice(f"::warning::waited {int(wait_seconds)}s for {runs_list(executing)}; "
                   "starting beside it rather than dropping this window", summary=False)
            notice(f"waited {int(wait_seconds)}s for {runs_list(executing)}: starting "
                   f"beside it rather than dropping this window; beginning at "
                   f"{base or 'the push base'}")
            return finish(base=base)
        notice(f"waiting for {runs_list(executing)} to finish before planning this "
               "window", summary=False)
        time.sleep(poll_seconds)


if __name__ == "__main__":
    sys.exit(main())
