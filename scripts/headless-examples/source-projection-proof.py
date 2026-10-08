#!/usr/bin/env python3
"""Controlled native Code edit -> Git approved check -> fresh Rust compilation.
No model, hidden coding task or quality score. Uses an operator-reviewed offline image.
"""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from _control import call
from reaper import die_with_parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


editor = module("editor_text_proof", "code-editor-text-proof.py")
bootstrap = editor.bootstrap
private_json = editor.private_json
digest = editor.digest
stop = editor.stop
INITIAL = 'compile_error!("generic source before edit");\n'
CHANGED = 'pub const READY: bool = true;\n'
RUSTC = "/usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin/rustc"
BINARY_NAMES = ("shell", "tilo-control", "tilo-launcher", "tilo-install", "tilo-app",
                "tilo-app-package", "tilo-runtime-bundle", "tilo-ush",
                "examples/packaged_native_governor", "examples/packaged_native_bootstrap")
SOURCE_PATHS = (
    "crates/collab/tilo-action-broker/src/collab_broker_capnp.rs",
    "crates/collab/tilo-action-broker/src/collab_broker_capnp/approved_checks.rs",
    "crates/collab/tilo-action-broker/src/collab_broker_capnp/published_invocation.rs",
    "crates/collab/tilo-action-broker/src/collab_broker_capnp/approved_checks/source_projection.rs",
    "crates/collab/tilo-action-broker/src/enrolled_tasks/source_projection.rs",
    "crates/runtime/runtime-collab/src/approved_checks.rs",
    "crates/session/shell/src/shell/broker_services/approved_checks.rs",
    "crates/session/shell/src/shell/broker_services/approved_checks/source_projection.rs",
    "crates/session/shell/src/shell/broker_services/approved_checks/source_projection/filesystem.rs",
    "crates/session/shell/src/shell/broker_services/approved_checks/source_projection/snapshots.rs",
    "scripts/headless-examples/source-projection-proof.py",
    "scripts/headless-examples/code-editor-text-proof.py",
    "scripts/headless-examples/packaged-native-bootstrap.py",
)


def sources():
    return {**editor.source_snapshot(), **{p: digest(bootstrap.REPO / p) for p in SOURCE_PATHS}}


def verify_image(directory):
    inventory_path = directory / "image-inventory.json"
    inventory = json.loads(inventory_path.read_text())
    entries = inventory.get("files")
    if not isinstance(entries, list) or len(entries) > 300000:
        raise RuntimeError("bounded reviewed image inventory unavailable")
    image = directory / "image"
    expected = set()
    for entry in entries:
        relative = Path(entry["path"])
        if relative.is_absolute() or any(part in (".", "..") for part in relative.parts) or relative.as_posix() in expected:
            raise RuntimeError("ambiguous image inventory path")
        expected.add(relative.as_posix())
        path = image / relative
        if entry["kind"] == "symlink":
            if not path.is_symlink() or os.readlink(path) != entry["target"]:
                raise RuntimeError("reviewed image link changed")
        elif entry["kind"] == "file":
            if path.is_symlink() or not path.is_file() or path.stat().st_size != entry["bytes"] or digest(path) != entry["sha256"] or bool(path.stat().st_mode & 0o100) != entry["executable"]:
                raise RuntimeError("reviewed image file changed")
        else:
            raise RuntimeError("unsupported image inventory entry")
    observed = {p.relative_to(image).as_posix() for p in image.rglob("*") if p.is_symlink() or p.is_file()}
    if observed != expected or not (image / RUSTC.lstrip("/")).is_file():
        raise RuntimeError("image closure or actual compiler unavailable")
    return {"identity": "sha256:" + digest(inventory_path), "root": str(image.resolve())}


def source_identity(root):
    entries = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise RuntimeError("source fixture contains a link")
        if path.is_file():
            key = path.relative_to(root).as_posix()
            if key.startswith("target/") or ".git" in path.relative_to(root).parts:
                raise RuntimeError("source contains history/build output")
            entries[key] = {"bytes": path.stat().st_size, "sha256": digest(path), "executable": bool(path.stat().st_mode & 0o100)}
    encoded = json.dumps(entries, ensure_ascii=False, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest(), entries


def package_pair(binaries, profile, env):
    publisher = profile / "publisher.json"
    bootstrap.run(binaries / "tilo-app-package", ["keygen", "--out", publisher], env)
    build_env = dict(env)
    for variable in ("XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
        if variable in os.environ:
            build_env[variable] = os.environ[variable]
        else:
            build_env.pop(variable, None)
    packages = []
    for name in ("code-editor", "git-client"):
        path = profile / f"{name}.tapp"
        bootstrap.run(binaries / "tilo-app", ["package", bootstrap.REPO / "apps" / name, "--output", path, "--identity", publisher, "--profile", "thin-app"], build_env)
        packages.append(path)
    triple = next(line.removeprefix("host: ") for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    apps = binaries.parent / triple / "thin-app"
    rust_lib = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    libraries = bootstrap.shared_libraries([apps / "code-editor", apps / "git-client"], f"{apps}:{apps / 'deps'}:{rust_lib}")
    args = ["build", "--version", "2026.07", "--out", profile / "runtimes/2026.07", "--identity", publisher]
    for library in libraries:
        args.extend(["--include", library])
    bootstrap.run(binaries / "tilo-runtime-bundle", args, env)
    public = subprocess.check_output([str(binaries / "tilo-runtime-bundle"), "key", "pubkey", "--identity", str(publisher)], env=env, text=True).strip()
    pin = profile / "runtime-publishers.txt"
    pin.write_text(public + "\n")
    env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(pin)
    for path in packages:
        bootstrap.run(binaries / "tilo-install", ["install", path, "--allow-self-signed"], env)
    return {path.name: digest(path) for path in packages}


def prepare(args):
    output = args.out.resolve()
    output.mkdir(mode=0o700)
    env = dict(os.environ)
    runtime = Path(env.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    if not (runtime / "systemd/private").exists():
        raise RuntimeError("actual user manager unavailable")
    profile = output / "profile"
    profile.mkdir(mode=0o700)
    for name in ("config", "data", "state", "cache", "identity"):
        (profile / name).mkdir(mode=0o700)
    env.update({"XDG_CONFIG_HOME": str(profile / "config"), "XDG_DATA_HOME": str(profile / "data"), "XDG_STATE_HOME": str(profile / "state"), "XDG_CACHE_HOME": str(profile / "cache"), "TILO_IDENTITY_ROOT": str(profile / "identity"), "XDG_RUNTIME_DIR": str(runtime), "TILO_APPS_DATA_DIR": str(profile / "apps"), "TILO_RUNTIME_BUNDLE_DIR": str(profile / "runtimes"), "TILO_LAUNCHER_BIN": str(args.binaries / "tilo-launcher"), "TILO_STATE_PATH": str(profile / "config/tilo/state.json"), "TILO_HEADLESS_SIZE": "1024x768"})
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_APPS_SYSTEM_DIR"):
        env.pop(name, None)
    ordinary_cargo = shutil.which(os.environ.get("CARGO", "cargo"))
    if ordinary_cargo is None:
        raise RuntimeError("ordinary Cargo entry point unavailable")
    env["CARGO"] = ordinary_cargo
    observed = sources()
    private_json(output / "source-at-start.json", {"hashes": observed, "compiler_input_attestation": False})
    image = verify_image(args.image.resolve())
    packages = package_pair(args.binaries, profile, env)
    if sources() != observed:
        raise RuntimeError("source changed during packaging")
    (output / "bin").mkdir(mode=0o700)
    shutil.copyfile(args.binaries / "tilo-ush", output / "bin/tilo-ush")
    (output / "bin/tilo-ush").chmod(0o700)
    inputs = {"binaries": {name: digest(args.binaries / name) for name in BINARY_NAMES}, "packages": packages, "runtime": {str(p.relative_to(profile / 'runtimes')): digest(p) for p in (profile / "runtimes").rglob("*") if p.is_file()}, "image": image, "ush_copy": digest(output / "bin/tilo-ush"), "model_evaluated": False}
    private_json(output / "inputs.json", inputs)
    fonts = output / "font-catalog"
    subprocess.run([sys.executable, "-B", str(bootstrap.REPO / "tools/font-catalog.py"), "prepare", str(bootstrap.REPO / "crates/sdk/ui-core/assets/fonts"), str(fonts), "--scope", "os"], check=True)
    private_json(output / "font-namespace.json", {"manifest_sha256": digest(fonts / "manifest.toml")})
    subprocess.run(["bwrap", "--die-with-parent", "--bind", "/", "/", "--dev-bind", "/dev", "/dev", "--tmpfs", "/usr/share/fonts", "--ro-bind", str(fonts), "/usr/share/fonts/tilo", "--", sys.executable, "-B", str(Path(__file__).resolve()), "--binaries", str(args.binaries), "--image", str(args.image), "--out", str(output), "--runtime-only"], env=env, check=True)


def native(args):
    output = args.out.resolve()
    inputs = editor.read_phase_receipt(output / "inputs.json")
    if verify_image(args.image.resolve()) != inputs["image"]:
        raise RuntimeError("reviewed image drift across package/runtime boundary")
    if editor.read_phase_receipt(output / "source-at-start.json")["hashes"] != sources() or inputs["binaries"] != {name: digest(args.binaries / name) for name in BINARY_NAMES}:
        raise RuntimeError("source/binary drift across package boundary")
    profile = output / "profile"
    if inputs["packages"] != {name: digest(profile / name) for name in inputs["packages"]} or inputs["runtime"] != {str(p.relative_to(profile / 'runtimes')): digest(p) for p in (profile / "runtimes").rglob("*") if p.is_file()} or digest(output / "bin/tilo-ush") != inputs["ush_copy"]:
        raise RuntimeError("signed artifacts/runtime drift")
    fonts = editor.read_phase_receipt(output / "font-namespace.json")
    if digest(Path("/usr/share/fonts/tilo/manifest.toml")) != fonts["manifest_sha256"] or not any(row.split()[4] == "/usr/share/fonts/tilo" and "ro" in row.split()[5].split(",") for row in Path("/proc/self/mountinfo").read_text().splitlines()):
        raise RuntimeError("actual read-only canonical font mount unavailable")
    subprocess.run([sys.executable, "-B", str(bootstrap.REPO / "tools/font-catalog.py"), "verify", "/usr/share/fonts/tilo", "--scope", "os"], check=True)
    env = dict(os.environ)
    runtime = Path(env["XDG_RUNTIME_DIR"])
    endpoints = [runtime / "tilo/services" / name for name in ("identity.sock", "model-gateway.sock")]
    if any(p.exists() or p.is_symlink() for p in endpoints):
        raise RuntimeError("foreign native service endpoint already present")
    processes, logs, owned_sockets = [], [], {}
    qualification = None
    with tempfile.TemporaryDirectory(prefix="tilo-source-projection-", dir=runtime) as control:
        socket = str(Path(control) / "control.sock")
        env["TILO_TEST_CONTROL"] = socket
        def spawn(binary, arguments, name):
            log = (output / name).open("x")
            logs.append(log)
            child = subprocess.Popen([str(binary), *map(str, arguments)], env=env, stdout=log, stderr=log, start_new_session=True, preexec_fn=die_with_parent())
            processes.append(child)
            return child
        try:
            def observe_owned_endpoints():
                # Initial admission refused every pre-existing endpoint. Retain
                # identities during this owned phase; never adopt a replacement
                # during cleanup.
                for path in endpoints:
                    if path.exists() and path.is_socket():
                        owned_sockets.setdefault(str(path), path.stat().st_ino)
            invitation_path = profile / "native-invitation.json"
            governor = spawn(args.binaries / "examples/packaged_native_governor", [profile, invitation_path], "governor.log")
            bootstrap.wait_for("actual signed invitation", invitation_path.exists, governor)
            invitation = json.loads(invitation_path.read_text())
            shell = spawn(args.binaries / "shell", ["--backend", "headless", "--enable-peer-sync"], "enrollment.log")
            bootstrap.wait_for("isolated compositor", lambda: Path(socket).exists(), shell)
            if not call(socket, {"op": "provision_test_identity"}).get("answer", {}).get("throwaway"):
                raise RuntimeError("isolated fixture identity refused")
            enrolled = bootstrap.enroll_native(socket, invitation, shell, args.binaries, env)
            private_json(output / "enrollment.json", enrolled)
            observe_owned_endpoints()
            room = enrolled["room"]
            state = json.loads(Path(env["TILO_STATE_PATH"]).read_text())
            saved = state["native_authority"]["rooms"][room]
            host = (saved.get("host") or saved["governor"])["device_public_key"]
            if host != saved["device"]["device_public_key"] or not all(c.isalnum() or c in "_-" for c in room):
                raise RuntimeError("actual local native host/room differs")
            directory = profile / "data/tilo-rooms" / room / "so.tilo.code-editor"
            directory.mkdir(mode=0o700, parents=True)
            source_file = directory / "lib.rs"
            source_file.write_text(INITIAL, encoding="utf-8")
            source_file.chmod(0o600)
            def offered(operation, app_id=None, resource=None):
                rows = [r for r in bootstrap.actions(socket) if r.get("room") == room and r.get("operation") == operation and (app_id is None or r.get("application") == app_id) and (resource is None or r.get("resource") == resource)]
                if len(rows) > 1:
                    raise RuntimeError("actual action target ambiguous")
                return rows[0] if rows else None
            def perform(action, arguments, name):
                private_json(output / (name + "-action.json"), bootstrap.public_action_observation(action))
                accepted = bootstrap.invoke(socket, action, arguments)
                private_json(output / (name + "-accepted.json"), accepted)
                terminal = bootstrap.wait_terminal(socket, accepted, shell, public_failure_path=output / (name + "-failure.json"))
                private_json(output / (name + "-terminal.json"), terminal)
                return terminal["update"]["output"].get("value")
            def open_app(app_id, name):
                action = bootstrap.wait_for("normal signed Applications Open", lambda: next((r for r in bootstrap.actions(socket) if r.get("provider") == "applications" and r.get("id") == app_id), None), shell)
                perform(action, {}, name)
            open_app("so.tilo.code-editor", "open-code")
            action = bootstrap.wait_for("owner OpenFile", lambda: offered("open-file", "so.tilo.code-editor"), shell)
            arguments = {"path": "/run/tilo/app/lib.rs"}
            names = {p["name"] for p in action.get("params", [])}
            confirmation = action.get("contract", {}).get("confirmation", {})
            if "confirm" in names:
                arguments["confirm"] = True
            elif "confirmation_phrase" in names:
                phrase = confirmation.get("phrase")
                if not isinstance(phrase, str) or not phrase:
                    raise RuntimeError("advertised confirmation phrase absent")
                arguments["confirmation_phrase"] = phrase
            elif confirmation.get("mode") != "none":
                raise RuntimeError("advertised required confirmation parameter absent")
            perform(action, arguments, "open-file")
            snapshot_action = bootstrap.wait_for("actual committed file source", lambda: offered("read-text-snapshot", "so.tilo.code-editor"), shell)
            snapshot = perform(snapshot_action, {"start_chars": 0, "max_chars": 4096}, "preloaded-source")
            if snapshot["content"]["text"] != INITIAL or snapshot["record"]["recordId"] != snapshot_action["record"] or not snapshot["basis"]["heads"]:
                raise RuntimeError("actual owner-preloaded source/record/heads differ")
            open_app("so.tilo.git-client", "open-git")
            check_action = bootstrap.wait_for("actual Git check target", lambda: offered("run-approved-check", "so.tilo.git-client"), shell)
            private_json(output / "preload-binding.json", {"source": snapshot_action, "snapshot": snapshot, "check": bootstrap.public_action_observation(check_action)})
            observe_owned_endpoints()
            stop(shell)
            baseline = output / "baseline"
            (baseline / "target").mkdir(parents=True)
            (baseline / "lib.rs").write_text(INITIAL, encoding="utf-8")
            (baseline / "lib.rs").chmod(0o600)
            baseline_sha, _ = source_identity(baseline)
            for name in ("build", "scratch", "artifacts"):
                (output / name).mkdir(mode=0o700)
            check = {"id": "compile-generic-source", "binding": {"workspace": room, "host": host, "record": check_action["record"], "resource": check_action["semantic_identity"]["key"]}, "checkout": str(baseline), "build": str(output / "build"), "scratch": str(output / "scratch"), "runtime_binary": str(output / "bin/tilo-ush"), "images": [inputs["image"]], "command": [RUSTC, "--crate-type", "lib", "--emit", "metadata", "/project/lib.rs", "--out-dir", "/project/target"], "environment": [], "limits": {"cpu_percent": 100, "memory_bytes": 1073741824, "processes": 64, "wall_seconds": 120, "output_bytes": 32768}}
            source_binding = {"app_id": snapshot_action["application"], "workspace": snapshot_action["room"], "record": snapshot_action["record"], "resource_identity": snapshot_action["semantic_identity"], "action": "read-text-snapshot"}
            config = output / "checks.json"
            private_json(config, {"version": 1, "projects": [{"app_id": check_action["application"], "resource_identity": check_action["semantic_identity"], "artifacts": str(output / "artifacts"), "checks": [check], "source_projections": [{"check_id": check["id"], "baseline_sha256": baseline_sha, "sources": [{"path": "lib.rs", "source": source_binding, "file_id": snapshot["content"]["file"]}]}]}]})
            Path(socket).unlink(missing_ok=True)
            shell = spawn(args.binaries / "shell", ["--backend", "headless", "--enable-peer-sync", "--approved-checks", config], "projected-shell.log")
            bootstrap.wait_for("projected compositor", lambda: Path(socket).exists(), shell)
            open_app("so.tilo.code-editor", "restore-code")
            restored_action = bootstrap.wait_for("restored actual source target", lambda: offered("read-text-snapshot", "so.tilo.code-editor"), shell)
            restored = perform(restored_action, {"start_chars": 0, "max_chars": 4096}, "restored-source")
            if {k: restored_action[k] for k in ("application", "room", "record", "semantic_identity")} != {k: snapshot_action[k] for k in ("application", "room", "record", "semantic_identity")} or restored["content"]["file"] != snapshot["content"]["file"] or restored["content"]["text"] != INITIAL:
                raise RuntimeError("restored source does not match immutable trusted mapping")
            open_app("so.tilo.git-client", "restore-git")
            current = bootstrap.wait_for("real check publication", lambda: offered("run-approved-check", "so.tilo.git-client"), shell)
            before = perform(current, {"check_id": check["id"]}, "baseline-compile")
            if before["outcome"] != "exited" or before["exit_code"] == 0 or not before["cleanup_observed"] or before.get("candidate_source_sha256") != baseline_sha or "generic source before edit" not in before["stderr"]["preview"]:
                raise RuntimeError("actual baseline compilation failure/source/cleanup not proven")
            edit_action = bootstrap.wait_for("contextual edit", lambda: offered("edit-text", "so.tilo.code-editor", restored_action["resource"]), shell)
            content = restored["content"]
            perform(edit_action, {"start_chars": 0, "delete_chars": len(INITIAL), "insert": CHANGED, "expected_base_digest": content["text_sha256"], "deleted_text": INITIAL, "before_context": "", "after_context": ""}, "contextual-edit")
            changed = perform(restored_action, {"start_chars": 0, "max_chars": 4096}, "changed-source")
            if changed["content"]["text"] != CHANGED or changed["basis"]["heads"] == restored["basis"]["heads"]:
                raise RuntimeError("actual CRDT edit/revision not observed")
            after = perform(current, {"check_id": check["id"]}, "candidate-compile")
            candidate = output / "artifacts" / ("candidate-" + after["invocation"]) / "source"
            candidate_sha, inventory = source_identity(candidate)
            if after["outcome"] != "exited" or after["exit_code"] != 0 or not after["cleanup_observed"] or after.get("candidate_source_sha256") != candidate_sha or candidate_sha == baseline_sha or (candidate / "lib.rs").read_text() != CHANGED:
                raise RuntimeError("fresh candidate compile/source/cleanup not proven")
            if source_identity(baseline)[0] != baseline_sha or source_file.read_text() != INITIAL or sources() != editor.read_phase_receipt(output / "source-at-start.json")["hashes"]:
                raise RuntimeError("baseline disk changed or observed source drifted")
            qualification = {"kind": "controlled-native-crdt-edit-to-fresh-approved-rust-compile", "source_sha256": candidate_sha, "candidate_inventory": inventory, "config_sha256": digest(config), "baseline_invocation": before["invocation"], "candidate_invocation": after["invocation"], "baseline_compile_failed": True, "fresh_candidate_compile_succeeded": True, "baseline_disk_unchanged": True, "model_executed": False, "coding_quality_measured": False, "compiler_input_attestation": False}
            observe_owned_endpoints()
        finally:
            for process in reversed(processes):
                stop(process)
            for log in logs:
                log.close()
            for path in endpoints:
                if path.exists() and path.is_socket() and owned_sockets.get(str(path)) == path.stat().st_ino:
                    path.unlink()
            cleanup = {"owned_processes_terminal": all(p.poll() is not None for p in processes), "native_service_endpoints_absent": all(not p.exists() and not p.is_symlink() for p in endpoints)}
            private_json(output / "cleanup.json", cleanup)
    if not all(cleanup.values()):
        raise RuntimeError("owned runtime cleanup not fully observed")
    if qualification is None:
        raise RuntimeError("native compilation proof never completed")
    private_json(output / "qualification.json", qualification)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True, help="reviewed offline compiler image/inventory directory")
    parser.add_argument("--out", type=Path, required=True, help="new private proof directory")
    parser.add_argument("--runtime-only", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.binaries = args.binaries.resolve()
    try:
        native(args) if args.runtime_only else prepare(args)
    except Exception as error:
        if args.out.exists() and not (args.out / "failure.json").exists():
            private_json(args.out / "failure.json", {"qualified": False, "reason": str(error), "model_executed": False})
        raise
