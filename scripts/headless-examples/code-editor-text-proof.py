#!/usr/bin/env python3
"""Developer proof: signed native Code Editor bounded text actions.
Generic UTF-8 fixture only; no model evaluation or grader content.
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
import tomllib

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


def package(binaries, profile, env):
    publisher = profile / "publisher.json"
    bootstrap.run(binaries / "tilo-app-package", ["keygen", "--out", publisher], env)
    tapp = profile / "code-editor.tapp"
    # Reuse the ordinary operator build configuration only for compilation;
    # Unix control sockets must fit the OS path bound. Session apps,
    # installation and the compositor retain their private XDG locations.
    build_env = dict(env)
    for variable in ("XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
        if variable in os.environ:
            build_env[variable] = os.environ[variable]
        else:
            build_env.pop(variable, None)
    bootstrap.run(binaries / "tilo-app", ["package", bootstrap.REPO / "apps/code-editor", "--output", tapp, "--identity", publisher, "--profile", "thin-app"], build_env)
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    triple = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    apps = binaries.parent / triple / "thin-app"
    rust_lib = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    libraries = bootstrap.shared_libraries([apps / "code-editor"], f"{apps}:{apps / 'deps'}:{rust_lib}")
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
        "apps/code-editor/src/actions.rs",
        "apps/code-editor/src/effects.rs",
        "apps/code-editor/src/lib.rs",
        "apps/code-editor/src/resource.rs",
        "apps/code-editor/src/app/surface.rs",
        "apps/code-editor/src/text_operations.rs",
        "apps/code-editor/src/text_operations_tests.rs",
        "crates/sdk/tilo-resource/src/resource.rs",
        "crates/sdk/tilo-resource/src/action.rs",
        "crates/sdk/tilo-resource/src/low_mutation.rs",
        "crates/sdk/ui-core/src/semantic_document_read.rs",
        "apps/code-editor/src/app.rs",
        "crates/sdk/ui-core/src/invocation_scope.rs",
        "crates/sdk/tilo-app/src/framework/dispatch.rs",
        "crates/sdk/ui-core/src/collaboration/broker.rs",
        "crates/sdk/ui-core/src/collaboration/committed_document.rs",
        "crates/sdk/ui-core/src/app/document_routes.rs",
        "crates/sdk/ui-core/src/app/actions.rs",
        "crates/sdk/ui-core/src/app/collaboration.rs",
        "crates/sdk/ui-core/src/collaboration.rs",
        "crates/session/shell/src/shell/certified_action_session.rs",
    )
    return {path: digest(root / path) for path in paths}


def run(args):
    output = args.out.resolve()
    if args.runtime_only:
        return resume_native(args, output)
    output.mkdir(mode=0o700)
    observed_sources = source_snapshot()
    private_json(output / "proof-source-at-start.json", {
        "sha256": digest(Path(__file__)), "observed_source_hashes": observed_sources,
        "compiler_input_attestation": False,
    })
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
    # The SDK packager honors CARGO. package() restores the ordinary build
    # environment for that subprocess without copying compiler configuration
    # into the isolated desktop profile.
    ordinary_cargo = shutil.which(os.environ.get("CARGO", "cargo"))
    if ordinary_cargo is None:
        raise RuntimeError("ordinary Cargo entry point unavailable")
    env["CARGO"] = ordinary_cargo
    private_json(output / "build-routing.json", {"cargo": ordinary_cargo, "private_runtime_config_copy": False, "packaging_uses_ordinary_xdg_build_environment": True})
    package_path = package(args.binaries, profile, env)
    if source_snapshot() != observed_sources:
        raise RuntimeError("observed qualification source changed during the signed app build")
    manifests = list((profile / "apps/pkgs").glob("*.pkg/manifest.toml"))
    if len(manifests) != 1:
        raise RuntimeError("normal installed package manifest is ambiguous")
    installed = tomllib.loads(manifests[0].read_text())
    if installed.get("package", {}).get("id") != "so.tilo.code-editor" or "code_editor.project" not in installed.get("resources", {}).get("kinds", []):
        raise RuntimeError("normal signed installed manifest omits the actual project resource kind")
    private_json(output / "installed-resource-declaration.json", {"manifest_sha256": digest(manifests[0]), "app_id": installed["package"]["id"], "resource_kinds": installed["resources"]["kinds"]})
    observed_binaries = {name: digest(args.binaries / name) for name in ("shell", "tilo-control", "tilo-launcher", "tilo-install", "tilo-app", "tilo-app-package", "tilo-runtime-bundle", "examples/packaged_native_governor", "examples/packaged_native_bootstrap")}
    runtime_files = {str(path.relative_to(profile / "runtimes")): digest(path) for path in (profile / "runtimes").rglob("*") if path.is_file()}
    private_json(output / "inputs.json", {"observed_binary_hashes": observed_binaries, "signed_runtime_file_hashes": runtime_files, "script_sha256": digest(Path(__file__)), "signed_code_editor_package_sha256": digest(package_path), "shell_sha256": digest(args.binaries / "shell"), "no_model_evaluation": True})
    fonts = output / "font-catalog"
    root = HERE.parents[1]
    subprocess.run([sys.executable, "-B", str(root / "tools/font-catalog.py"),
                    "prepare", str(root / "crates/sdk/ui-core/assets/fonts"),
                    str(fonts), "--scope", "os"], check=True)
    private_json(output / "font-namespace.json", {
        "preparer": "tools/font-catalog.py prepare --scope os",
        "manifest_sha256": digest(fonts / "manifest.toml"),
        "mount": "/usr/share/fonts/tilo", "host_files_modified": False,
    })
    # Normal Cargo/SSH packaging happens above, outside this user namespace.
    # Only the enrolled desktop runtime enters the standard OS font mount.
    subprocess.run(["bwrap", "--die-with-parent", "--bind", "/", "/",
                    "--dev-bind", "/dev", "/dev", "--tmpfs", "/usr/share/fonts",
                    "--ro-bind", str(fonts), "/usr/share/fonts/tilo", "--",
                    sys.executable, "-B", str(Path(__file__).resolve()),
                    "--binaries", str(args.binaries), "--out", str(output),
                    "--runtime-only"], env=env, check=True)


def read_phase_receipt(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 1_048_576:
        raise RuntimeError("bounded regular preparation receipt unavailable")
    return json.loads(path.read_text())


def resume_native(args, output):
    if output.is_symlink() or not output.is_dir() or output.stat().st_mode & 0o077:
        raise RuntimeError("private prepared output directory unavailable")
    source = read_phase_receipt(output / "proof-source-at-start.json")
    inputs = read_phase_receipt(output / "inputs.json")
    fonts = read_phase_receipt(output / "font-namespace.json")
    observed_sources = source_snapshot()
    if source["sha256"] != digest(Path(__file__)) or source["observed_source_hashes"] != observed_sources:
        raise RuntimeError("source changed across package/runtime boundary")
    names = ("shell", "tilo-control", "tilo-launcher", "tilo-install", "tilo-app",
             "tilo-app-package", "tilo-runtime-bundle", "examples/packaged_native_governor",
             "examples/packaged_native_bootstrap")
    observed_binaries = {name: digest(args.binaries / name) for name in names}
    if inputs["observed_binary_hashes"] != observed_binaries:
        raise RuntimeError("platform executable changed across package/runtime boundary")
    profile = output / "profile"
    if digest(profile / "code-editor.tapp") != inputs["signed_code_editor_package_sha256"]:
        raise RuntimeError("signed app changed across package/runtime boundary")
    runtime_files = {str(path.relative_to(profile / "runtimes")): digest(path)
                     for path in (profile / "runtimes").rglob("*") if path.is_file()}
    if runtime_files != inputs["signed_runtime_file_hashes"]:
        raise RuntimeError("signed runtime changed across package/runtime boundary")
    if digest(Path("/usr/share/fonts/tilo/manifest.toml")) != fonts["manifest_sha256"]:
        raise RuntimeError("mounted font catalog differs from prepared catalog")
    mounts = Path("/proc/self/mountinfo").read_text().splitlines()
    if not any(row.split()[4] == "/usr/share/fonts/tilo" and "ro" in row.split()[5].split(",") for row in mounts):
        raise RuntimeError("canonical font catalog is not mounted read-only")
    subprocess.run([sys.executable, "-B", str(HERE.parents[1] / "tools/font-catalog.py"),
                    "verify", "/usr/share/fonts/tilo", "--scope", "os"], check=True)
    private_json(output / "runtime-boundary.json", {
        "prepared_sources_and_binaries_match": True,
        "signed_app_and_runtime_match": True, "font_mount_observed_read_only": True,
        "compiler_input_attestation": False,
    })
    run_native(args, output, profile, dict(os.environ), observed_sources, observed_binaries)


def run_native(args, output, profile, env, observed_sources, observed_binaries):
    runtime = Path(env["XDG_RUNTIME_DIR"])
    processes, logs = [], []
    with tempfile.TemporaryDirectory(prefix="tilo-code-editor-text-", dir=runtime) as control_dir:
        socket = str(Path(control_dir) / "control.sock")
        env["TILO_TEST_CONTROL"] = socket
        def spawn(binary, arguments, name):
            log = (output / name).open("x")
            logs.append(log)
            process = subprocess.Popen([str(binary), *map(str, arguments)], env=env, stdout=log, stderr=log, start_new_session=True, preexec_fn=die_with_parent())
            processes.append(process)
            return process
        try:
            service_sockets = [runtime / "tilo/services" / name for name in ("identity.sock", "model-gateway.sock")]
            if any(path.exists() or path.is_symlink() for path in service_sockets):
                raise RuntimeError("existing native session service endpoints belong to another fixture")
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
            if not all(c in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for c in room):
                raise RuntimeError("fixture room is not a verbatim launcher room slug")
            directory = profile / "data/tilo-rooms" / room / "so.tilo.code-editor"
            directory.mkdir(mode=0o700, parents=True)
            fixture = directory / "fixture.txt"
            initial = "αβ😀γ\nfinal"
            fixture.write_text(initial, encoding="utf-8")
            fixture.chmod(0o600)
            private_json(output / "fixture.json", {"sha256": digest(fixture), "generic_fixture": True,
                "mount": "normal room app directory", "sandbox_path": "/run/tilo/app/fixture.txt"})
            Path(socket).unlink(missing_ok=True)
            shell = spawn(args.binaries / "shell", ["--backend", "headless", "--enable-peer-sync"], "qualified-shell.log")
            bootstrap.wait_for("qualified compositor control", lambda: Path(socket).exists(), shell)
            app = bootstrap.wait_for("signed Code Editor Applications Open", lambda: next((row for row in bootstrap.actions(socket) if row.get("provider") == "applications" and row.get("id") == "so.tilo.code-editor"), None), shell)
            bootstrap.wait_terminal(socket, bootstrap.invoke(socket, app), shell, public_failure_path=output / "failed-open.json")
            observed_catalogue = []
            def offered(operation, resource=None):
                nonlocal observed_catalogue
                rows = bootstrap.actions(socket)
                observed_catalogue = [bootstrap.public_action_observation(row) for row in rows if row.get("application") == "so.tilo.code-editor"]
                matches = [row for row in rows if row.get("application") == "so.tilo.code-editor" and row.get("room") == room and row.get("operation") == operation and (resource is None or row.get("resource") == resource)]
                if len(matches) > 1:
                    raise RuntimeError("ambiguous Code Editor operation target")
                return matches[0] if matches else None
            def perform(operation, arguments, name, resource=None):
                try:
                    action = bootstrap.wait_for(operation, lambda: offered(operation, resource), shell)
                except Exception:
                    private_json(output / (name + "-catalogue-at-refusal.json"), {"observed_code_editor_actions": observed_catalogue, "expected_room": room, "operation": operation})
                    windows = subprocess.run([str(args.binaries / "tilo-control"), "--socket", socket, "windows", "--json"], capture_output=True, text=True, timeout=5, check=False, env=env)
                    private_json(output / (name + "-windows-at-refusal.json"), {"returncode": windows.returncode, "windows": json.loads(windows.stdout) if windows.returncode == 0 else None})
                    raise
                private_json(output / (name + "-action.json"), {**bootstrap.public_action_observation(action), "params": action.get("params"), "contract": action.get("contract"), "operation_kind": action.get("operation_kind")})
                if operation == "open-file":
                    # Owner-authorized setup follows this actual publication's
                    # approval contract. Device-only actor checks stay in the
                    # production door; no argument invents a filesystem grant.
                    params = {value["name"] for value in action.get("params", [])}
                    policy = action.get("contract", {}).get("confirmation", {})
                    if "confirm" in params:
                        arguments = {**arguments, "confirm": True}
                    elif "confirmation_phrase" in params:
                        phrase = policy.get("phrase")
                        if not isinstance(phrase, str) or not phrase:
                            raise RuntimeError("advertised confirmation phrase unavailable")
                        arguments = {**arguments, "confirmation_phrase": phrase}
                    elif policy.get("mode") != "none":
                        raise RuntimeError("required approval parameter is not advertised")
                private_json(output / (name + "-invoke-request.json"), {"provider": action["provider"], "id": action["id"], "args": arguments})
                response = call(socket, {"op": "actions_invoke", "provider": action["provider"], "id": action["id"], "args": arguments})
                private_json(output / (name + "-invoke-response.json"), response)
                if "err" in response:
                    raise RuntimeError(f"ordinary {operation} argument/admission refused: {response['err']}")
                accepted = response.get("answer")
                if not isinstance(accepted, dict):
                    raise RuntimeError("original ordinary action acknowledgement is absent")
                private_json(output / (name + "-accepted.json"), accepted)
                terminal = bootstrap.wait_terminal(socket, accepted, shell, public_failure_path=output / (name + "-failure.json"))
                private_json(output / (name + "-terminal.json"), terminal)
                return action, terminal.get("update", {}).get("output", {}).get("value")
            perform("open-file", {"path": "/run/tilo/app/fixture.txt"}, "open-file")
            action, observed = perform("read-text", {"start_chars": 1, "max_chars": 2}, "read-before")
            if observed["text"] != "β😀" or observed["text_sha256"] != digest(fixture) or observed["position_units"] != "unicode_scalar_values":
                raise RuntimeError("actual Unicode range/current identity differs")
            resource = action["resource"]
            snapshot_action, snapshot = perform("read-text-snapshot", {"start_chars": 0, "max_chars": 3}, "snapshot-first", resource)
            heads = snapshot["basis"]["heads"]
            if not heads or heads != sorted(set(heads)) or snapshot["content"]["text"] != "αβ😀":
                raise RuntimeError("original committed snapshot first chunk differs")
            _, remaining = perform("read-text-snapshot", {"start_chars": 3, "max_chars": 4096,
                "expected_heads": json.dumps(heads)}, "snapshot-second", resource)
            if remaining["basis"]["heads"] != heads or snapshot["content"]["text"] + remaining["content"]["text"] != initial:
                raise RuntimeError("same-revision source chunks differ")
            for name, chunk in (("snapshot-first", snapshot), ("snapshot-second", remaining)):
                terminal = json.loads((output / (name + "-terminal.json")).read_text())
                action = json.loads((output / (name + "-action.json")).read_text())
                if chunk["record"]["roomId"] != action["room"] or chunk["record"]["appId"] != action["application"]:
                    raise RuntimeError("source chunk differs from actual published workspace/app")
                if chunk["record"]["recordId"] != action["record"]:
                    raise RuntimeError("source chunk differs from actual published canonical record")
                if chunk["content"]["file"] != action["semantic_identity"]["key"].rsplit("/", 1)[-1]:
                    raise RuntimeError("source chunk differs from exact file target")
                if not terminal["update"]["invocation"] or chunk["content"]["text_sha256"] != digest(fixture):
                    raise RuntimeError("source chunk lacks original terminal or complete source identity")
            file_id = resource.rsplit("/", 1)[-1]
            edit = {"edit": {"path": [{"field": "buffers"}, {"key": file_id}], "op": {"op": "rich", "rich": {
                "action": "contextual", "expected_base_digest": observed["text_sha256"], "start_chars": 1,
                "delete_chars": 2, "insert": "X", "deleted_text": observed["text"],
                "before_context": observed["before_context"], "after_context": observed["after_context"]}}}}
            perform("field-edit", edit, "edit")
            _, changed = perform("read-text", {"start_chars": 0, "max_chars": 4096}, "read-after", resource)
            expected = "αXγ\nfinal"
            if changed["text"] != expected or changed["text_sha256"] != hashlib.sha256(expected.encode()).hexdigest():
                raise RuntimeError("public contextual edit did not yield expected current document")
            stale_snapshot_action = bootstrap.wait_for("same file snapshot action", lambda: offered("read-text-snapshot", resource), shell)
            stale_snapshot = bootstrap.invoke(socket, stale_snapshot_action, {"start_chars": 0, "max_chars": 1,
                "expected_heads": json.dumps(heads)})
            private_json(output / "snapshot-stale-accepted.json", stale_snapshot)
            snapshot_failure = output / "snapshot-stale-failure.json"
            try:
                bootstrap.wait_terminal(socket, stale_snapshot, shell, public_failure_path=snapshot_failure)
            except RuntimeError:
                if not snapshot_failure.exists() or "stale_cursor" not in snapshot_failure.read_text():
                    raise RuntimeError("terminal refusal does not prove exact-head snapshot staleness")
            else:
                raise RuntimeError("stale source snapshot unexpectedly succeeded")
            stale_action = bootstrap.wait_for("project field edit action", lambda: offered("field-edit"), shell)
            stale = bootstrap.invoke(socket, stale_action, edit)
            private_json(output / "stale-accepted.json", stale)
            failure_path = output / "stale-failure.json"
            try:
                bootstrap.wait_terminal(socket, stale, shell, public_failure_path=failure_path)
            except RuntimeError:
                if not failure_path.exists():
                    raise
                refused = json.loads(failure_path.read_text())
                if refused.get("update", {}).get("state") != "failed" or "the text changed where the edit was made" not in json.dumps(refused):
                    raise RuntimeError("failed operation does not prove contextual stale refusal")
            else:
                raise RuntimeError("overlapping stale edit unexpectedly succeeded")
            _, unchanged = perform("read-text", {"start_chars": 0, "max_chars": 4096}, "read-refused", resource)
            if unchanged != changed or fixture.read_text() != initial:
                raise RuntimeError("refused stale edit mutated document or text action wrote disk")
            if source_snapshot() != observed_sources:
                raise RuntimeError("observed source changed during native qualification")
            if any(digest(args.binaries / name) != value for name, value in observed_binaries.items()):
                raise RuntimeError("observed platform executable changed during native qualification")
            private_json(output / "qualification.json", {"kind": "signed-native-code-editor-text-actions",
                "original_public_actions_invoked": True, "unicode_range_observed": True,
                "partial_edit_observed": True, "stale_overlap_refused": True,
                "current_text_sha256": changed["text_sha256"], "disk_unchanged": True,
                "same_committed_heads_chunks_observed": True,
                "original_snapshot_admission_observed": True, "stale_snapshot_heads_refused": True,
                "model_evaluation": False, "compiler_input_attestation": False})
        finally:
            for process in reversed(processes):
                stop(process)
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True, help="new private directory")
    parser.add_argument("--runtime-only", action="store_true", help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    arguments.binaries = arguments.binaries.resolve()
    try:
        run(arguments)
    except Exception as error:
        if arguments.out.exists() and not (arguments.out / "failure.json").exists():
            private_json(arguments.out / "failure.json", {"qualified": False, "reason": str(error), "model_evaluation": False})
        raise
