#!/usr/bin/env python3
"""Measure a native SDK test payload through the production sandbox launcher.

No app installation, identity enrollment, file grants or app networking. The
headless developer fixture is separate from an ordinary first-login OS session.
Foreign-frame observation is not physical display or device-to-photon latency.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import tempfile
import time

from _control import call

APP = "io.tilo.startup-frame-probe"
POLL_SECONDS = 0.02


def stop(child: subprocess.Popen | None) -> None:
    if child is None or child.poll() is not None:
        return
    os.killpg(child.pid, signal.SIGTERM)
    try:
        child.wait(timeout=8)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)


def frames(sock: pathlib.Path) -> list[dict]:
    response = call(str(sock), {"op": "foreign_frames"}, timeout=5)
    if "err" in response:
        raise RuntimeError(f"foreign-frame observation refused: {response['err']}")
    rows = response.get("foreign_frames", {}).get("frames")
    if not isinstance(rows, list):
        raise RuntimeError(f"foreign-frame response cannot observe its subject: {response}")
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=90)
    args = parser.parse_args()
    if not 1 <= args.samples <= 10 or not 0 < args.timeout <= 240:
        parser.error("samples must be 1..10 and timeout must be positive and at most 240s")
    binaries = args.binaries.resolve()
    paths = {"shell": binaries / "shell", "launcher": binaries / "tilo-launcher",
             "probe": binaries / "examples/startup-frame-probe"}
    for path in paths.values():
        if not path.is_file():
            raise RuntimeError(f"required built binary missing: {path}")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    print(f"ARTIFACTS {out}", flush=True)
    # Keep AF_UNIX endpoints short and private; large binaries/evidence stay on disk.
    runtime = pathlib.Path(tempfile.mkdtemp(prefix="tilo-startup-"))
    runtime.chmod(0o700)
    profile = out / "profile"
    for name in ("home", "config", "data", "state", "cache", "identity", "system"):
        (profile / name).mkdir(parents=True, mode=0o700)
    env = dict(os.environ)
    env.update(HOME=str(profile / "home"), XDG_CONFIG_HOME=str(profile / "config"),
               XDG_DATA_HOME=str(profile / "data"), XDG_STATE_HOME=str(profile / "state"),
               XDG_CACHE_HOME=str(profile / "cache"), XDG_RUNTIME_DIR=str(runtime),
               TILO_IDENTITY_ROOT=str(profile / "identity"),
               TILO_AUTHORITY_SOCKET_DIR=str(runtime / "authority"),
               TILO_FS_AUTHORITY_CACHE_DIR=str(profile / "system/fs-cache"),
               TILO_FS_CONTENT_PEER_CACHE_DIR=str(profile / "system/peer-cache"),
               TILO_TEST_CONTROL=str(runtime / "control.sock"), TILO_HEADLESS_SIZE="1280x720",
               TILO_FRAME_TRACE=str(out / "compositor-frames.csv"),
               TILO_STARTUP_METRICS_PATH=str(out / "compositor-startup.json"))
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_TEST_CONTROL_FIRST_USE"):
        env.pop(name, None)
    payload = out / "payload"
    (payload / "bin").mkdir(parents=True)
    shutil.copy2(paths["probe"], payload / "bin/probe")
    manifest = payload / "manifest.toml"
    manifest.write_text(f'package_id = "{APP}"\nversion = "0.0.0"\nkind = "app"\n'
                        '[entrypoint]\nbin = "bin/probe"\n[sandbox.net]\nmode = "off"\n')
    result = {"scope": "registered host wrapper launching a confined native SDK payload in a watched headless compositor",
              "physical_presentation": False, "cache": "fresh processes; private app data initially empty and retained between samples; host/driver caches not flushed",
              "poll_interval_ms": POLL_SECONDS * 1000, "samples": [],
              "binary_sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in paths.items()}}
    # The existing host action registers the wrapper's process ancestry before
    # exec; its child still enters the normal net-off native sandbox.
    helper = binaries / "examples/startup_frame_timeline"
    if not helper.is_file():
        raise RuntimeError(f"required built registered-launch wrapper missing: {helper}")
    desktop_dir = profile / "data/applications"
    desktop_dir.mkdir()
    def desktop_quote(value):
        return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$') + '"'
    (desktop_dir / f"{APP}.desktop").write_text(
        '[Desktop Entry]\nType=Application\nName=Startup frame probe\n'
        f'Exec={desktop_quote(helper)} --launch-probe {desktop_quote(payload)}\n'
        f'Terminal=false\nStartupWMClass={APP}\n')
    shell = None
    def stop_probe():
        pidfile = payload / "probe.pid"
        if not pidfile.exists():
            return
        pid = int(pidfile.read_text())
        try:
            argv = pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
            if os.fsencode(helper) not in argv or os.fsencode(payload) not in argv or os.getpgid(pid) != pid:
                raise RuntimeError("refusing cleanup of a process outside this probe")
            os.killpg(pid, signal.SIGTERM)
        except (FileNotFoundError, ProcessLookupError):
            pass
        pidfile.unlink(missing_ok=True)
    try:
        with (out / "compositor.log").open("w") as log:
            started = time.monotonic_ns()
            shell = subprocess.Popen([str(paths["shell"]), "--backend", "headless"], env=env,
                                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            sock = runtime / "control.sock"
            deadline = time.monotonic() + args.timeout
            while True:
                if shell.poll() is not None:
                    raise RuntimeError(f"compositor exited {shell.returncode}; see compositor.log")
                try:
                    state = call(str(sock), {"op": "state_dump"}, timeout=0.5)["state"]
                    break
                except (OSError, KeyError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError("compositor did not answer semantic state before deadline")
                    time.sleep(POLL_SECONDS)
            if state["chrome"]["input_capture"]["onboarding"]:
                raise RuntimeError("synthetic fixture is in first use; do not bypass ordinary OS enrollment")
            result["compositor_semantic_ready_ms"] = (time.monotonic_ns() - started) / 1e6
            watch = call(str(sock), {"op": "watch", "watching": True})
            if "err" in watch:
                raise RuntimeError(f"headless watch refused: {watch['err']}")
            banner = re.search(r'tilo compositor listening on "([^"\n]+)"', (out / "compositor.log").read_text())
            if not banner:
                raise RuntimeError("production Wayland endpoint banner missing")
            wayland = runtime / banner.group(1)
            if not wayland.is_socket():
                raise RuntimeError("announced production Wayland endpoint is not a socket")
            deadline = time.monotonic() + args.timeout
            while True:
                catalog = call(str(sock), {"op": "actions_enumerate", "query": "Startup frame probe"})
                actions = catalog.get("answer", {}).get("actions", [])
                launch = [a for a in actions if a.get("provider") == "host-applications" and a.get("id") in (APP, APP + ".desktop")]
                if len(launch) == 1:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"registered synthetic launch action missing: {catalog}")
                time.sleep(POLL_SECONDS)
            for index in range(args.samples):
                # Remove only the prior probe's metric; keep normal launcher routing.
                for previous in (profile / "data").rglob("startup-frame-probe.json"):
                    previous.unlink()
                ignore = {row["surface_id"] for row in frames(sock)}
                before = time.monotonic_ns()
                response = call(str(sock), {"op": "actions_invoke", "provider": launch[0]["provider"], "id": launch[0]["id"]})
                if "err" in response:
                    raise RuntimeError(f"registered launch refused: {response}")
                spawned = time.monotonic_ns()
                lower = before
                deadline = time.monotonic() + args.timeout
                while True:
                    query_before = time.monotonic_ns()
                    observed = frames(sock)
                    query_after = time.monotonic_ns()
                    positive = [row for row in observed if row["surface_id"] not in ignore
                                and row.get("app_id") == APP and row.get("frame_seq", 0) > 0
                                and row.get("presented") and not row.get("opening") and not row.get("last_error")]
                    if positive:
                        frame = positive[0]
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"no new {APP} presented surface before deadline; see payload/probe.log and compositor.log")
                    lower = query_before
                    time.sleep(POLL_SECONDS)
                while True:
                    metric_paths = list((profile / "data").rglob("startup-frame-probe.json"))
                    if len(metric_paths) > 1:
                        raise RuntimeError("ambiguous private probe metrics")
                    metrics = json.loads(metric_paths[0].read_text()) if metric_paths else None
                    if metrics and any(e["name"] == "first-frame-presented" for e in metrics["events"]):
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError("SDK did not record an actually presented first frame")
                    time.sleep(POLL_SECONDS)
                row = {"index": index, "launch_action_return_ms": (spawned - before) / 1e6,
                       "first_frame_observation_lower_ms": (lower - before) / 1e6,
                       "first_frame_observation_upper_ms": (query_after - before) / 1e6,
                       "frame": frame, "sdk_startup": metrics}
                result["samples"].append(row)
                (out / "timeline.json").write_text(json.dumps(result, indent=2) + "\n")
                print(json.dumps(row), flush=True)
                stop_probe()
                if (payload / "probe.log").exists():
                    shutil.copy2(payload / "probe.log", out / f"app-{index}.log")
                deadline = time.monotonic() + 8
                while any(row["surface_id"] == frame["surface_id"] for row in frames(sock)):
                    if time.monotonic() > deadline:
                        raise RuntimeError("probe surface did not close after owned process cleanup")
                    time.sleep(POLL_SECONDS)
            with (out / "compositor-frames.csv").open() as trace:
                recorded = list(csv.DictReader(trace))
            if not recorded or not any(row["painted"] == "1" for row in recorded):
                raise RuntimeError("live compositor trace has no actually painted sample")
            result["live_trace_rows"] = len(recorded)
            (out / "timeline.json").write_text(json.dumps(result, indent=2) + "\n")
    finally:
        try:
            stop_probe()
        finally:
            stop(shell)
            shutil.rmtree(runtime)


if __name__ == "__main__":
    main()
