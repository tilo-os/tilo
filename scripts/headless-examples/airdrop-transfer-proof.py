#!/usr/bin/env python3
"""Run signed Files → native headless compositor → pinned OpenDrop, in isolation.

Requires prebuilt debug shell/app/install/launcher/test-registry binaries, thin
Files plus its matching runtime library and Rust libstd, and the OS runtime-bundle
CLI. Build with an authorized Cargo drop-in before running; this script never builds or configures
interfaces. The pinned GPL-3.0 OpenDrop checkout and its Python dependencies are
external test prerequisites, never shipped components. Missing prerequisites,
missing observable actions/frames, and timeouts fail rather than skip.

All certificates, identities, grants, registry and application state are temporary.
Only loopback is used. No user desktop, shared VM, real receiver or account is used.
"""

import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True

from _control import call

REPO = Path(__file__).resolve().parents[2]
REVISION = "11fe7ba7861093b302bc0637e8cb10adf2d29337"
PAYLOAD = b"Native compositor Files OpenDrop exact bytes\0\xff"


def wait_for(predicate, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.15)
    raise RuntimeError(f"not observed: {description}")


class Processes:
    """Own only process groups created by this proof, including sandbox children."""

    def __init__(self):
        self.children = []

    def spawn(self, command, env, log):
        process = subprocess.Popen(command, cwd=REPO, env=env, stdout=log,
                                   stderr=log, start_new_session=True)
        self.children.append(process)
        return process

    def stop(self, process):
        # A group can survive its parent (e.g. a sandboxed app). Always signal the
        # group we created, then reap its leader. Never search process command lines.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, sig)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                continue
            if sig == signal.SIGKILL:
                break

        if process in self.children:
            self.children.remove(process)

    def close(self):
        for process in reversed(self.children.copy()):
            self.stop(process)


class Desktop:
    def __init__(self, socket):
        self.socket = str(socket)

    def request(self, op, **args):
        response = call(self.socket, {"op": op, **args}, timeout=5)
        if "err" in response:
            raise RuntimeError(f"{op} failed: {response['err']}")
        return response

    def actions(self):
        return self.request("actions_enumerate")["answer"]["actions"]

    def action(self, operation, before=()):
        return next((row for row in self.actions() if row.get("operation") == operation
                     and row["provider"] == "app-actions" and row["id"] not in before
                     and not row["id"].startswith("directory")), None)

    def invoke(self, action, args=None):
        return self.request("actions_invoke", provider=action["provider"],
                            id=action["id"], args=args or {})

    def output(self, reply):
        receipt = reply["answer"]["receipt"]
        update = receipt["update"]
        if update["state"] not in ("succeeded", "failed", "cancelled"):
            invocation = receipt["broker_invocation_id"]
            def finished():
                current = self.request("actions_status", invocation_id=invocation)["answer"]["receipt"]["update"]
                return current if current["state"] in ("succeeded", "failed", "cancelled") else None
            update = wait_for(finished, "structured action answer")
        assert update["state"] == "succeeded", update
        return update.get("output", {}).get("value")

    def transfer_phase(self, action, expected):
        def observed():
            status = self.output(self.invoke(action))["transfer"]
            assert status["total_bytes"] == len(PAYLOAD)
            assert 0 <= status["transferred_bytes"] <= len(PAYLOAD)
            return status if status["phase"] == expected else None
        return wait_for(observed, "public transfer phase " + expected)

    def dialog(self):
        return self.request("state_dump")["state"]["chrome"]["portal_dialog"]

    def start(self, port):
        action = wait_for(lambda: self.action("send-airdrop"), "Files send-airdrop")
        before = {row["id"] for row in self.actions()}
        self.invoke(action, {"address": "127.0.0.1", "port": port, "confirm": True})
        sheet = wait_for(self.dialog, "new trusted verification sheet")
        assert sheet["handle"].startswith("airdrop:") and sheet.get("value") == ""
        self.transfer = wait_for(lambda: self.action("status", before), "public transfer status")
        waiting = self.transfer_phase(self.transfer, "awaiting-consent")
        assert waiting["transferred_bytes"] == 0
        return sheet

    def enter_pin(self, value):
        assert self.dialog()["handle"].startswith("airdrop:")
        # The compositor owns this focused shared TextInput. Canonical key/type
        # transport exercises its real controller; no direct consent queue writes.
        for op, args in (("key_down", {"key": "Control"}), ("key", {"key": "a"}),
                         ("key_up", {"key": "Control"}), ("type", {"text": value}),
                         ("key", {"key": "Enter"})):
            self.request(op, **args)


def prepare_profile(root, args):
    env = os.environ.copy()
    # Identity overrides, ambient buses and UI sockets must not attach to a user.
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "TILO_USER_ID"):
        env.pop(key, None)
    for key, name in (("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                      ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                      ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "run"),
                      ("TILO_IDENTITY_ROOT", "identity"), ("TILO_AUTHORITY_SOCKET_DIR", "authority"),
                      ("TILO_FS_AUTHORITY_CACHE_DIR", "fs-cache"),
                      ("TILO_FS_CONTENT_PEER_CACHE_DIR", "peer-cache")):
        directory = root / name
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    config = root / "config/tilo"
    config.mkdir()
    (config / "settings.json").write_text(json.dumps({"continuity": {
        "enabled": True, "file_transfer": True}}))
    env.update(TILO_TEST_CONTROL=str(root / "run/control.sock"),
               TILO_STATE_PATH=str(config / "state.json"),
               TILO_SESSION_SETTINGS=str(config / "settings.json"),
               TILO_AIRDROP_LAB="1", TILO_IDENTITY_FILE_SECRETS="1",
               TILO_HEADLESS_SIZE="1280x900", WGPU_BACKEND="vulkan",
               WGPU_ADAPTER_NAME="llvmpipe", RUST_LOG="info",
               TILO_APPS_DATA_DIR=str(root / "apps"),
               TILO_STORE_CONFIG_DIR=str(root / "store"),
               TILO_STORE_CACHE_DIR=str(root / "store-cache"),
               TILO_APP_BIN=str(args.debug_bin / "app"),
               TILO_INSTALL_BIN=str(args.debug_bin / "tilo-install"),
               TILO_LAUNCHER_BIN=str(args.debug_bin / "tilo-launcher"))
    identity = root / "runtime-identity.json"
    identity.write_text(json.dumps({"secret_key_hex": os.urandom(32).hex()}))
    identity.chmod(0o600)
    with (root / "runtime.log").open("wb") as log:
        subprocess.run([str(args.runtime_bundle), "build", "--release", "2026.07",
                        "--out-root", str(root / "data/tilo-runtime"),
                        "--version-out", str(root / "runtime-version"),
                        "--identity", str(identity), "--include", str(args.thin_dir / "libruntime_dylib.so"),
                        "--include", str(args.libstd)], env=env, stdout=log, stderr=log,
                       check=True, timeout=120)
    version = (root / "runtime-version").read_text().strip()
    manifest = root / "files-manifest.toml"
    manifest.write_text((REPO / "apps/file-explorer/tilo-app.toml").read_text().replace(
        "tilo-runtime/2026.07", "tilo-runtime/" + version))
    (root / "home/approved.txt").write_bytes(PAYLOAD)
    return env, manifest


def boot_files(root, args, processes, stack):
    env, manifest = prepare_profile(root, args)
    registry_log = stack.enter_context((root / "registry.log").open("wb"))
    registry = processes.spawn([str(args.debug_bin / "examples/serve-test-registry"),
        "--config-dir", str(root / "store"), "--payload", str(args.thin_dir / "file-explorer"),
        "--manifest", str(manifest), "--package-id", "so.tilo.file-explorer", "--name", "Files"],
        env, registry_log)
    wait_for(lambda: (root / "store/registries.toml").exists(), "signed fixture registry", 90)
    assert registry.poll() is None
    with (root / "install.log").open("wb") as log:
        subprocess.run([str(args.debug_bin / "app"), "install", "so.tilo.file-explorer"],
                       env=env, stdout=log, stderr=log, check=True, timeout=120)
    shell_log = stack.enter_context((root / "shell.log").open("wb"))
    command = [str(args.debug_bin / "shell"), "--backend", "headless"]
    shell = processes.spawn(command, env, shell_log)
    socket = root / "run/control.sock"
    wait_for(socket.exists, "private compositor socket", 70)
    desktop = Desktop(socket)
    provision = desktop.request("provision_test_identity")["answer"]
    assert provision["created"] and provision["throwaway"]
    local = desktop.request("state_dump")["state"]["chrome"]["local_user_id"]
    processes.stop(shell)
    # Discard only the pre-identity throwaway workspace. The real compositor
    # bootstraps its new workspace from the certified owner on restart.
    (root / "config/tilo/state.json").unlink(missing_ok=True)
    processes.spawn(command, env, shell_log)
    def restarted():
        try:
            return desktop.request("state_dump")["state"]["chrome"]["local_user_id"] == local
        except (OSError, RuntimeError):
            return False
    wait_for(restarted, "certified isolated owner", 70)
    grant = desktop.request("grant", subject="app:so.tilo.file-explorer",
        scope="fs:" + str(root / "home"), verb="read", id="fixture-read-home")
    assert grant["answer"]["grantor"] == local
    files = wait_for(lambda: next((row for row in desktop.actions()
                     if row["id"] == "so.tilo.file-explorer"), None), "installed Files launch action")
    desktop.invoke(files)
    def frame():
        return next((row for row in desktop.request("foreign_frames")["foreign_frames"]["frames"]
                     if row.get("app_id") == "so.tilo.file-explorer" and row.get("presented")
                     and row.get("frame_seq", 0) > 0), None)
    presented = wait_for(frame, "real Files frame", 60)
    go = wait_for(lambda: desktop.action("go-to"), "Files folder action")
    desktop.invoke(go, {"path": str(root / "home")})
    wait_for(lambda: desktop.action("send-airdrop"), "granted file AirDrop capability")
    assert "Landlock enforcement" in (root / "shell.log").read_text() or "FullyEnforced" in (root / "shell.log").read_text()
    return desktop, presented


def journeys(desktop, fixture, destination):
    passed = []
    def record(name):
        passed.append(name)
        print("PASS", name, flush=True)
    with fixture.receiver() as (port, observed, _):
        sheet = desktop.start(port)
        assert fixture.pins[0] not in sheet["detail"] and observed == []
        desktop.enter_pin("00" * 32)
        wait_for(lambda: desktop.dialog() and desktop.dialog().get("error"), "wrong-pin error")
        assert observed == []
        desktop.enter_pin(fixture.pins[0])
        wait_for(lambda: (destination / "approved.txt").is_file(), "upstream extracted bytes")
        assert (destination / "approved.txt").read_bytes() == PAYLOAD
        assert observed == ["/Discover", "/Ask", "/Upload"]
        wait_for(lambda: desktop.dialog() is None, "consent withdrawal")
        complete = desktop.transfer_phase(desktop.transfer, "completed")
        assert complete["transferred_bytes"] == len(PAYLOAD)
        record("native exact transfer, semantic completion progress and wrong-pin metadata exclusion")
    with fixture.receiver() as (port, observed, _):
        desktop.start(port)
        desktop.request("key", key="Escape")
        wait_for(lambda: desktop.dialog() is None, "cancelled consent withdrawal")
        time.sleep(0.5)
        assert observed == []
        cancelled = desktop.transfer_phase(desktop.transfer, "cancelled")
        assert cancelled["transferred_bytes"] == 0
        record("consent cancellation excludes metadata")
    held, release = threading.Event(), threading.Event()
    def hold_ask():
        held.set()
        assert release.wait(12), "fixture Ask must be released"
    before = {row["id"] for row in desktop.actions()}
    with fixture.receiver(on_ask=hold_ask) as (port, observed, _):
        try:
            desktop.start(port)
            desktop.enter_pin(fixture.pins[0])
            wait_for(held.is_set, "receiver Ask")
            sending = desktop.transfer_phase(desktop.transfer, "transferring")
            assert sending["transferred_bytes"] == 0, "held Ask has confirmed no payload bytes"
            cancel = wait_for(lambda: desktop.action("cancel", before), "public active transfer cancel")
            desktop.invoke(cancel)
            time.sleep(0.7)
        finally:
            release.set()
        time.sleep(0.5)
        assert observed == ["/Discover", "/Ask"] and desktop.dialog() is None
        desktop.transfer_phase(desktop.transfer, "cancelled")
        record("public transfer cancellation and semantic progress prevent upload")
    before = {row["id"] for row in desktop.actions()}
    with fixture.receiver("decline") as (port, observed, _):
        desktop.start(port)
        desktop.enter_pin(fixture.pins[0])
        retry = wait_for(lambda: desktop.action("retry", before), "declined transfer retry resource")
        desktop.invoke(retry)
        time.sleep(0.7)
        assert observed == ["/Discover", "/Ask"] and desktop.dialog() is None
        failed = desktop.transfer_phase(desktop.transfer, "failed")
        assert failed["transferred_bytes"] == 0
        record("receiver decline and generic retry do not reuse consent")
    before = {row["id"] for row in desktop.actions()}
    with fixture.receiver() as (port, observed, rotation):
        desktop.start(port)
        rotation["active"] = True
        desktop.enter_pin(fixture.pins[0])
        wait_for(lambda: desktop.action("retry", before), "changed-certificate failure")
        assert observed == []
        desktop.transfer_phase(desktop.transfer, "failed")
        record("certificate rotation excludes metadata")
    (destination / "approved.txt").unlink()
    with fixture.receiver() as (port, observed, _):
        desktop.start(port)
        assert observed == []
        desktop.enter_pin(fixture.pins[0])
        wait_for(lambda: (destination / "approved.txt").is_file(), "recovered transfer")
        assert (destination / "approved.txt").read_bytes() == PAYLOAD
        assert observed == ["/Discover", "/Ask", "/Upload"]
        recovered = desktop.transfer_phase(desktop.transfer, "completed")
        assert recovered["transferred_bytes"] == len(PAYLOAD)
        record("fresh-consent failure recovery delivers exact bytes")
    return passed


def file_sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    if not __debug__:
        raise RuntimeError("proof requires enabled Python assertions")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-commit", required=True, help="source commit recorded outside the remote source snapshot")
    parser.add_argument("--opendrop", type=Path, required=True)
    parser.add_argument("--runtime-bundle", type=Path, required=True)
    parser.add_argument("--libstd", type=Path, required=True, help="exact thin build toolchain's libstd .so")
    parser.add_argument("--debug-bin", type=Path, default=REPO / "target/debug")
    parser.add_argument("--thin-dir", type=Path, default=REPO / "target/thin-app")
    parser.add_argument("--evidence", type=Path, required=True, help="JSON report outside scratch profile")
    args = parser.parse_args()
    if len(args.source_commit) != 40 or any(c not in "0123456789abcdef" for c in args.source_commit):
        parser.error("source-commit must be a full commit hash")
    for name in ("opendrop", "runtime_bundle", "libstd", "debug_bin", "thin_dir", "evidence"):
        setattr(args, name, getattr(args, name).resolve())
    for path in (args.runtime_bundle, args.libstd, args.thin_dir / "file-explorer",
                 args.thin_dir / "libruntime_dylib.so", args.debug_bin / "shell", args.debug_bin / "app",
                 args.debug_bin / "tilo-install", args.debug_bin / "tilo-launcher",
                 args.debug_bin / "examples/serve-test-registry"):
        if not path.is_file():
            parser.error(f"missing prebuilt prerequisite: {path}")
    revision = subprocess.check_output(["git", "-C", str(args.opendrop), "rev-parse", "HEAD"], text=True).strip()
    if revision != REVISION:
        parser.error("OpenDrop revision does not match the pinned oracle")
    subprocess.run(["git", "-C", str(args.opendrop), "diff", "--quiet", "HEAD"], check=True)
    if subprocess.check_output(["git", "-C", str(args.opendrop), "status", "--porcelain", "--untracked-files=normal"], text=True).strip():
        parser.error("OpenDrop checkout contains extra or modified files")
    sys.path.insert(0, str(args.opendrop))
    spec = importlib.util.spec_from_file_location("opendrop_oracle", REPO / "tools/ci/test_airdrop_sender.py")
    oracle = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(oracle)
    # Validate dependencies before creating any native processes.
    import opendrop.server  # noqa: F401
    with tempfile.TemporaryDirectory(prefix="tilo-airdrop-native-") as temporary, contextlib.ExitStack() as stack:
        root = Path(temporary)
        root.chmod(0o700)
        processes = Processes()
        # Close processes before logs and profile, on success and every failure.
        desktop, frame = None, None
        try:
            desktop, frame = boot_files(root, args, processes, stack)
            oracle.AirDropTests.setUpClass()
            try:
                destination = root / "received"
                destination.mkdir()
                previous = Path.cwd()
                os.chdir(destination)
                try:
                    passed = journeys(desktop, oracle.AirDropTests(), destination)
                finally:
                    os.chdir(previous)
            finally:
                oracle.AirDropTests.tearDownClass()
            report = {"schema": 1, "revision": args.source_commit,
                "artifact_sha256": {name: file_sha256(args.debug_bin / name)
                    for name in ("shell", "app", "tilo-install", "tilo-launcher")},
                "thin_files_sha256": file_sha256(args.thin_dir / "file-explorer"),
                "runtime_sha256": file_sha256(args.thin_dir / "libruntime_dylib.so"),
                "libstd_sha256": file_sha256(args.libstd),
                "opendrop_revision": REVISION, "journeys": passed, "files_frame": frame,
                "bytes": len(PAYLOAD), "sha256": hashlib.sha256(PAYLOAD).hexdigest(),
                "physical_receiver": False, "release_availability": False}
            args.evidence.parent.mkdir(parents=True, exist_ok=True)
            args.evidence.write_text(json.dumps(report, indent=2) + "\n")
        except BaseException:
            for name in ("runtime.log", "install.log", "registry.log", "shell.log"):
                path = root / name
                if path.exists():
                    print(f"{name}:\n{path.read_text(errors='replace')[-4000:]}", file=sys.stderr)
            raise
        finally:
            processes.close()


if __name__ == "__main__":
    main()
