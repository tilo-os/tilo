#!/usr/bin/env python3
"""Chat, Terminal, Settings and Notes show their content soon after they are
opened.

An app's open is one chain: the desk asks for it, the session spawns the
launcher, the launcher starts the app in its sandbox, the app commits its
first window buffer, and the compositor's first frame showing that buffer
reaches a display. A person sees nothing until the last step. This proof
drives a headless session of the tree's own apps and reads each step back off
the session's own launch-latency log, which is the only account of the whole
chain (the app's clock starts inside it, the compositor's ends on screen):

    application launch requested        package_id=<app>
    application launcher spawned        request_to_spawn_us=<us>
    application toplevel mapped         spawn_to_map_us=<us>
    application first frame presented   request_to_first_present_us=<us>

The budget is what a person can wait without noticing: well under a second
for every app, quiet or on a loaded machine. The open it was written for
measured 0.22-0.37 s per app on a quiet machine and 0.62-1.39 s under hard
load, so the budget leaves room for a loaded CI runner and still catches a
return to the 1.5 s an audit of this desktop reported (most of that wait was
the audit's own polling, not the app).

The first open of a session also waits for what the session readies for its
first app — the device's font index on a fresh profile, and on a machine with
no GPU the render service, which the session starts only when an app first
asks for it — so it is reported apart from an open beside apps already
running: an app that is always opened first carries that wait as its own.
`--rounds N` starts N sessions and turns the order each time, so each app is
the first open in some rounds and opened beside others in the rest.

Usage:
  ./first-frame.py                     # builds nothing; uses TILO_NESTED_SESSION_BUILD
  ./first-frame.py --build             # builds the session and its apps first
  ./first-frame.py --rounds 8          # every app opened first twice, beside others six times

A nested-session build is needed (`dist/dev/nested-session-build.sh`, or
`TILO_NESTED_SESSION_BUILD_EXEC` where this machine cannot build it).
"""
import os
import shutil
import subprocess
import sys
import time
import pathlib

REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts" / "headless-examples"))

import real_apps  # noqa: E402

# The apps a person opens all day, and the budget their content may take from
# the ask to the frame that shows it.
APPS = ["chat", "terminal", "settings", "notes"]
BUDGET_MS = 1200
DEADLINE = 60.0

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def real_apps_lines(log: str) -> list[dict]:
    """Every `tilo::launch_latency` line of a session log, with its fields."""
    import re
    from datetime import datetime, timezone

    pattern = re.compile(
        r"^\S+Z\s+\w+\s+(?P<target>[A-Za-z0-9_:]+):\s*(?P<message>.*)$")
    fields = re.compile(r"(\w+)=(\S+)")
    lines = []
    for raw in log.splitlines():
        match = pattern.match(raw.strip())
        if not match or match.group("target") != "tilo::launch_latency":
            continue
        message = match.group("message")
        # `application launch requested` and `application first frame
        # presented` say which step they are; the rest say it in the value.
        stage = None
        for name, word in (("requested", "application launch requested"),
                           ("spawned", "application launcher spawned"),
                           ("mapped", "application toplevel mapped"),
                           ("presented", "application first frame presented")):
            if message.startswith(word):
                stage = name
        if stage is None:
            continue
        line = dict(fields.findall(message))
        line["stage"] = stage
        line["message"] = message
        line["epoch"] = datetime.fromisoformat(
            raw.strip().split()[0].replace("Z", "+00:00")
        ).astimezone(timezone.utc).timestamp()
        lines.append(line)
    return lines


def opens(session, app: str) -> dict:
    """Open `app` and answer the launch-latency lines of that open."""
    app_id = real_apps.app_id(app)
    asked = time.time()
    session.control({"op": "actions_invoke", "provider": "applications", "id": app_id})
    end = time.time() + DEADLINE
    while time.time() < end:
        found = [line for line in real_apps_lines(session.log()) if line["epoch"] >= asked - 0.05]
        if any(line["stage"] == "presented" and line["package_id"] == app_id for line in found):
            return found
        time.sleep(0.1)
    return [line for line in real_apps_lines(session.log())
            if line["epoch"] >= asked - 0.05 and line["package_id"] == app_id]


def one_open(session, app: str, first: bool) -> float | None:
    """Open `app` in `session`, check it against the budget and answer its time in ms."""
    lines = opens(session, app)
    by_stage = {line["stage"]: line for line in lines
                if line["package_id"] == real_apps.app_id(app)}
    where = "the session's first open" if first else "beside others"
    label = f"{app} ({where})"
    if "presented" not in by_stage:
        check(f"{label} shows its content", False,
              f"no first presented frame within {DEADLINE:.0f} s "
              f"(stages seen: {sorted(by_stage)})")
        return None
    presented = by_stage["presented"]
    request_to_present = int(presented["request_to_first_present_us"]) / 1000.0
    stages = []
    if "spawned" in by_stage:
        stages.append(f"spawn {int(by_stage['spawned']['request_to_spawn_us']) / 1000.0:.0f}")
    if "mapped" in by_stage:
        stages.append(f"map {int(by_stage['mapped']['spawn_to_map_us']) / 1000.0:.0f}")
    stages.append(f"present {int(presented['map_to_first_present_us']) / 1000.0:.0f}")
    detail = f"{request_to_present:.0f} ms ({', '.join(stages)})"
    check(f"{label} shows its content within {BUDGET_MS} ms", request_to_present < BUDGET_MS,
          detail)
    return request_to_present


def main() -> int:
    built = "--build" in sys.argv[1:]
    rounds = int(sys.argv[sys.argv.index("--rounds") + 1]) if "--rounds" in sys.argv else 1
    if not (real_apps.BUILD / "bin/tilo-session").is_file() or built:
        real_apps.build(remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))
    prepared = pathlib.Path(os.environ.get("TILO_FIRST_FRAME_STATE")
                            or pathlib.Path.home() / ".cache" / "tilo-first-frame")
    real_apps.prepare(prepared, apps=" ".join(APPS))
    taken: dict[tuple[str, bool], list[float]] = {}
    for round_ in range(rounds):
        # Each round opens the apps in a turned order, so every app is in turn the session's
        # first open, which also waits for what the session readies for its first app.
        order = APPS[round_ % len(APPS):] + APPS[:round_ % len(APPS)]
        # A state that has run before brings back the desk it left, apps and all: this measures
        # the open of an app that is not open yet, so each round gets its own copy of the
        # prepared state and leaves it behind.
        state = prepared.with_name(prepared.name + "-run")
        shutil.rmtree(state, ignore_errors=True)
        subprocess.run(["cp", "-a", "--reflink=auto", str(prepared), str(state)], check=True)
        session = real_apps.Session(state)
        session.start(apps=" ".join(APPS), fresh=True, out=prepared / "logs",
                      tag="first-frame", env={"TILO_APP_STDERR_DIR": str(state / "app-diag")})
        try:
            open_at_start = [row.get("app_id") for row in session.windows()]
            check("the desk starts with no window open", not open_at_start, str(open_at_start))
            for index, app in enumerate(order):
                took = one_open(session, app, index == 0)
                if took is not None:
                    taken.setdefault((app, index == 0), []).append(took)
        finally:
            session.stop()
    for (app, first), times in sorted(taken.items()):
        times.sort()
        print(f"  {app:9} {'first open' if first else 'beside   '}  n={len(times)} "
              f"median {times[len(times) // 2]:.0f} ms  max {times[-1]:.0f} ms")
    print(f"{len(failures)} failed" if failures else "every app showed its content in budget")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
