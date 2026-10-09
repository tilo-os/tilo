#!/usr/bin/env python3
"""Developer proof: signed native Git public action -> approved ush fixture.
No model evaluation; use only the previously qualified fixed confinement image.
"""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location("packaged_native_bootstrap", HERE / "packaged-native-bootstrap.py")
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)
from _control import call
from reaper import die_with_parent


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def private_json(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def image(fixture):
    inventory_path = fixture / "image-inventory.json"
    inventory = json.loads(inventory_path.read_text())
    expected = set()
    if any(path.is_symlink() for path in (fixture / "image").rglob("*")):
        raise RuntimeError("qualified fixture image contains a symbolic link")
    for entry in inventory["files"]:
        relative = Path(entry["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError("unsafe fixture inventory path")
        path = fixture / "image" / relative
        if path.is_symlink() or not path.is_file() or path.stat().st_size != entry["bytes"] or digest(path) != entry["sha256"]:
            raise RuntimeError("prior qualified fixture image no longer matches its inventory")
        expected.add(relative.as_posix())
    observed = {path.relative_to(fixture / "image").as_posix() for path in (fixture / "image").rglob("*") if path.is_file()}
    if observed != expected:
        raise RuntimeError("qualified fixture image contains unrecorded files")
    return {"identity": "sha256:" + digest(inventory_path), "root": str((fixture / "image").resolve())}


def package(binaries, profile, env):
    publisher = profile / "publisher.json"
    bootstrap.run(binaries / "tilo-app-package", ["keygen", "--out", publisher], env)
    tapp = profile / "git-client.tapp"
    # Compilation uses the caller's ordinary build configuration and cache.
    # Session apps, installation and the compositor keep their private profile.
    build_env = {**env,
        "XDG_CONFIG_HOME": os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")),
        "XDG_CACHE_HOME": os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))}
    bootstrap.run(binaries / "tilo-app", ["package", bootstrap.REPO / "apps/git-client", "--output", tapp, "--identity", publisher, "--profile", "thin-app"], build_env)
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    triple = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    apps = binaries.parent / triple / "thin-app"
    rust_lib = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    libraries = bootstrap.shared_libraries([apps / "git-client"], f"{apps}:{apps / 'deps'}:{rust_lib}")
    args = ["build", "--version", "2026.07", "--out", profile / "runtimes/2026.07", "--identity", publisher]
    for library in libraries:
        args.extend(["--include", library])
    bootstrap.run(binaries / "tilo-runtime-bundle", args, env)
    public = subprocess.check_output([str(binaries / "tilo-runtime-bundle"), "key", "pubkey", "--identity", str(publisher)], env=env, text=True).strip()
    pin = profile / "runtime-publishers.txt"
    pin.write_text(public + "\n")
    env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(pin)
    bootstrap.run(binaries / "tilo-install", ["install", tapp, "--allow-self-signed"], env)
    return tapp


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def source_snapshot():
    root = HERE.parents[1]
    paths = (
        "crates/sdk/ui-core/src/collaboration.rs",
        "crates/sdk/ui-core/src/collaboration/broker.rs",
        "crates/sdk/ui-core/src/app/actions.rs",
        "crates/sdk/ui-core/src/app/document_routes.rs",
        "crates/collab/tilo-action-broker/src/collab_broker_capnp/approved_checks.rs",
        "crates/collab/tilo-action-broker/src/collab_broker_capnp/published_invocation.rs",
        "crates/session/shell/src/shell/broker_services/approved_checks.rs",
    )
    return {path: digest(root / path) for path in paths}


def run(args):
    output = args.out.resolve()
    output.mkdir(mode=0o700)
    observed_sources = source_snapshot()
    private_json(output / "proof-source-at-start.json", {
        "sha256": digest(Path(__file__)), "observed_source_hashes": observed_sources,
        "compiler_input_attestation": False,
    })
    verified_image = image(args.supervision_fixture.resolve())
    # Keep the actual manager transport: FullyEnforced checks must be observable.
    runtime = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    if not (runtime / "systemd/private").exists():
        raise RuntimeError("actual user manager transport is unavailable")
    env = dict(os.environ)
    env["RUST_LOG"] = "warn,tilo_ui::collaboration=debug,tilo_ui::app::document_routes=debug"
    profile = output / "profile"
    profile.mkdir(mode=0o700)
    for name in ("config", "data", "state", "cache", "identity"):
        (profile / name).mkdir(mode=0o700)
    env.update({"XDG_CONFIG_HOME": str(profile / "config"), "XDG_DATA_HOME": str(profile / "data"), "XDG_STATE_HOME": str(profile / "state"), "XDG_CACHE_HOME": str(profile / "cache"), "TILO_IDENTITY_ROOT": str(profile / "identity")})
    private_json(output / "profile-location.json", {"path": str(profile), "developer_only": True})
    env.update({"XDG_RUNTIME_DIR": str(runtime), "TILO_APPS_DATA_DIR": str(profile / "apps"), "TILO_RUNTIME_BUNDLE_DIR": str(profile / "runtimes"), "TILO_LAUNCHER_BIN": str(args.binaries / "tilo-launcher"), "TILO_STATE_PATH": str(profile / "config/tilo/state.json"), "TILO_HEADLESS_SIZE": "1024x768"})
    for variable in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_APPS_SYSTEM_DIR"):
        env.pop(variable, None)
    ordinary_cargo = shutil.which(os.environ.get("CARGO", "cargo"))
    if ordinary_cargo is None:
        raise RuntimeError("ordinary Cargo entry point unavailable")
    env["CARGO"] = ordinary_cargo
    private_json(output / "build-routing.json", {"cargo": ordinary_cargo, "build_configuration": "caller configuration", "session_profile_private": True, "compiler_input_attestation": False})
    # The startup registry is immutable: its executable must not race another
    # build replacing the shared worktree output after it is captured.
    (output / "bin").mkdir(mode=0o700)
    runtime_binary = output / "bin/tilo-ush"
    shutil.copyfile(args.binaries / "tilo-ush", runtime_binary)
    runtime_binary.chmod(0o700)
    runtime_sha256 = digest(runtime_binary)
    package_path = package(args.binaries, profile, env)
    if source_snapshot() != observed_sources:
        raise RuntimeError("observed qualification source changed during the signed app build")
    private_json(output / "inputs.json", {"script_sha256": digest(Path(__file__)), "signed_git_package_sha256": digest(package_path), "shell_sha256": digest(args.binaries / "shell"), "ush_sha256": runtime_sha256, "verified_fixture_image": verified_image, "no_model_evaluation": True})
    processes, logs = [], []
    with tempfile.TemporaryDirectory(prefix="tilo-approved-check-", dir=runtime) as control_dir:
        socket = str(Path(control_dir) / "control.sock")
        env["TILO_TEST_CONTROL"] = socket
        def spawn(binary, arguments, name):
            log = (output / name).open("x")
            logs.append(log)
            process = subprocess.Popen([str(binary), *map(str, arguments)], env=env, stdout=log, stderr=log, start_new_session=True, preexec_fn=die_with_parent())
            processes.append(process)
            return process
        try:
            invitation_path = profile / "native-invitation.json"
            governor = spawn(args.binaries / "examples/packaged_native_governor", [profile, invitation_path], "governor.log")
            bootstrap.wait_for("actual signed fixture invitation", invitation_path.exists, governor)
            invitation = json.loads(invitation_path.read_text())
            shell = spawn(args.binaries / "shell", ["--backend", "headless", "--enable-peer-sync"], "enrollment-shell.log")
            bootstrap.wait_for("isolated compositor control", lambda: Path(socket).exists(), shell)
            if not call(socket, {"op": "provision_test_identity"}).get("answer", {}).get("throwaway"):
                raise RuntimeError("scratch identity provisioning refused")
            enrolled = bootstrap.enroll_native(socket, invitation, shell, args.binaries, env)
            private_json(output / "enrollment.json", enrolled)
            stop(shell)
            state = json.loads(Path(env["TILO_STATE_PATH"]).read_text())
            room = enrolled["room"]
            saved = state["native_authority"]["rooms"][room]
            host = (saved.get("host") or saved["governor"])["device_public_key"]
            if host != saved["device"]["device_public_key"]:
                raise RuntimeError("native fixture host differs from actual local enrolled device")
            source = output / "source"
            (source / "target").mkdir(parents=True)
            (source / "readonly").write_text("approved native supervision fixture\n")
            for name in ("build", "scratch", "artifacts"):
                (output / name).mkdir(mode=0o700)
            check = {"id": "fixture-pass", "binding": {"workspace": room, "host": host, "record": "derived:default@tab:0000000000000000", "resource": "git_client.repo"}, "checkout": str(source), "build": str(output / "build"), "scratch": str(output / "scratch"), "runtime_binary": str(runtime_binary), "images": [verified_image], "command": ["/usr/bin/check-fixture", "pass"], "environment": [], "limits": {"cpu_percent": 100, "memory_bytes": 134217728, "processes": 32, "wall_seconds": 15, "output_bytes": 4096}}
            config = output / "checks.json"
            private_json(config, {"version": 1, "projects": [{"app_id": "so.tilo.git-client", "resource_identity": {"kind": "git_client.repo", "key": "git_client.repo"}, "artifacts": str(output / "artifacts"), "checks": [check]}]})
            Path(socket).unlink(missing_ok=True)
            shell = spawn(args.binaries / "shell", ["--backend", "headless", "--enable-peer-sync", "--approved-checks", config], "qualified-shell.log")
            bootstrap.wait_for("qualified compositor control", lambda: Path(socket).exists(), shell)
            git = bootstrap.wait_for("signed Git Applications Open", lambda: next((row for row in bootstrap.actions(socket, "Git") if row.get("provider") == "applications" and row.get("id") == "so.tilo.git-client"), None), shell)
            opened = bootstrap.invoke(socket, git)
            bootstrap.wait_terminal(socket, opened, shell, public_failure_path=output / "failed-open.json")
            def offered():
                matches = [row for row in bootstrap.actions(socket) if row.get("application") == "so.tilo.git-client" and row.get("room") == room and row.get("record") == check["binding"]["record"] and row.get("operation") == "run-approved-check"]
                if len(matches) > 1:
                    raise RuntimeError("ambiguous actual Git action target")
                return matches[0] if matches else None
            action = bootstrap.wait_for("actual native Git approved-check action", offered, shell)
            private_json(output / "offered-action.json", bootstrap.public_action_observation(action))
            accepted = bootstrap.invoke(socket, action, {"check_id": check["id"]})
            private_json(output / "accepted.json", accepted)
            terminal = bootstrap.wait_terminal(socket, accepted, shell, public_failure_path=output / "failed-check.json")
            private_json(output / "terminal.json", terminal)
            receipt = terminal["update"]["output"]["value"]
            if receipt["binding"] != {"app_id": "so.tilo.git-client", **check["binding"], "resource": action["resource"]} or receipt["outcome"] != "exited" or receipt["exit_code"] != 0 or receipt["cleanup_observed"] is not True:
                raise RuntimeError("actual check receipt lacks exact binding and observed guest completion")
            if receipt["runtime_binary_sha256"] != runtime_sha256:
                raise RuntimeError("actual runtime executable differs from immutable captured input")
            artifact = output / "artifacts" / (receipt["artifact_sha256"] + ".json")
            if digest(artifact) != receipt["artifact_sha256"]:
                raise RuntimeError("native portable receipt does not identify actual private supervision artifact")
            private_artifact = json.loads(artifact.read_text())
            if private_artifact["resource_identity"] != {"kind": "git_client.repo", "key": "git_client.repo"} or private_artifact["accepted_opaque_resource"] != action["resource"] or private_artifact["observed"]["binding"] != check["binding"]:
                raise RuntimeError("private receipt does not bind fixed semantic project and actual opaque target")
            private_json(output / "qualification.json", {"kind": "signed-native-Git-public-action-supervision", "config_sha256": digest(config), "artifact_sha256": digest(artifact), "invocation": receipt["invocation"], "original_public_action_invoked": True, "cleanup_observed": True, "model_evaluation": False, "coding_quality_measured": False})
        finally:
            for process in reversed(processes):
                stop(process)
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--supervision-fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True, help="new private directory")
    arguments = parser.parse_args()
    arguments.binaries = arguments.binaries.resolve()
    try:
        run(arguments)
    except Exception as error:
        if arguments.out.exists() and not (arguments.out / "failure.json").exists():
            private_json(arguments.out / "failure.json", {"qualified": False, "reason": str(error), "model_evaluation": False})
        raise
