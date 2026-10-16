#!/usr/bin/env python3
"""Signed Files Open -> real selection/Paste -> edit -> native Undo/Redo.

Run through the files_clipboard_native example on an approved remote Cargo runner.
Uses the normal SDK packager, installer, native enrollment, Applications action,
production app main and real headless compositor. Everything lives in an owned
scratch profile; no operator desktop, persistent grant or machine setting changes.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from _control import call, require_ok
from reaper import die_with_parent
from tilo_client import scratch_session_env

REPO = pathlib.Path(__file__).resolve().parents[2]
APP = "so.tilo.file-explorer"


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


shared = module("packaged_files_support", "packaged-native-bootstrap.py")
pixels = module("files_pixel_support", "full-loop.py")


def install(binaries, profile, env):
    publisher = profile / "publisher.json"
    shared.run(binaries / "tilo-app-package", ["keygen", "--out", publisher], env)
    package = profile / "files.tapp"
    shared.run(binaries / "tilo-app", ["package", REPO / "apps/file-explorer",
        "--output", package, "--identity", publisher, "--profile", "thin-app"], env)
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    targets = [line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: ")]
    if len(targets) != 1:
        raise RuntimeError("normal package build target is unobservable")
    apps = binaries.parent / targets[0] / "thin-app"
    library_root = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    libraries = shared.shared_libraries([apps / "file-explorer"], f"{apps}:{apps / 'deps'}:{library_root}")
    version = "2026.07"
    args = ["build", "--version", version, "--out", profile / "runtimes" / version, "--identity", publisher]
    for library in libraries:
        args.extend(["--include", library])
    shared.run(binaries / "tilo-runtime-bundle", args, env)
    key = subprocess.check_output([str(binaries / "tilo-runtime-bundle"), "key", "pubkey", "--identity", str(publisher)], env=env, text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{64}", key):
        raise RuntimeError("scratch runtime publisher is unobservable")
    pin = profile / "runtime-publishers.txt"
    pin.write_text(key + "\n")
    env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(pin)
    shared.run(binaries / "tilo-install", ["install", package, "--allow-self-signed"], env)
    return {"signed_files_package": shared.digest_file(package),
            "files_entrypoint": shared.digest_file(apps / "file-explorer"),
            "signed_runtime_manifest": shared.digest_file(profile / "runtimes" / version / "bundle.toml")}


def door(socket, room, record, operation):
    rows = [row for row in shared.actions(socket) if row.get("application") == APP
            and row.get("room") == room and row.get("record") == record
            and row.get("semantic_identity") == {"kind": "files.folder", "key": "files.folder"}
            and row.get("operation") == operation]
    if len(rows) > 1:
        raise RuntimeError(f"ambiguous Files {operation} action")
    return rows[0] if rows else None


def entry_keys(socket, room, record):
    # Publication can be gated during capture. None is not an empty folder;
    # callers must observe this exact folder before testing its entry set.
    rows = [row for row in shared.actions(socket) if row.get("application") == APP
            and row.get("room") == room and row.get("record") == record]
    if not any(row.get("semantic_identity") == {"kind": "files.folder", "key": "files.folder"}
               and row.get("operation") == "go-to" for row in rows):
        return None
    return {row["semantic_identity"]["key"] for row in rows
            if isinstance(row.get("semantic_identity"), dict)
            and row["semantic_identity"].get("kind") == "files.entry"}


def execute(socket, room, record, operation, shell, output, args=None):
    action = shared.wait_for(f"published Files {operation}", lambda: door(socket, room, record, operation), shell)
    acknowledgement = call(socket, {"op": "actions_invoke", "provider": action["provider"],
                                    "id": action["id"], "args": args or {}})
    (output / f"acknowledged-{operation}.json").write_text(json.dumps(acknowledgement, indent=2))
    if "err" in acknowledgement:
        raise RuntimeError(f"ordinary Files {operation} refused: {acknowledgement['err']}")
    receipt = shared.wait_terminal(socket, acknowledgement["answer"], shell,
                                   public_failure_path=output / f"failed-{operation}.json")
    (output / f"completed-{operation}.json").write_text(json.dumps(receipt, indent=2))
    return receipt


def observe_grants(socket, room, output, phase):
    tree = call(socket, {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
    rows = [row for row in tree.get("workspaces", []) if row.get("room") == room]
    if len(rows) != 1 or not isinstance(rows[0].get("capabilities"), list):
        raise RuntimeError("exact Files room grants are unobservable")
    (output / f"grants-{phase}.json").write_text(json.dumps(rows[0]["capabilities"], indent=2))


def key(socket, value, shift=False):
    require_ok(call(socket, {"op": "key_down", "key": "Control"}))
    try:
        if shift:
            require_ok(call(socket, {"op": "key_down", "key": "Shift"}))
        require_ok(call(socket, {"op": "key", "key": value}))
    finally:
        if shift:
            require_ok(call(socket, {"op": "key_up", "key": "Shift"}))
        require_ok(call(socket, {"op": "key_up", "key": "Control"}))


def capture(socket, output, name):
    path = output / f"{name}.png"
    require_ok(call(socket, {"op": "snapshot", "path": str(path)}))
    pixels.assert_nonblank_png(path)


def require_fixture_binaries(binaries):
    required = ("shell", "tilo-launcher", "tilo-app", "tilo-app-package",
                "tilo-runtime-bundle", "tilo-install",
                "examples/packaged_native_governor", "examples/packaged_native_bootstrap")
    missing = [name for name in required
               if not (binaries / name).is_file() or not os.access(binaries / name, os.X_OK)]
    if missing:
        raise RuntimeError("build required signed fixture binaries before package preparation: "
                           + ", ".join(missing))


def proof(binaries, output, trash_restore=False, theme=None):
    require_fixture_binaries(binaries)
    env, profile = scratch_session_env()
    if theme is not None:
        env["TILO_SHOTS_THEME"] = theme
    processes, logs = [], []
    runtime_owner = tempfile.TemporaryDirectory(prefix="tilo-files-entry-run-")
    started = time.monotonic()
    try:
        runtime = pathlib.Path(runtime_owner.name)
        socket = str(runtime / "control.sock")
        env.update({"XDG_RUNTIME_DIR": str(runtime), "TILO_TEST_CONTROL": socket,
                    "TILO_APPS_DATA_DIR": str(profile / "apps"),
                    "TILO_RUNTIME_BUNDLE_DIR": str(profile / "runtimes"),
                    "TILO_LAUNCHER_BIN": str(binaries / "tilo-launcher"),
                    "TILO_STATE_PATH": str(profile / "config/tilo/state.json"),
                    "TILO_HEADLESS_SIZE": "1600x900", "CARGO_BUILD_JOBS": "4",
                    "RAYON_NUM_THREADS": "4", "LP_NUM_THREADS": "4"})
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_APPS_SYSTEM_DIR"):
            env.pop(name, None)
        if trash_restore:
            # Exercise the common host data layout under the already granted
            # Home. This changes no grant or launcher sandbox policy.
            data = profile / "home" / ".local" / "share"
            data.mkdir(parents=True)
            env["XDG_DATA_HOME"] = str(data)
        inputs = install(binaries, profile, env)
        inputs.update({name: shared.digest_file(path) for name, path in {
            "shell": binaries / "shell", "launcher": binaries / "tilo-launcher",
            "proof": pathlib.Path(__file__).resolve(),
            "process_binding": REPO / "crates/runtime/runtime-portal/src/process_binding/linux.rs",
        }.items()})
        (output / "input-digests.json").write_text(json.dumps(inputs, indent=2))
        packaged_at = time.monotonic()
        source = profile / "home" / "source"
        receiving = profile / "home" / "receiving"
        source.mkdir()
        receiving.mkdir()
        original, edited = b"actual signed Files source\n", b"later edited receiving bytes\n"
        (source / "report.txt").write_bytes(original)
        protected = profile / "protected-control"
        protected.write_bytes(b"ungranted host control")
        invitation_path = profile / "native-invitation.json"
        for name, command in [
            ("governor", [str(binaries / "examples/packaged_native_governor"), str(profile), str(invitation_path)]),
            ("shell", [str(binaries / "shell"), "--backend", "headless", "--enable-peer-sync"]),
        ]:
            log = open(output / f"{name}.log", "w")
            logs.append(log)
            process = subprocess.Popen(command, env={**env, "RUST_LOG": "warn"}, stdout=log, stderr=log,
                                       start_new_session=True, preexec_fn=die_with_parent())
            processes.append(process)
            if name == "governor":
                shared.wait_for("signed governor invitation", invitation_path.exists, process)
            else:
                shell = process
        invitation = json.loads(invitation_path.read_text())
        if invitation.get("empty_app_records") is not True:
            raise RuntimeError("native fixture must not seed app records")
        shared.wait_for("headless control socket", lambda: pathlib.Path(socket).exists(), shell)
        if not call(socket, {"op": "provision_test_identity"}).get("answer", {}).get("throwaway"):
            raise RuntimeError("isolated identity provisioning failed")
        enrollment = shared.enroll_native(socket, invitation, shell, binaries, env)
        room = enrollment["room"]
        (output / "native-enrollment.json").write_text(json.dumps(enrollment, indent=2))

        def authorship_surface():
            rows = [row for row in shared.actions(socket)
                    if row.get("application") == "so.tilo.desktop"
                    and row.get("room") == room
                    and row.get("operation") == "grant-app-authorship"]
            if len(rows) > 1:
                raise RuntimeError("ambiguous native app authorship surface")
            return rows[0] if rows else None

        # The normal Applications route performs the grant. Observe startup
        # readiness through its published surface, as the packaged app proof does.
        ready = shared.wait_for("native app authorship surface", authorship_surface, shell)
        (output / "authorship-ready.json").write_text(json.dumps(
            shared.public_action_observation(ready), indent=2))
        application = shared.wait_for("signed Files application", lambda: next((row for row in shared.actions(socket, "Files")
            if row.get("provider") == "applications" and row.get("id") == APP), None), shell)
        observe_grants(socket, room, output, "before-open")
        acknowledgement = shared.invoke(socket, application)
        observe_grants(socket, room, output, "open-acknowledged")
        shared.wait_terminal(socket, acknowledgement, shell,
                             public_failure_path=output / "failed-open.json")
        frame = shared.wait_for("actual signed Files presentation", lambda: shared.presented_primary_frame(
            call(socket, {"op": "foreign_frames"}), APP), shell)
        (output / "presented-files.json").write_text(json.dumps(frame, indent=2))
        observe_grants(socket, room, output, "presented")
        def files_record():
            rows = [row for row in shared.actions(socket) if row.get("application") == APP
                    and row.get("room") == room and row.get("operation") == "go-to"
                    and row.get("semantic_identity") == {"kind": "files.folder", "key": "files.folder"}]
            if len(rows) > 1:
                raise RuntimeError("ambiguous presented Files record")
            if rows and isinstance(rows[0].get("record"), str) and rows[0]["record"]:
                return rows[0]
            return None

        folder = shared.wait_for("presented Files folder action", files_record, shell)
        record = folder["record"]
        (output / "files-record.json").write_text(json.dumps(
            shared.public_action_observation(folder), indent=2))
        execute(socket, room, record, "go-to", shell, output, {"path": str(source)})
        execute(socket, room, record, "select-all", shell, output)
        execute(socket, room, record, "copy-selection", shell, output)
        execute(socket, room, record, "go-to", shell, output, {"path": str(receiving)})
        capture(socket, output, "before-paste")
        execute(socket, room, record, "paste", shell, output)
        destination = receiving / "report.txt"
        shared.wait_for("actual pasted bytes", lambda: destination.is_file() and destination.read_bytes() == original, shell)
        def pasted_entry():
            keys = entry_keys(socket, room, record)
            return next(iter(keys)) if keys is not None and len(keys) == 1 else None
        entry = shared.wait_for("actual pasted entry resource", pasted_entry, shell)
        (output / "pasted-entry-resource.json").write_text(json.dumps({"key": entry}, indent=2))
        capture(socket, output, "after-paste")
        destination.write_bytes(edited)
        key(socket, "z")
        shared.wait_for("native Undo preserves source and removes receiving copy", lambda:
                        not destination.exists() and (source / "report.txt").read_bytes() == original, shell)
        trash = pathlib.Path(env["XDG_DATA_HOME"]) / "Trash" / "files"
        copies = [item for item in trash.iterdir() if item.is_file() and item.read_bytes() == edited]
        if len(copies) != 1:
            raise RuntimeError("edited native Copy must be recoverable in standard Trash")
        # Observe the committed publication prefix before asserting its surface.
        # The physical host operation precedes document reconciliation.
        undo_history = execute(socket, room, record, "history", shell, output)
        undo_output = undo_history.get("update", {}).get("output", {}).get("value", {})
        if not undo_output.get("versions") or not undo_output.get("now"):
            raise RuntimeError("post-Undo history did not observe the actual document")
        (output / "completed-post-undo-history.json").write_text(json.dumps(undo_history, indent=2))
        shared.wait_for("Undo withdraws the receiving entry resource", lambda:
                        (keys := entry_keys(socket, room, record)) is not None and entry not in keys, shell)
        capture(socket, output, "after-undo")
        key(socket, "z", shift=True)
        shared.wait_for("native Redo restores later edited bytes", lambda: destination.is_file() and destination.read_bytes() == edited, shell)
        # A successful read-only history receipt follows the same document
        # publication prefix. Physical compensation alone cannot pass this.
        history = execute(socket, room, record, "history", shell, output)
        history_output = history.get("update", {}).get("output", {}).get("value", {})
        if not history_output.get("versions") or not history_output.get("now"):
            raise RuntimeError("post-Redo history did not observe the actual document")
        shared.wait_for("Redo republishes the exact receiving entry resource", lambda:
                        (keys := entry_keys(socket, room, record)) is not None and entry in keys, shell)
        capture(socket, output, "after-redo")
        if trash_restore:
            key(socket, "z")
            shared.wait_for("second Undo retains edited bytes for Trash Restore", lambda:
                            not destination.exists(), shell)
            execute(socket, room, record, "history", shell, output)
            execute(socket, room, record, "open-trash", shell, output)
            capture(socket, output, "trash-before-restore")

            def restore_action():
                rows = [row for row in shared.actions(socket) if row.get("application") == APP
                        and row.get("room") == room and row.get("record") == record
                        and row.get("operation") == "restore"
                        and isinstance(row.get("semantic_identity"), dict)
                        and row["semantic_identity"].get("kind") == "files.entry"]
                if len(rows) > 1:
                    raise RuntimeError("ambiguous actual Trash Restore entry")
                return rows[0] if rows else None

            action = shared.wait_for("actual Trash entry Restore action", restore_action, shell)
            acknowledgement = shared.invoke(socket, action)
            restored = shared.wait_terminal(socket, acknowledgement, shell,
                                            public_failure_path=output / "failed-trash-restore.json")
            (output / "completed-trash-restore.json").write_text(json.dumps(restored, indent=2))
            shared.wait_for("Trash Restore returns the later edited bytes", lambda:
                            destination.is_file() and destination.read_bytes() == edited, shell)
            execute(socket, room, record, "go-to", shell, output, {"path": str(receiving)})
            capture(socket, output, "after-trash-restore")
        if (source / "report.txt").read_bytes() != original or protected.read_bytes() != b"ungranted host control":
            raise RuntimeError("source or ungranted control changed")
        (output / "result.json").write_text(json.dumps({
            "passed": True, "signed_files_entrypoint": True, "production_native_enrollment": True,
            "presented_primary_frame": True, "native_copy_paste_undo_redo": True,
            "later_edit_preserved_in_trash_and_redo": True, "source_and_control_intact": True,
            "post_redo_history_receipt": True, "published_entry_tracks_undo_redo": True,
            "packaged_trash_restore": trash_restore,
            "requested_theme": theme,
            "packaging_seconds": round(packaged_at - started, 3),
            "journey_seconds": round(time.monotonic() - packaged_at, 3),
            "operator_profile_modified": False, "display_launched": False,
        }, indent=2))
        print(f"PASS signed Files entrypoint clipboard journey; evidence {output}", flush=True)
    except Exception:
        if "receiving" in locals():
            pasted = receiving / "report.txt"
            (output / "failed-filesystem.json").write_text(json.dumps({
                "receiving_copy_present": pasted.is_file(),
                "receiving_copy_matches_source": pasted.is_file() and pasted.read_bytes() == original,
                "source_intact": (source / "report.txt").read_bytes() == original,
                "protected_control_intact": protected.read_bytes() == b"ungranted host control",
            }, indent=2))
        if len(processes) == 2 and processes[-1].poll() is None:
            try:
                (output / "failed-foreign-frames.json").write_text(json.dumps(
                    shared.foreign_frames(call(socket, {"op": "foreign_frames"})), indent=2))
                (output / "failed-catalog.json").write_text(json.dumps([shared.public_action_observation(row)
                    for row in shared.actions(socket)], indent=2))
                capture(socket, output, "failed")
            except Exception as error:
                (output / "failed-observation.txt").write_text(type(error).__name__ + "\n")
        raise
    finally:
        for process in reversed(processes):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=8)
        for log in logs:
            log.close()
        runtime_owner.cleanup()
        shutil.rmtree(profile)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, required=True)
    parser.add_argument("--trash-restore", action="store_true",
                        help="Also browse host Trash and restore the edited copy through its published entry action")
    parser.add_argument("--theme", choices=("dark", "light", "high-contrast"),
                        help="Render the isolated installed journey with this system theme")
    parser.add_argument("--font-namespace", type=pathlib.Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.font_namespace is not None:
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"), "verify",
                        "/usr/share/fonts/tilo", "--scope", "os"], check=True)
        proof(args.binaries.resolve(), args.font_namespace, args.trash_restore, args.theme)
        return
    parent = REPO / "target/files-clipboard-native"
    parent.mkdir(parents=True, exist_ok=True)
    output = pathlib.Path(tempfile.mkdtemp(prefix="run-", dir=parent))
    print(f"Evidence: {output}", flush=True)
    with tempfile.TemporaryDirectory(prefix="tilo-files-fonts-") as scratch:
        fonts = pathlib.Path(scratch) / "catalog"
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"), "prepare",
                        str(REPO / "crates/sdk/ui-core/assets/fonts"), str(fonts), "--scope", "os"], check=True)
        (output / "font-namespace.json").write_text(json.dumps({
            "installer": "tools/font-catalog.py prepare --scope os",
            "manifest_sha256": shared.digest_file(fonts / "manifest.toml"),
            "mount": "/usr/share/fonts/tilo", "read_only": True,
            "host_files_modified": False,
        }, indent=2))
        command = ["bwrap", "--die-with-parent", "--bind", "/", "/", "--dev-bind", "/dev", "/dev",
                        "--tmpfs", "/usr/share/fonts", "--ro-bind", str(fonts), "/usr/share/fonts/tilo", "--",
                        sys.executable, "-B", str(pathlib.Path(__file__).resolve()), "--binaries",
                        str(args.binaries.resolve()), "--font-namespace", str(output)]
        if args.trash_restore:
            command.append("--trash-restore")
        if args.theme is not None:
            command.extend(["--theme", args.theme])
        subprocess.run(command, check=True)


if __name__ == "__main__":
    main()
