#!/usr/bin/env python3
"""Measure a native SDK test payload through the production sandbox launcher.

No app installation, identity enrollment, file grants or app networking. The
headless developer fixture is separate from an ordinary first-login OS session.
Window-frame observation is not physical display or device-to-photon latency.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import time

from _control import call

REPO = pathlib.Path(__file__).resolve().parents[2]
# The one place that picks a session's private runtime directory.
SESSION_LAUNCHER = REPO / "dist" / "dev" / "nested-session.sh"
APP = "io.tilo.startup-frame-probe"
POLL_SECONDS = 0.02


def display_name(runtime: pathlib.Path) -> str:
    """The Wayland socket name the session on `runtime` published for the login, as every
    program it starts reads it (`$XDG_RUNTIME_DIR/tilo-session/wayland-display`)."""
    published = runtime / "tilo-session" / "wayland-display"
    try:
        name = published.read_text().strip()
    except FileNotFoundError:
        raise RuntimeError(f"the session published no Wayland display at {published}") from None
    if not name:
        raise RuntimeError(f"the session's published display {published} names no socket")
    return name


def opens_probe(row: dict) -> bool:
    """Whether `row` is the desk's one launch of the probe: the applications provider's, under
    this machine's own app id (`host:<desktop id>`) or the desktop file's."""
    if row.get("provider") != "applications":
        return False
    desktop_id = str(row.get("id", ""))
    return desktop_id.removeprefix("host:") in (APP, APP + ".desktop")


def runtime_directory(state: pathlib.Path) -> pathlib.Path:
    """The private runtime directory a session on `state` uses.

    The launcher picks it (a login's XDG_RUNTIME_DIR, the user's /run/user
    seat, or a short directory of its own) and prints it, so no second copy of
    that rule lives here. A socket path must fit the kernel's limit, which is
    why it is never left to whatever depth this run's artifacts land at."""
    printed = subprocess.run(
        [str(SESSION_LAUNCHER), "--print-runtime", "--state", str(state)],
        capture_output=True, text=True, check=True,
    )
    runtime = printed.stdout.strip()
    if not runtime:
        raise SystemExit("the session launcher named no runtime directory")
    runtime = pathlib.Path(runtime)
    runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
    return runtime


class ProductionSeed:
    """Exercise the ordinary publisher and launcher, without Mesa env overrides."""

    def __init__(self, runtime: pathlib.Path, profile: pathlib.Path, out: pathlib.Path):
        self.active = runtime / "tilo/pipeline-seed/mesa"
        self.saved = out / "public-seed-source"
        self.profile = profile
        self.app_cache = None
        self.hashes = {}

    def wait(self, shell: subprocess.Popen, timeout: float) -> dict:
        started = time.monotonic()
        while not (self.active / "seed.json").is_file():
            if shell.poll() is not None or time.monotonic() - started > timeout:
                raise RuntimeError("ordinary compositor did not publish a public Mesa seed")
            time.sleep(POLL_SECONDS)
        shutil.copytree(self.active, self.saved)
        manifest = json.loads((self.saved / "seed.json").read_text())
        for entry in manifest["files"]:
            path = self.saved / entry["path"]
            if not path.is_relative_to(self.saved) or not path.is_file():
                raise RuntimeError("publisher produced an invalid archive path")
            self.hashes[entry["path"]] = hashlib.sha256(path.read_bytes()).hexdigest()
        return {"publication_wait_ms": (time.monotonic() - started) * 1000,
                "archive_bytes": sum(entry["bytes"] for entry in manifest["files"]),
                "fingerprint": manifest["fingerprint"], "archive_sha256": self.hashes}

    def select(self, index: int, count: int) -> str:
        if self.app_cache is not None:
            shutil.rmtree(self.app_cache, ignore_errors=False)
        if self.active.exists():
            shutil.rmtree(self.active)
        mode = "cold" if index % 2 == 0 else "seed"
        if index == count - 2:
            mode = "wrong-driver"
        elif index == count - 1:
            mode = "corrupt"
        if mode == "cold":
            return mode
        shutil.copytree(self.saved, self.active)
        manifest_path = self.active / "seed.json"
        manifest = json.loads(manifest_path.read_text())
        if mode == "wrong-driver":
            replacements = {}
            for entry in manifest["files"]:
                driver = pathlib.Path(entry["path"]).parts[0]
                replacements[driver] = f"{(int(driver[0], 16) + 1) % 16:x}" + driver[1:]
            if len(set(replacements.values())) != len(replacements):
                raise RuntimeError("wrong-driver fixture namespaces collided")
            # Stage every namespace first: an invalidated name may coincide
            # with another original namespace, but must never merge archives.
            for index, old in enumerate(replacements):
                (self.active / old).rename(self.active / f"invalidated-{index}")
            for index, new in enumerate(replacements.values()):
                (self.active / f"invalidated-{index}").rename(self.active / new)
            for entry in manifest["files"]:
                parts = pathlib.Path(entry["path"]).parts
                entry["path"] = str(pathlib.Path(replacements[parts[0]], *parts[1:]))
            manifest_path.unlink()
            manifest_path.write_text(json.dumps(manifest))
        elif mode == "corrupt":
            archive = self.active / manifest["files"][0]["path"]
            data = bytearray(archive.read_bytes())
            data[-1] ^= 1
            archive.unlink()
            archive.write_bytes(data)
        return mode

    def observe(self, metric: pathlib.Path, log: str, mode: str) -> None:
        app = metric.parent.parent.parent
        if not app.is_relative_to(self.profile / "data"):
            raise RuntimeError("private app cache is outside the owned fixture")
        self.app_cache = app / ".cache"
        if not self.app_cache.is_dir():
            raise RuntimeError("ordinary launcher private app cache is missing")
        if "landlock: fully enforced" not in log:
            raise RuntimeError("ordinary app did not report fully enforced Landlock")
        if "no_new_privs=true" not in log or "net_restricted=true" not in log:
            raise RuntimeError("ordinary app confinement was not observed")
        seeded = "validated read-only Mesa seed in private bounded cache" in log
        if seeded != (mode in ("seed", "wrong-driver")):
            raise RuntimeError(f"ordinary launcher cache selection disagrees with {mode}")
        if mode == "corrupt" and "Mesa seed unavailable" not in log:
            raise RuntimeError("corrupt seed did not take the observed cold fallback")
        if mode != "cold" and "public seed write-open denied" not in log:
            raise RuntimeError("app did not observe read-only access to the session seed")
        for relative, digest in self.hashes.items():
            if hashlib.sha256((self.saved / relative).read_bytes()).hexdigest() != digest:
                raise RuntimeError("public seed source changed")
            if mode == "seed" and hashlib.sha256((self.active / relative).read_bytes()).hexdigest() != digest:
                raise RuntimeError("ordinary app changed the read-only session seed")


def stop(child: subprocess.Popen | None) -> None:
    if child is None or child.poll() is not None:
        return
    os.killpg(child.pid, signal.SIGTERM)
    try:
        child.wait(timeout=8)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)


def windows(sock: pathlib.Path) -> list[dict]:
    """The desk's open windows: each with `window`, `app_id`, `rect` (`[x, y, width, height]`,
    null while no window of the tab is mapped) and `frames`, the frames its client committed."""
    response = call(str(sock), {"op": "windows"}, timeout=5)
    if "err" in response:
        raise RuntimeError(f"window observation refused: {response['err']}")
    rows = response.get("answer", {}).get("windows")
    if not isinstance(rows, list):
        raise RuntimeError(f"window response cannot observe its subject: {response}")
    return rows


def idle_sample(shell: subprocess.Popen) -> dict:
    def read():
        proc = pathlib.Path(f"/proc/{shell.pid}")
        fields = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
        rss = next(line.split()[1] for line in (proc / "status").read_text().splitlines()
                   if line.startswith("VmRSS:"))
        return int(fields[11]) + int(fields[12]), int(rss)
    ticks, rss_before = read()
    start = time.monotonic()
    time.sleep(5)
    after, rss_after = read()
    seconds = time.monotonic() - start
    return {"scope": "owned headless compositor after public-seed publication and all probe exits; watcher disabled; not full OS idle",
            "seconds": seconds,
            "cpu_percent_one_core": 100 * (after - ticks) / os.sysconf("SC_CLK_TCK") / seconds,
            "rss_kib_before": rss_before, "rss_kib_after": rss_after}

def seed_fixture(seed_dir: pathlib.Path, archive_root: pathlib.Path | None) -> None:
    seed_dir.mkdir(parents=True, mode=0o700)
    (seed_dir / "startup-probe-fresh-mesa").write_text("private fixture\n")
    if archive_root is None:
        return
    source = archive_root.resolve(strict=True)
    if any(path.is_symlink() for path in source.rglob("*")):
        raise RuntimeError("fixture seed must contain regular archives, not symlinks")
    archives = list(source.glob("*/*/foz_cache*.foz"))
    if not archives:
        raise RuntimeError("no driver-specific fixture archives observed")
    for archive in archives:
        if not archive.is_file() or archive.stat().st_size == 0:
            raise RuntimeError(f"missing or empty fixture archive: {archive}")
        target = seed_dir / "mesa" / archive.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        shutil.copyfile(archive, target)
        target.chmod(0o444)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--fresh-mesa-cache", action="store_true")
    parser.add_argument("--mesa-seed", type=pathlib.Path)
    parser.add_argument("--mesa-seed-source", choices=("package", "session"), default="package")
    parser.add_argument("--alternate-mesa-seed", action="store_true")
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--production-mesa-seed", action="store_true",
                        help="six alternating fresh-cache launches plus wrong-driver and corrupt fallbacks; requires --samples 8")
    args = parser.parse_args()
    if not 1 <= args.samples <= 10 or not 0 < args.timeout <= 240:
        parser.error("samples must be 1..10 and timeout must be positive and at most 240s")
    if args.production_mesa_seed and (args.fresh_mesa_cache or args.mesa_seed or args.alternate_mesa_seed):
        parser.error("ordinary production qualification cannot use test-only Mesa fixtures")
    if args.production_mesa_seed and args.samples != 8:
        parser.error("production Mesa qualification requires --samples 8")
    if args.production_mesa_seed and any(name.startswith(("MESA_SHADER_CACHE_", "MESA_DISK_CACHE_")) for name in os.environ):
        parser.error("production Mesa qualification requires an environment without cache overrides")

    if args.alternate_mesa_seed and (not args.mesa_seed or args.mesa_seed_source != "package" or args.samples % 2):
        parser.error("alternating seed mode requires a package seed and an even sample count")
    binaries = args.binaries.resolve()
    paths = {"session": binaries / "tilo-session", "launcher": binaries / "tilo-launcher",
             "probe": binaries / "examples/startup-frame-probe"}
    for path in paths.values():
        if not path.is_file():
            raise RuntimeError(f"required built binary missing: {path}")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    print(f"ARTIFACTS {out}", flush=True)
    profile = out / "profile"
    for name in ("home", "config", "data", "state", "cache", "identity", "system"):
        (profile / name).mkdir(parents=True, mode=0o700)
    if not SESSION_LAUNCHER.is_file():
        raise RuntimeError(f"no session launcher at {SESSION_LAUNCHER}")
    # The private runtime directory the launcher names for this fixture's
    # state: short and private wherever the artifacts land, and the same place
    # a session on that state puts its sockets.
    runtime = runtime_directory(profile / "state")
    env = dict(os.environ)
    env.update(HOME=str(profile / "home"), XDG_CONFIG_HOME=str(profile / "config"),
               XDG_DATA_HOME=str(profile / "data"), XDG_STATE_HOME=str(profile / "state"),
               XDG_CACHE_HOME=str(profile / "cache"), XDG_RUNTIME_DIR=str(runtime),
               TILO_IDENTITY_ROOT=str(profile / "identity"),
               TILO_AUTHORITY_SOCKET_DIR=str(runtime / "authority"),
               TILO_FS_AUTHORITY_CACHE_DIR=str(profile / "system/fs-cache"),
               TILO_FS_CONTENT_PEER_CACHE_DIR=str(profile / "system/peer-cache"),
               TILO_TEST_CONTROL=str(runtime / "control.sock"),
               TILO_FRAME_TRACE=str(out / "compositor-frames.csv"),
               TILO_STARTUP_METRICS_PATH=str(out / "compositor-startup.json"))
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_TEST_CONTROL_FIRST_USE"):
        env.pop(name, None)
    payload = out / "payload"
    (payload / "bin").mkdir(parents=True)
    shutil.copy2(paths["probe"], payload / "bin/probe")
    seed_dir = (payload / "startup-mesa-seed" if args.mesa_seed_source == "package"
                else runtime / "tilo/pipeline-seed")
    try:
        if args.fresh_mesa_cache or args.mesa_seed:
            seed_fixture(seed_dir, args.mesa_seed)
    except BaseException:
        shutil.rmtree(runtime)
        raise
    manifest = payload / "manifest.toml"
    manifest.write_text(f'[package]\nid = "{APP}"\nversion = "0.0.0"\nkind = "app"\n'
                        f'[publisher]\nidentity = "ed25519:{"00" * 32}"\n'
                        '[sandbox]\nnamespaces = ["user", "pid", "uts", "ipc", "mount"]\n'
                        'seccomp = "tilo-default"\nlandlock = "strict"\n'
                        '[sandbox.net]\nmode = "off"\n'
                        '[entrypoint]\nbin = "bin/probe"\n')
    result = {"scope": "registered host wrapper launching a confined native SDK payload in a watched headless compositor",
              "physical_presentation": False, "cache": "fresh processes; private app data initially empty and retained between samples; host/driver caches not flushed",
              "poll_interval_ms": POLL_SECONDS * 1000, "samples": [],
              "host_load_before": os.getloadavg(),
              "binary_sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in paths.items()}}
    result["mesa_cache"] = (f"fresh private directory per app process with read-only {args.mesa_seed_source} seed"
                            if args.mesa_seed else "fresh private directory per app process"
                            if args.fresh_mesa_cache else "ordinary private app cache retained")
    result["mesa_seed_source"] = args.mesa_seed_source if args.mesa_seed else None
    result["alternate_mesa_seed"] = args.alternate_mesa_seed
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
    production_seed = ProductionSeed(runtime, profile, out) if args.production_mesa_seed else None
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
    provision = subprocess.run([str(paths["session"]), "--provision-identity"], env=env,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                               timeout=args.timeout)
    if provision.returncode != 0:
        raise RuntimeError(f"the fixture's identity was not provisioned: {provision.stdout[-2000:]}")
    try:
        with (out / "compositor.log").open("w") as log:
            started = time.monotonic_ns()
            shell = subprocess.Popen(
                [str(paths["session"]), "--backend", "headless", "--size", "1280x720"], env=env,
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
            # A fixture in first use shows its own setup controls, which this probe must not drive
            # past: it measures an ordinary desktop's first frame, not the enrollment flow.
            targets = call(str(sock), {"op": "list_targets"}, timeout=5).get("targets", [])
            if any(str(entry.get("name", "")).startswith("onboarding.") for entry in targets):
                raise RuntimeError("synthetic fixture is in first use; do not bypass ordinary OS enrollment")
            result["compositor_semantic_ready_ms"] = (time.monotonic_ns() - started) / 1e6
            watch = call(str(sock), {"op": "watch", "watching": True})
            if "err" in watch:
                raise RuntimeError(f"headless watch refused: {watch['err']}")
            wayland = runtime / display_name(runtime)
            if not wayland.is_socket():
                raise RuntimeError("the published production Wayland endpoint is not a socket")
            deadline = time.monotonic() + args.timeout
            while True:
                catalog = call(str(sock), {"op": "actions_enumerate", "query": "Startup frame probe"})
                actions = catalog.get("answer", {}).get("actions", [])
                launch = [a for a in actions if opens_probe(a)]
                if len(launch) == 1:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"registered synthetic launch action missing: {catalog}")
                time.sleep(POLL_SECONDS)
            if production_seed:
                result["production_mesa_seed"] = production_seed.wait(shell, args.timeout)
                result["cache"] = "fresh private app cache per launch; ordinary compositor-generated public seed; six alternating cold/seed launches and two observed fallback controls"
            for index in range(args.samples):
                mode = production_seed.select(index, args.samples) if production_seed else None

                seeded = bool(args.mesa_seed) and (not args.alternate_mesa_seed or index % 2 == 1)
                if args.alternate_mesa_seed:
                    active, inactive = seed_dir / "mesa", seed_dir / "mesa-inactive"
                    if seeded and inactive.exists():
                        inactive.rename(active)
                    elif not seeded and active.exists():
                        active.rename(inactive)
                # Remove only the prior probe's metric; keep normal launcher routing.
                for previous in (profile / "data").rglob("startup-frame-probe.json"):
                    previous.unlink()
                ignore = {row["window"] for row in windows(sock)}
                host_load = os.getloadavg()
                before = time.monotonic_ns()
                response = call(str(sock), {"op": "actions_invoke", "provider": launch[0]["provider"], "id": launch[0]["id"]})
                if "err" in response:
                    raise RuntimeError(f"registered launch refused: {response}")
                spawned = time.monotonic_ns()
                lower = before
                deadline = time.monotonic() + args.timeout
                while True:
                    query_before = time.monotonic_ns()
                    observed = windows(sock)
                    query_after = time.monotonic_ns()
                    positive = [row for row in observed if row["window"] not in ignore
                                and row.get("app_id") == APP and row.get("frames", 0) > 0
                                and row.get("rect") is not None]
                    if positive:
                        frame = positive[0]
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"no new {APP} window with a drawn frame before deadline; see payload/probe.log and compositor.log")
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
                names = {event["name"] for event in metrics["events"]}
                if (args.fresh_mesa_cache or args.mesa_seed) and "probe-fresh-mesa-cache-ready" not in names:
                    raise RuntimeError("app did not observe the requested fresh Mesa fixture")
                if seeded != ("probe-mesa-seed-ready" in names):
                    raise RuntimeError("app did not observe the requested cold/seeded shader mode")
                if (args.fresh_mesa_cache or args.mesa_seed) and f"probe-mesa-source-{args.mesa_seed_source}" not in names:
                    raise RuntimeError("app did not observe the requested read-only seed source")
                row = {"index": index, "mesa_seeded": seeded, "launch_action_return_ms": (spawned - before) / 1e6,
                       "host_load_before": host_load,
                       "first_frame_observation_lower_ms": (lower - before) / 1e6,
                       "first_frame_observation_upper_ms": (query_after - before) / 1e6,
                       "frame": frame, "sdk_startup": metrics}
                if production_seed:
                    if "probe-fresh-mesa-cache-ready" in names:
                        raise RuntimeError("ordinary production launch unexpectedly enabled a test-only cache fixture")
                    production_seed.observe(metric_paths[0], (payload / "probe.log").read_text(), mode)
                    row["mesa_mode"] = mode
                    row["mesa_seeded"] = mode == "seed"
                result["samples"].append(row)
                (out / "timeline.json").write_text(json.dumps(result, indent=2) + "\n")
                print(json.dumps(row), flush=True)
                stop_probe()
                if (payload / "probe.log").exists():
                    shutil.copy2(payload / "probe.log", out / f"app-{index}.log")
                deadline = time.monotonic() + 8
                while any(row["window"] == frame["window"] for row in windows(sock)):
                    if time.monotonic() > deadline:
                        raise RuntimeError("the probe's window did not close after owned process cleanup")
                    time.sleep(POLL_SECONDS)
            with (out / "compositor-frames.csv").open() as trace:
                recorded = list(csv.DictReader(trace))
            if not recorded or not any(row["painted"] == "1" for row in recorded):
                raise RuntimeError("live compositor trace has no actually painted sample")
            result["live_trace_rows"] = len(recorded)
            if production_seed:
                unwatch = call(str(sock), {"op": "watch", "watching": False})
                if "err" in unwatch:
                    raise RuntimeError(f"idle observer could not stop the headless watch: {unwatch}")
                result["idle_after_publication"] = idle_sample(shell)
            (out / "timeline.json").write_text(json.dumps(result, indent=2) + "\n")
    finally:
        try:
            stop_probe()
        finally:
            stop(shell)
            shutil.rmtree(runtime)


if __name__ == "__main__":
    main()
