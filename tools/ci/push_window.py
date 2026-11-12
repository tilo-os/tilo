#!/usr/bin/env python3
"""Main's push window: which push run is the window, and which runs it replaces.

Main takes a push every couple of minutes from many agents, and a full run needs
five large runner slots for about an hour, so runs that only ever queue never
drain: every waiting run asks for the same slots and the newest head sits behind
the oldest. Forgejo's own `concurrency:` cannot express what this needs (see the
`supersede` job in check.yml): with `cancel-in-progress: false` it neither
replaces nor drops a waiting run, and with `cancel-in-progress: true` it would
cancel runs that are already testing.

So before anything expensive starts, a push run asks the forge's API about the
other push runs of its workflow on main, and answers with two outputs:

* `superseded` -- main's window is somebody else's. Either a newer push run is
  running a window that covers these commits, or an older run holds the window:
  its plan and this one's are the same next window, which starts above both. In
  both cases this run skips its jobs, and its `untested` job ends it as
  cancelled (`--close`): never green, since nothing was tested here, and
  `window` names the run that measures these commits.
* `base` -- where this run's plan starts: the head of the newest earlier push
  run whose window was measured (it finished, successfully or not, its decision
  and plan jobs ran, and its builder job gave the checks their image). A run
  that skipped, was cancelled, broke while planning, or never got a builder
  checked nothing and is not a base. A window's plan always starts below every
  run that was dropped before it, so the chain of measured windows leaves no
  commit unmeasured.

The decision never waits. It reads the queue once and answers in seconds, and
the window it hands out is a run that decided to run and has not finished yet,
which every later run sees in the API as a run whose plan job was not skipped.
Waiting was how the window used to be handed over, and under a constant push
rate it broke the model: a run that waited for the running window to finish was
superseded by every push behind it, and each of those skipped in turn for the
next one, so no run ever claimed the window and the window ran only when pushes
happened to pause. Now a window holds until it is done, every run behind it
skips at once, and the first push after it claims the next window: one window
per run duration, at any push rate.

An older run that has started but not decided yet may be claiming the window, so
a later run skips behind it rather than open a second window beside it. It
resolves in seconds, and when it gives up the next push claims; nothing waits on
it, and a run whose decision never ran at all is not started, so the run that
reads it cancels it. A window that is still unfinished after WINDOW_LIMIT is
broken rather than slow -- every job of a window has its own timeout -- so it
stops blocking the runs behind it and one stuck window costs one window of
latency instead of main's window forever.

Planning needs the run that measured the previous window, which under a stream of
pushes is many runs back, so the read follows the API's pages back as far as a
window can last, and the job lists are fetched only for the runs the decision
still has a question about.

When the API has no answer, the outputs stay empty and check.yml's select job
plans from the push's own `before` commit, which is narrower but correct.

`--sweep` is the safety net the CI host controller calls: it cancels the push
runs on main that never started, so a decision job that died cannot let the pile
grow back.

`--close` ends a superseded run once the jobs it runs for its own head
(`HEAD_RESULTS`, such as the page build) have their results: it cancels the
run, so a head whose checks ran elsewhere reads as cancelled rather than as a
success it never earned. A failed head job keeps the run red instead, and a
refused cancellation fails the job, so the run is never green either way.

`--dry-run` says what would be cancelled and where the plan would start, without
touching a run: the same report the job logs, for reading by hand.
"""
from __future__ import annotations

import datetime
import json
import os
import sys
import time
import urllib.error
import urllib.request

# The job of the workflow that decides a run's window.
DECISION_JOB = os.environ.get("DECISION_JOB", "supersede")
# The first job a run starts only when it decided to run: `skipped` on a run
# that gave its commits away, anything else on the run that holds the window.
MARKER_JOB = os.environ.get("MARKER_JOB", "select")
# The workflow whose push runs supersede each other.
WORKFLOW = os.environ.get("WORKFLOW", "check.yml")
# The job that resolves and starts the image every other job runs in.
BUILDER_JOB = "builder"
# A job in one of these states has not begun; a run holding only these has not
# started and costs nothing to drop.
UNSTARTED_JOB = ("waiting", "blocked")
# A run in one of these states has finished, whatever it found.
FINISHED_RUN = ("success", "failure")
# A run in one of these states was stopped or never ran. A run whose planning
# job failed is `blocked`: its remaining jobs wait behind that failure forever,
# so it is not a window somebody else has to wait for.
GAVE_UP_RUN = ("cancelled", "skipped", "blocked")
# `--dry-run`: report the decision without cancelling anything.
DRY_RUN = "--dry-run" in sys.argv[1:]
# The longest a window can legitimately hold: its jobs' own timeouts (120
# minutes each) and the queue in front of them. A run still unfinished after
# that is not making progress, so it stops standing in the way of the next one.
WINDOW_LIMIT_SECONDS = 3 * 60 * 60
# How the runs are read: the API lists them newest first, a page at a time. The
# read goes back until it has looked a whole WINDOW_LIMIT_SECONDS back, which is
# as far as the run that measured the previous window can be, and `PAGES` keeps
# one decision's requests bounded when pushes arrive faster than that.
PAGE_SIZE = 50
PAGES = 24


def job_status(jobs, name):
    """The status of a run's job called `name`, or None when it has no such job."""
    for job in jobs:
        if job.get("name") == name:
            return job.get("status")
    return None


def started(jobs):
    """Whether any job of the run has left the queue."""
    return any(job.get("status") not in UNSTARTED_JOB for job in jobs)


def gave_up(run, jobs, marker):
    """Whether this run is over, or never got as far as a window."""
    return (run.get("status") in FINISHED_RUN + GAVE_UP_RUN
            or job_status(jobs, marker) == "skipped")


def timestamp(value):
    """An API timestamp as a POSIX timestamp, or None when it says nothing."""
    try:
        return datetime.datetime.fromisoformat(
            value.replace("Z", "+00:00")).timestamp()
    except (AttributeError, ValueError):
        return None


def running_since(run):
    """When this run started, as a POSIX timestamp, or None when it says nothing."""
    return timestamp(run.get("started") or run.get("created"))


def stale(run, now):
    """Whether this run has been going longer than any window can."""
    since = running_since(run)
    return since is not None and now - since > WINDOW_LIMIT_SECONDS


def holds(run, jobs, marker, now):
    """Whether this run is main's window: it decided to run and has not finished.

    A run that gave its commits away decided too, and its plan job is `skipped`;
    that is what tells the two apart. It stops holding once it is stale, so a
    window that never finishes cannot leave main without another.
    """
    return (not gave_up(run, jobs, marker)
            and job_status(jobs, DECISION_JOB) == "success"
            and not stale(run, now))


def measured(run, jobs, marker):
    """Whether this run finished and acted on the window it planned.

    Planning is only half of it: every job that checks a window - the test
    shards, Clippy, the architecture checks - runs in the builder's image, so a
    run whose builder job failed skipped all of them. Its commits were not
    checked, and the next window has to plan them again.
    """
    return (run.get("status") in FINISHED_RUN
            and job_status(jobs, DECISION_JOB) == "success"
            and job_status(jobs, marker) == "success"
            and job_status(jobs, BUILDER_JOB) in (None, "success"))


def cancellable(run, jobs, marker):
    """Whether dropping this run loses nothing but its queue position."""
    return not gave_up(run, jobs, marker) and not started(jobs)


def decide(runs, jobs_of, run_number, marker=None, now=None):
    """(superseding, base, holding, cancelling) for the run numbered `run_number`.

    `runs` is the API's list of push runs of this workflow on main, `jobs_of`
    returns one run's jobs, `marker` names the first job only a run that holds
    the window starts, and every value returned is a run number: the newer run
    whose window covers this one (0 when there is none), the commit this run's
    plan starts from (empty when no earlier run is a base), the older run that
    holds the window right now (0 when there is none), and the older runs this
    run cancels.
    """
    marker = MARKER_JOB if marker is None else marker
    now = time.time() if now is None else now
    older = sorted((run for run in runs if run["index_in_repo"] < run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)
    newer = sorted((run for run in runs if run["index_in_repo"] > run_number),
                   key=lambda run: run["index_in_repo"], reverse=True)

    # Only a newer run that already holds the window covers this one. A newer run
    # that is still deciding might give its commits away in turn, and every run
    # in a stream is newer than the one it arrived behind, so skipping for those
    # is how no window gets claimed at all. Finished runs cannot hold it, and
    # checking them would cost a job lookup each.
    superseding = 0
    for run in newer:
        if run.get("status") in FINISHED_RUN + GAVE_UP_RUN:
            continue
        if holds(run, jobs_of(run), marker, now):
            superseding = run["index_in_repo"]
            break
    if superseding:
        return superseding, "", 0, []

    holding, cancelling, barrier = 0, [], 0
    for run in older:
        if run.get("status") in FINISHED_RUN + GAVE_UP_RUN:
            # Measured, or long since stopped: it is not this run's to drop. A
            # run that stopped before a window that came after it was inside
            # that window's plan, which is the chain this rule keeps.
            continue
        jobs = jobs_of(run)
        if gave_up(run, jobs, marker):
            # Unfinished, but already past its decision: it gave its commits
            # away and is only waiting to be closed.
            continue
        if not started(jobs):
            # Never started: it may go, and then the plan has to start below it.
            cancelling.append(run["index_in_repo"])
            barrier = run["index_in_repo"]
            continue
        if not holding and not stale(run, now):
            # Started and unfinished: a window in progress, or one about to be
            # claimed. Either way this run's commits belong to the window above.
            holding = run["index_in_repo"]

    # Only a finished run can have measured a window, so the scan back to the
    # previous one costs a job lookup per finished run it passes.
    base = ""
    for run in older:
        if barrier and run["index_in_repo"] >= barrier:
            continue
        if run.get("status") in FINISHED_RUN and measured(run, jobs_of(run), marker):
            base = run.get("commit_sha") or ""
            break
    return superseding, base, holding, cancelling


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
    """This workflow's push runs on main, newest first.

    Enough of them to hold both the run that holds main's window and the run
    that measured the previous one: a window lasts at most WINDOW_LIMIT_SECONDS
    and the next one is claimed right after it, so the previous window's run was
    created within that span. Reading only the newest page was how a plan lost
    its base: in a stream of pushes the measuring run had fallen out of the
    window, the new run planned from its own push alone, and one window of
    commits went unmeasured.
    """
    cutoff = time.time() - WINDOW_LIMIT_SECONDS
    runs = []
    for page in range(1, PAGES + 1):
        answer = api.get(f"/actions/runs?event=push&ref=refs/heads/main"
                         f"&workflow_id={WORKFLOW}&limit={PAGE_SIZE}&page={page}")
        listed = answer.get("workflow_runs") or []
        runs.extend(run for run in listed
                    if run.get("index_in_repo") is not None
                    and run.get("commit_sha"))
        if len(listed) < PAGE_SIZE:
            break
        if all((timestamp(run.get("created")) or float("inf")) < cutoff
               for run in listed):
            break
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


def finish(superseded=False, base="", window=0):
    write_output("superseded", "true" if superseded else "false")
    write_output("base", base)
    write_output("window", str(window) if window else "")
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
    stale_runs = []
    for run in runs:
        if run["index_in_repo"] >= newest:
            continue
        if run.get("status") in FINISHED_RUN + GAVE_UP_RUN:
            continue
        if cancellable(run, api.get(f"/actions/runs/{run['id']}/jobs"), MARKER_JOB):
            stale_runs.append(run["index_in_repo"])
    if not stale_runs:
        notice(f"newest push run on main is #{newest}; nothing older is waiting to start")
        return 0
    cancelled = cancel_runs(api, runs, stale_runs, os.environ.get("CI_SPARE_TOKEN", ""))
    notice(f"newest push run on main is #{newest}; cancelled {len(cancelled)} run(s) "
           f"that never started: {runs_list(cancelled) or 'none'}")
    return 0


def close():
    """End a superseded run: cancelled, or red when a head job failed; never green."""
    window = os.environ.get("WINDOW_RUN") or "a later run"
    if window.isdigit():
        window = f"run #{window}"
    failed = [result for result in os.environ.get("HEAD_RESULTS", "").split()
              if result not in ("success", "skipped")]
    if failed:
        notice(f"::error::a job this run ran for its own head ended {failed[0]}; "
               f"the rest of its checks run in {window}", summary=False)
        notice(f"not tested in this run: {window} measures these commits, and a job "
               "for this head failed")
        return 1
    notice(f"not tested in this run: {window} measures these commits; this run "
           "ends cancelled")
    try:
        run_number = int(os.environ["RUN_NUMBER"])
    except (KeyError, ValueError):
        notice("::error::no run number for this run; it cannot cancel itself",
               summary=False)
        return 1
    api = api_from_environment()
    if api is None:
        return 1
    try:
        runs = [run for run in read_window(api) if run["index_in_repo"] == run_number]
    except (urllib.error.HTTPError, OSError) as error:
        notice(f"::error::cannot read this workflow's runs ({error})", summary=False)
        return 1
    if not runs or not cancel_runs(api, runs, [run_number],
                                   os.environ.get("CI_SPARE_TOKEN", "")):
        notice("::error::this run cannot cancel itself; it fails instead, since it "
               "tested nothing", summary=False)
        return 1
    # The cancellation already ended this job; a failure is what remains if the
    # forge still took this job's result.
    return 1


def main():
    if "--sweep" in sys.argv[1:]:
        return sweep()
    if "--close" in sys.argv[1:]:
        return close()
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

    jobs = {}

    def jobs_of(run):
        if run["id"] not in jobs:
            jobs[run["id"]] = api.get(f"/actions/runs/{run['id']}/jobs")
        return jobs[run["id"]]

    try:
        runs = read_window(api)
        superseding, base, holding, cancelling = decide(runs, jobs_of, run_number)
    except (urllib.error.HTTPError, OSError) as error:  # a lookup never fails the run
        notice(f"::warning::cannot read this workflow's runs ({error}); planning "
               "from the push's own before commit", summary=False)
        notice("earlier runs unavailable: the plan falls back to the push base")
        return finish()

    if superseding:
        notice(f"run #{superseding} is newer and runs main's window: skipping this "
               "run's jobs, so these commits are measured there")
        return finish(superseded=True, window=superseding)
    if holding:
        notice(f"run #{holding} holds main's window: skipping this run's jobs, whose "
               "commits the window above it plans with its own")
        return finish(superseded=True, window=holding)
    cancelled = cancel_runs(api, runs, cancelling,
                            os.environ.get("CI_SPARE_TOKEN", "")) if cancelling else []
    if cancelled:
        notice(f"this run is main's window: cancelled {len(cancelled)} older run(s) "
               f"that never started ({runs_list(cancelled)})")
    notice(f"this run is main's window; its plan starts at "
           f"{base or 'the push base'}")
    return finish(base=base)


if __name__ == "__main__":
    sys.exit(main())
