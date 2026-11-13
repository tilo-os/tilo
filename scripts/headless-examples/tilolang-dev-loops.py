#!/usr/bin/env python3
"""Measure tilolang's two development loops in a headless nested session.

The app loop: `tilolang dev` follows a copy of Notes in a session it attaches to. Each run
saves a change to the note editor's placeholder and waits until the running app shows it:

  app         from the save to the app's own first frame with the new build (the line
              `tilolang: <app> draws build N at <unix ms>` the app writes when it draws it)
  presented   from the save to the first frame of the new build the compositor presented, the
              first `tilo-display-frame` row the app logs after its draw

Before the first run a note is created and opened, so the detail, the list's selection and the
window's state are there to keep; after each run the app must have taken the build in place
(`runs the new build, with its state`) and the note must still be open.

The runtime loop: `tilolang dev --build-runtime` runs the apps on a tilolang runtime it builds
from this checkout (through `cargo`, on the build server) and builds again on every change of
the runtime's sources. Each run edits a line of the runtime's source and waits until the app was
restarted on the new runtime and presented a frame, end to end.

The session is headless and renders with lavapipe only (`--software`); its state lives in
OUT/state and the session is stopped at the end. Prints the medians of the runs and writes every
sample to OUT/loops.json.

With `--session READY` it attaches to a session that is already running (its `--ready` file)
instead of starting one, and leaves it running. `--app-only` runs the app loop on the built
runtime without timing the runtime loop; `--skip-runtime` uses the session's own runtime.

The desktop loop: `--desktop` times `tilolang dev DIR --search DIR`, which runs the app as an
ordinary window with no session. Its first save (cold) and the warm saves are measured from the
save to two points too:

  app         the app's own first frame of the new build, as above
  presented   the app's first frame of the new build the compositor presented: the app logs
              `tilo-display-frame` rows with `TILO_PROFILE_PRESENTATION=1`, and the first row
              after the draw is that frame

The app still renders on the session's headless compositor (`--software`), as a plain client,
so the numbers include the compositor's presentation. It runs the checkout's `fast` profile
`target/fast/tilolang-app`, the binary the loop's own runtime build makes, and `--tool` and
`--runtime` pick another. `--busy N` keeps N CPU-bound processes running for the measurement, so
the medians also show the loop under load; the load average is printed with every sample.

  scripts/headless-examples/tilolang-dev-loops.py [--runs 5] [--session READY]
      [--app-only | --skip-runtime] [--no-build] [--desktop] [--busy N]
"""

import argparse
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
APP = "so.tilo.lang.notes"
# The string each package under test shows, so a save paints something new: Notes' body
# placeholder in a session, Tasks' new-task placeholder on the desktop.
PLACEHOLDER = {"notes": r'placeholder: "Write[^"]*"',
               "tasks": r'placeholder: "New task(, [^"]*)?"'}
# The app's own line when it draws a new build, and the row it logs when the compositor presents
# a frame (`TILO_PROFILE_PRESENTATION=1`).
DRAW = re.compile(r"tilolang: (\S+) draws build (\d+) at (\d+) ms, (\d+) ms after it arrived")
PRESENTED = re.compile(r"tilo-display-frame \{.*\}")
BUILT = re.compile(r"built in (\d+) ms, (\w+) in (\d+) ms")
# The line of the runtime's source each runtime run edits: a comment, so the build is a real
# incremental rebuild of the crate with nothing else changed.
RUNTIME_SOURCE = REPO / "apps/tilolang/src/updates.rs"
RUNTIME_MARK = "// tilolang-dev-loops: runtime run"


class Failed(Exception):
    pass


def run(argv, **kwargs) -> str:
    result = subprocess.run([str(a) for a in argv], text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(f"{' '.join(map(str, argv))} failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result.stdout


def until(what: str, check, deadline: float = 60.0, step: float = 0.01):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


class Session:
    def __init__(self, state: Path, ready: dict):
        self.state = state
        self.control = ready["control"]
        self.runtime = ready["runtime"]

    def env(self) -> dict:
        state = self.state
        env = dict(os.environ)
        env.update(
            HOME=str(state / "home"),
            XDG_CONFIG_HOME=str(state / "config"),
            XDG_DATA_HOME=str(state / "data"),
            XDG_STATE_HOME=str(state / "state"),
            XDG_CACHE_HOME=str(state / "cache"),
            TILO_IDENTITY_ROOT=str(state / "identity"),
            TILO_APPS_DATA_DIR=str(state / "apps"),
            TILO_RUNTIME_BUNDLE_DIR=str(state / "runtimes"),
            TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=str(state / "keys/runtime-publishers.txt"),
            TILO_TEST_CONTROL=self.control,
            XDG_RUNTIME_DIR=self.runtime,
        )
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        return env

    def control_json(self, *args: str):
        return json.loads(run([BIN / "tilo-control", *args], env=self.env()))

    def actions(self, query: str) -> list:
        return self.control_json("actions", "list", query, "--json").get("actions", [])

    def invoke(self, provider: str, ident: str):
        return self.control_json("actions", "invoke", provider, ident, "--wait")

    def log(self) -> str:
        return (self.state / "logs/session.log").read_text(errors="replace")


class Log:
    """A growing log file, read from where the last read stopped."""

    def __init__(self, path: Path):
        self.path = path
        self.offset = path.stat().st_size if path.exists() else 0

    def new(self) -> str:
        if not self.path.exists():
            return ""
        with self.path.open(errors="replace") as file:
            file.seek(self.offset)
            text = file.read()
        self.offset += len(text.encode())
        return text


class Tail:
    """The complete lines a growing file gained since the last read."""

    def __init__(self, path: Path):
        self.path = path
        self.offset = 0
        self.partial = b""

    def lines(self) -> list[str]:
        if not self.path.exists():
            return []
        with self.path.open("rb") as file:
            file.seek(self.offset)
            data = self.partial + file.read()
            self.offset = file.tell()
        self.partial = b""
        cut = data.rfind(b"\n")
        if cut < 0:
            self.partial = data
            return []
        self.partial = data[cut + 1:]
        return data[:cut].decode(errors="replace").split("\n")


def tilolang_app_pids(session_pid: int) -> set[int]:
    """The tilolang-app processes under the session."""
    found = set()
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if os.readlink(proc / "exe").endswith("/tilolang-app") and descends(int(proc.name), session_pid):
                found.add(int(proc.name))
        except OSError:
            continue
    return found


def descends(pid: int, ancestor: int) -> bool:
    for _ in range(64):
        if pid == ancestor:
            return True
        try:
            stat = (Path("/proc") / str(pid) / "stat").read_text()
        except OSError:
            return False
        pid = int(stat.rsplit(")", 1)[1].split()[1])
        if pid <= 1:
            return False
    return False


def edit_placeholder(source: Path, text: str, package: str = "notes"):
    body = source.read_text()
    new, n = re.subn(PLACEHOLDER[package], f'placeholder: "{text}"', body)
    if n != 1:
        raise Failed(f"cannot find {package}'s placeholder")
    # Replace the file whole, as an editor's save does: a writer that truncates it lets the
    # watcher read half a package.
    saving = source.with_name(source.name + ".save")
    saving.write_text(new)
    os.replace(saving, source)


def wait_for_build(reader, saved: float, timeout: float = 60.0) -> tuple[dict, list[str]]:
    """The app's first frame of the build a save makes, and the first frame it presented after.

    `reader` hands over the lines the app's log gained since the last call.
    """
    drawn = None
    seen = []

    def watch():
        nonlocal drawn
        for line in reader():
            seen.append(line)
            found = DRAW.search(line)
            if drawn is None and found and int(found.group(3)) >= saved * 1000:
                drawn = found
                continue
            if drawn is not None and PRESENTED.search(line):
                return time.time()
        return None

    presented = until("the app's first presented frame of the new build", watch, timeout, 0.001)
    return {
        "build": int(drawn.group(2)),
        "draw_ms": round(int(drawn.group(3)) - saved * 1000, 1),
        "arrival_ms": int(drawn.group(4)),
        "presented_ms": round((presented - saved) * 1000, 1),
    }, seen


def app_loop(dev_log: Log, shell_log: Log, source: Path, runs: int) -> list[dict]:
    samples = []
    for index in range(runs):
        text = f"Write something, take {index + 1}"
        saved = time.time()
        edit_placeholder(source, text)
        sample, seen = wait_for_build(lambda: shell_log.new().splitlines(), saved)
        # `tilolang dev` says what the app does with the build once tilo-install has returned,
        # which can be after the app already drew it.
        said = []
        def told():
            said.extend(seen)
            said.append(dev_log.new())
            text = "".join(said)
            if "starts over" in text or "last good build" in text:
                raise Failed(f"the build was not taken in place: {text[-1000:]}")
            return "takes it with its state" in text
        until("`tilolang dev` to say the app takes the build in place", told, 30)
        samples.append(sample)
        print(f"    app loop {index + 1}: app {sample['draw_ms']:.0f} ms, "
              f"presented {sample['presented_ms']:.0f} ms", file=sys.stderr)
        time.sleep(0.5)
    return samples


# The tool, as the nested session's programs are built (dist/dev/nested-session-build.sh).
PROFILE_ARGS = ["--profile", "nested",
                "--config", 'profile.nested.inherits="release"',
                "--config", "profile.nested.lto=false",
                "--config", "profile.nested.codegen-units=16",
                "--config", "profile.nested.incremental=true"]


def runtime_loop(session_pid: int, dev_log: Log, shell_log: Log, runs: int) -> list[dict]:
    original = RUNTIME_SOURCE.read_text()
    samples = []
    try:
        for index in range(runs):
            old = tilolang_app_pids(session_pid)
            shell_log.new()
            started = time.time()
            RUNTIME_SOURCE.write_text(original.rstrip("\n") + f"\n{RUNTIME_MARK} {index + 1} at {started}\n")
            # `tilolang dev` builds the runtime, re-bakes and reinstalls it and restarts the app.
            said = []
            def built_s():
                said.append(dev_log.new())
                found = re.search(r"the runtime built in ([0-9.]+) s", "".join(said))
                return float(found.group(1)) if found else None
            build_s = until("the runtime's build", built_s, 1800, 0.25)
            new = until("the app on the new runtime", lambda: (tilolang_app_pids(session_pid) - old) or None, 120)
            until("a frame of the restarted app",
                  lambda: any(PRESENTED.search(line) for line in shell_log.new().splitlines()), 60, 0.25)
            total = time.time() - started
            text = "".join(said) + dev_log.new()
            baked = re.findall(r"baked in (\d+) ms, installed in (\d+) ms", text)
            samples.append({"total_s": round(total, 2), "build_s": build_s,
                            "after_build_s": round(total - build_s, 2),
                            "bake_ms": int(baked[-1][0]) if baked else None,
                            "bake_install_ms": int(baked[-1][1]) if baked else None,
                            "pids": sorted(new)})
            print(f"    runtime loop {index + 1}: {total:.1f} s (build {build_s:.1f} s)", file=sys.stderr)
    finally:
        RUNTIME_SOURCE.write_text(original)
    return samples


def start_session(state: Path, out: Path):
    unattach(state)
    ready = out / "ready.json"
    ready.unlink(missing_ok=True)
    argv = [SESSION, "--no-build", "--headless", "--software", "--fresh-desktop", "--apps", "",
            "--state", state, "--ready", ready]
    log = open(out / "session.log", "w")
    # The apps log the frame the compositor presents (see `wait_for_build`).
    env = dict(os.environ, TILO_PROFILE_PRESENTATION="1")
    proc = subprocess.Popen([str(a) for a in argv], stdout=log, stderr=subprocess.STDOUT,
                            env=env, start_new_session=True)
    until("the session", lambda: ready.is_file() or proc.poll() is not None, 300, 0.5)
    if proc.poll() is not None:
        raise Failed(f"the session ended ({proc.returncode}); see {out / 'session.log'}")
    return proc, Session(state, json.loads(ready.read_text())), ready


def stop(proc):
    if proc is not None and proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(60)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)


def unattach(state: Path):
    """Leave no stale mount of a session that died, so the same state starts again."""
    # The launcher picks the runtime directory (a login's XDG_RUNTIME_DIR, or one of its own
    # where there is none); asking it keeps one derivation of that path.
    runtime = Path(subprocess.run([str(SESSION), "--print-runtime", "--state", str(state)],
                                  capture_output=True, text=True, check=True).stdout.strip())
    for line in Path("/proc/self/mounts").read_text().splitlines():
        mount = line.split()[1]
        if mount.startswith(str(runtime) + "/"):
            subprocess.run(["fusermount3", "-u", mount], capture_output=True)
    shutil.rmtree(runtime, ignore_errors=True)


# The desktop loop's compositor is the session's headless one on lavapipe: the app is a plain
# Wayland client of it, as it would be of this machine's desktop.
LAVAPIPE = "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json"


def desktop_env(session: Session, out: Path) -> dict:
    """The tool's own desktop: its state, and the session's compositor instead of the host's."""
    home = out / "desktop"
    env = dict(os.environ)
    for key, value in (
        ("XDG_CONFIG_HOME", home / "config"),
        ("XDG_DATA_HOME", home / "data"),
        ("XDG_STATE_HOME", home / "state"),
        ("XDG_CACHE_HOME", home / "cache"),
    ):
        Path(value).mkdir(parents=True, exist_ok=True)
        env[key] = str(value)
    env.update(
        XDG_RUNTIME_DIR=session.runtime,
        WAYLAND_DISPLAY="wayland-1",
        TILO_PROFILE_PRESENTATION="1",
        VK_DRIVER_FILES=LAVAPIPE,
        VK_ICD_FILENAMES=LAVAPIPE,
        WGPU_ADAPTER_NAME="llvmpipe",
        TILO_DMABUF_DEVICE="",
    )
    for name in ("TILO_TEST_CONTROL", "TILO_IDENTITY_ROOT", "TILO_APPS_DATA_DIR",
                 "TILO_RUNTIME_BUNDLE_DIR", "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"):
        env.pop(name, None)
    return env


def desktop_save(tail: Tail, source: Path, text: str, timeout: float = 60.0) -> dict:
    """One save, timed to the app's first frame of the build it makes and to its presentation."""
    saved = time.time()
    edit_placeholder(source, text, "tasks")
    sample, seen = wait_for_build(tail.lines, saved, timeout)
    said = "\n".join(seen)
    if "starts over" in said or "last good build" in said:
        raise Failed(f"the build was not taken in place: {said[-1000:]}")
    built = BUILT.search(said)
    return {
        **sample,
        "tool_ms": int(built.group(1)) if built else None,
        "tool_build_ms": int(built.group(3)) if built else None,
        "load": round(os.getloadavg()[0], 1),
    }


def desktop_loops(session: Session, out: Path, source: Path, runs: int, tool: Path,
                  runtime: Path, busy: list) -> dict:
    dev_log_path = out / "dev.log"
    dev = subprocess.Popen([str(tool), "dev", out / "src/tasks", "--search", out / "src",
                            "--runtime", str(runtime), "--repo", REPO],
                           env=desktop_env(session, out),
                           stdout=subprocess.DEVNULL, stderr=open(dev_log_path, "w"),
                           start_new_session=True)
    try:
        tail = Tail(dev_log_path)
        opened = []

        def open_app():
            if dev.poll() is not None:
                raise Failed(f"tilolang dev ended: {dev_log_path.read_text()[-2000:]}")
            opened.extend(tail.lines())
            return any("is open;" in line for line in opened)

        until("`tilolang dev` to open the app", open_app, 1800, 0.05)

        def shown():
            return any(PRESENTED.search(line) for line in tail.lines())

        until("the app's first frame", shown, 120, 0.05)
        result = {"busy": len(busy), "warm": []}
        print("==> first save", file=sys.stderr)
        result["first"] = desktop_save(tail, source, "New task, first")
        print(f"    first save: draws {result['first']['draw_ms']:.0f} ms, "
              f"presented {result['first']['presented_ms']:.0f} ms", file=sys.stderr)
        for index in range(runs):
            result["warm"].append(desktop_save(tail, source, f"New task, save {index + 1}"))
            sample = result["warm"][-1]
            print(f"    save {index + 1}: draws {sample['draw_ms']:.0f} ms, "
                  f"presented {sample['presented_ms']:.0f} ms (load {sample['load']})",
                  file=sys.stderr)
            time.sleep(0.5)
        return result
    finally:
        stop(dev)


def start_busy(count: int) -> list:
    return [subprocess.Popen(["yes"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(count)]


def stop_all(processes: list):
    for proc in processes:
        proc.terminate()
    for proc in processes:
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--skip-runtime", action="store_true")
    parser.add_argument("--app-only", action="store_true")
    parser.add_argument("--session", type=Path)
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-dev-loops")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--desktop", action="store_true")
    parser.add_argument("--busy", type=int, default=0)
    parser.add_argument("--tool", type=Path)
    parser.add_argument("--runtime", type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out / "src", ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    state = out / "state"
    if args.desktop:
        unattach(state)
        tool = (args.tool or REPO / "target/fast/tilolang").resolve()
        runtime = (args.runtime or REPO / "target/fast/tilolang-app").resolve()
        if not args.no_build:
            run(["cargo", "build", "--profile", "fast", "--locked",
                 "-p", "tilolang-cli", "--bin", "tilolang"], cwd=REPO)
            run(["cargo", "build", "--profile", "fast", "--locked",
                 "-p", "tilolang-app", "--bin", "tilolang-app"], cwd=REPO)
        for binary in (tool, runtime):
            if not binary.is_file():
                raise Failed(f"no {binary} (build tilolang-cli and tilolang-app with --profile fast)")
    else:
        tool = (args.tool or REPO / "target/nested/tilolang").resolve()
        if not args.no_build:
            # The tool of this checkout: one built before would time the loops of older code.
            run(["cargo", "build", *PROFILE_ARGS, "-p", "tilolang-cli", "--bin", "tilolang"], cwd=REPO)
        if not tool.is_file():
            raise Failed(f"no {tool} (cargo build {' '.join(PROFILE_ARGS)} -p tilolang-cli)")
    # A copy of the package to edit: the repository's sources stay as they are.
    package = "tasks" if args.desktop else "notes"
    shutil.copytree(REPO / "apps/tl" / package, out / "src" / package,
                    ignore=shutil.ignore_patterns("target"))
    source = out / "src" / package / "app.tl"

    result = {}
    session_proc = dev = None
    busy = start_busy(args.busy)
    try:
        if args.session:
            ready = args.session.resolve()
            attached = json.loads(ready.read_text())
            state = Path(attached["state"])
            session = Session(state, attached)
            session_pid = int((state / "session.pid").read_text())
        else:
            run([SESSION, "--no-build", "--prepare-only", "--apps", "", "--state", state])
            session_proc, session, ready = start_session(state, out)
            session_pid = session_proc.pid
        if args.desktop:
            print("==> desktop loop", file=sys.stderr)
            result["desktop"] = desktop_loops(session, out, source, args.runs, tool, runtime, busy)
        else:
            shell_log = Log(state / "logs/session.log")
            dev_log_path = out / "dev.log"
            dev_argv = [tool, "dev", out / "src/notes", "--session", ready, "--repo", REPO]
            if not args.skip_runtime:
                dev_argv += ["--build-runtime"]
            dev = subprocess.Popen([str(a) for a in dev_argv], stdout=subprocess.DEVNULL,
                                   stderr=open(dev_log_path, "w"), start_new_session=True)
            dev_log = Log(dev_log_path)
            dev_log.offset = 0
            def opened():
                if dev.poll() is not None:
                    raise Failed(f"tilolang dev ended: {dev_log_path.read_text()[-2000:]}")
                return "is open;" in dev_log_path.read_text()
            until("`tilolang dev` to open the app", opened, 1800, 0.25)
            shell_log.new()
            until("the app's window",
                  lambda: any(PRESENTED.search(line) for line in shell_log.new().splitlines()), 120, 0.25)
            new_note = until("Notes' New note", lambda: next((a for a in session.actions("New note")
                                                              if a.get("title") == "New note"), None), 30, 0.25)
            session.invoke(new_note["provider"], new_note["id"])
            time.sleep(1.0)
            dev_log.new()
            shell_log.new()
            print("==> app loop", file=sys.stderr)
            result["app"] = app_loop(dev_log, shell_log, source, args.runs)
            if not (args.skip_runtime or args.app_only):
                print("==> runtime loop", file=sys.stderr)
                result["runtime"] = runtime_loop(session_pid, dev_log, shell_log, args.runs)
    finally:
        stop(dev)
        stop(session_proc)
        stop_all(busy)
        (out / "loops.json").write_text(json.dumps(result, indent=2))
    desktop = result.get("desktop")
    if desktop:
        first = desktop["first"]
        warm = desktop["warm"]
        print(f"desktop loop, load {first['load']} (busy {desktop['busy']}): "
              f"first save (cold) draws {first['draw_ms']:.0f} ms, presented {first['presented_ms']:.0f} ms; "
              f"median of {len(warm)} warm saves draws "
              f"{statistics.median(s['draw_ms'] for s in warm):.0f} ms, presented "
              f"{statistics.median(s['presented_ms'] for s in warm):.0f} ms")
    app = result.get("app", [])
    if app:
        print(f"app loop, median of {len(app)}: draws {statistics.median(s['draw_ms'] for s in app):.0f} ms, "
              f"presented {statistics.median(s['presented_ms'] for s in app):.0f} ms")
    rt = result.get("runtime", [])
    if rt:
        print(f"runtime loop, median of {len(rt)}: "
              f"{statistics.median(s['total_s'] for s in rt):.1f} s end to end, "
              f"build {statistics.median(s['build_s'] for s in rt):.1f} s, "
              f"after the build {statistics.median(s['after_build_s'] for s in rt):.1f} s")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL {error}", file=sys.stderr)
        sys.exit(1)
