#!/usr/bin/env python3
"""Prove signed Applications Open -> signed Chat -> durable native Category.
Uses only the existing scratch session, control socket, launcher and owner.
No managed Agent execution, model provider, floating window or custom authority fixture.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import math
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
sys.path.insert(0, str(REPO / "tools"))
import atspi_external_client as atspi
ACCESSIBILITY_OBSERVATION = {}

def foreign_frames(response):
    """Require the existing control operation's complete frame observation."""
    frames = response.get("answer", {}).get("frames")
    if frames is None:
        frames = response.get("foreign_frames", {}).get("frames")
    if not isinstance(frames, list) or any(not isinstance(row, dict) for row in frames):
        raise RuntimeError("foreign_frames observation is absent or malformed")
    return frames


def presented_primary_frame(response, app_id):
    """A retained buffer or another presentation is not the launched window."""
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == app_id and row.get("floating_id") is None
            and row.get("presented") is True and "last_error" in row
            and row["last_error"] is None
            and type(row.get("surface_id")) is int and row["surface_id"] > 0
            and type(row.get("frame_seq")) is int and row["frame_seq"] > 0]
    if len(rows) > 1:
        raise RuntimeError("ambiguous presented primary application window")
    return rows[0] if rows else None


def wait_for(label, predicate, process=None, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"{label}: process exited with {process.returncode}")
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for {label}")


def run(binary, args, env):
    result = subprocess.run([str(binary), *map(str, args)], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        raise RuntimeError(f"{binary.name} failed ({result.returncode}): {result.stderr.strip()}")


def digest_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def shared_libraries(binaries, library_path):
    libraries = set()
    for binary in binaries:
        output = subprocess.check_output(["ldd", str(binary)], text=True,
                                         env={**os.environ, "LD_LIBRARY_PATH": library_path})
        if "not found" in output:
            raise RuntimeError(f"unresolved native library in {binary.name}")
        for line in output.splitlines():
            match = re.search(r"(?:=>\s*)?(/[^\s]+)\s+\(", line)
            if match:
                libraries.add(pathlib.Path(match.group(1)).resolve())
    return sorted(libraries)


def install_packages(binaries, profile, env):
    """Use the ordinary SDK packager and authored runtime/declarations."""
    package = binaries / "tilo-app-package"
    identity = profile / "publisher.json"
    run(package, ["keygen", "--out", identity], env)
    tapp = profile / "chat.tapp"
    run(binaries / "tilo-app", ["package", REPO / "apps/chat",
        "--output", tapp, "--identity", identity, "--profile", "thin-app"], env)
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    triples = [line.removeprefix("host: ") for line in compiler.splitlines()
               if line.startswith("host: ")]
    if len(triples) != 1 or not triples[0]:
        raise RuntimeError("normal packager host target is unobservable")
    # The ordinary SDK builds thin packages for its explicit host target.
    apps = binaries.parent / triples[0] / "thin-app"
    rust_libraries = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    includes = shared_libraries([apps / "chat"], f"{apps}:{apps / 'deps'}:{rust_libraries}")
    # The ordinary runtime CLI supports a signed named release, as authored
    # by this app. No manifest/declaration is synthesized or rewritten.
    version = "2026.07"
    args = ["build", "--version", version, "--out", profile / "runtimes" / version,
            "--identity", identity]
    for library in includes:
        args.extend(["--include", library])
    run(binaries / "tilo-runtime-bundle", args, env)
    public = subprocess.check_output(
        [str(binaries / "tilo-runtime-bundle"), "key", "pubkey", "--identity", str(identity)],
        env=env, text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{64}", public):
        raise RuntimeError("fixture runtime publisher is unobservable")
    pin = profile / "runtime-publishers.txt"
    pin.write_text(public + "\n")
    env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(pin)
    run(binaries / "tilo-install", ["install", tapp, "--allow-self-signed"], env)
    return version, apps / "chat"

def diagnose_owned_shell(process, output):
    """Bounded thread evidence for this exact unreaped fixture child only."""
    if process.poll() is not None:
        return
    task_root = pathlib.Path(f"/proc/{process.pid}/task")
    threads = []
    for task in sorted(task_root.iterdir())[:128]:
        try:
            threads.append({"thread": int(task.name),
                            "wait_channel": (task / "wchan").read_text()[:128]})
        except (OSError, ValueError):
            continue
    (output / "owned-shell-threads.json").write_text(json.dumps(threads, indent=2))
    debugger = shutil.which("gdb")
    if debugger is not None and process.poll() is None:
        try:
            result = subprocess.run([
                debugger, "--batch", "--nx", "--quiet", "-p", str(process.pid),
                "-ex", "set pagination off", "-ex", "set print frame-arguments none",
                "-ex", "thread apply all bt 8", "-ex", "detach",
            ], capture_output=True, timeout=3, check=False)
            (output / "owned-shell-stacks.txt").write_bytes(
                (result.stdout + result.stderr)[:131072])
        except subprocess.TimeoutExpired:
            (output / "owned-shell-stacks.txt").write_text("debugger timed out; no stack proof")


def actions(socket, query=""):
    response = call(socket, {"op": "actions_enumerate", "query": query})
    if "err" in response:
        raise RuntimeError(f"actions enumeration refused: {response['err']['code']}")
    return response["answer"]["actions"]


def invoke(socket, action, args=None):
    response = call(socket, {"op": "actions_invoke", "provider": action["provider"],
                             "id": action["id"], "args": args or {}})
    if "err" in response:
        raise RuntimeError(f"ordinary action {action['id']} refused: {response['err']['code']}")
    if not isinstance(response.get("answer"), dict):
        raise RuntimeError("ordinary semantic action acknowledgement is absent")
    return response["answer"]


def wait_terminal(socket, acknowledgement, process, *, public_failure_path=None):
    """Observe the original invocation; admission is not completed work."""
    receipt = acknowledgement.get("receipt")
    if not isinstance(receipt, dict):
        raise RuntimeError("ordinary action receipt is absent")
    original = receipt.get("update", {}).get("invocation")
    if not original:
        raise RuntimeError("ordinary action invocation identity is absent")

    def completed():
        nonlocal receipt
        update = receipt.get("update")
        if not isinstance(update, dict) or update.get("invocation") != original:
            if public_failure_path is not None:
                public_failure_path.write_text(json.dumps(receipt, indent=2) + "\n")
            raise RuntimeError("ordinary action observation changed its invocation")
        state = update.get("state")
        if state == "succeeded":
            return receipt
        if state in {"failed", "cancelled"}:
            # Opt in only for a known public, test-owned operation. Generic
            # calls may carry private drafts/results and must not export them.
            if public_failure_path is not None:
                public_failure_path.write_text(json.dumps(receipt, indent=2) + "\n")
            raise RuntimeError(f"ordinary action terminated {state}")
        if state not in {"accepted", "progress", "item"}:
            raise RuntimeError("ordinary action lifecycle observation is absent")
        broker_id = receipt.get("broker_invocation_id")
        if not isinstance(broker_id, int) or isinstance(broker_id, bool) or broker_id <= 0:
            raise RuntimeError("pending ordinary action has no observable broker identity")
        response = call(socket, {"op": "actions_status", "invocation_id": broker_id})
        if "err" in response:
            if public_failure_path is not None:
                # This opt-in is only for a known public, test-owned operation.
                # Retain the control protocol's error, never arbitrary reply
                # payloads or a pending action's drafts/results. A status
                # refusal is not evidence that the original action failed.
                error = response["err"]
                fields = {}
                if isinstance(error, dict):
                    for key, limit in (("code", 128), ("message", 4096)):
                        value = error.get(key)
                        if isinstance(value, str):
                            fields[key] = value[:limit]
                            if len(value) > limit:
                                fields[key + "_truncated"] = True
                public_failure_path.write_text(json.dumps({
                    "observation": "actions_status_refused",
                    "invocation": original[:256] if isinstance(original, str) else None,
                    "broker_invocation_id": broker_id,
                    "err": fields,
                }, indent=2) + "\n")
            raise RuntimeError("ordinary action terminal observation was refused")
        receipt = response.get("answer", {}).get("receipt")
        if not isinstance(receipt, dict):
            raise RuntimeError("ordinary action terminal receipt is absent")
        return None

    return wait_for("original ordinary action terminal success", completed, process)


def chrome_state(socket):
    response = call(socket, {"op": "state_dump"})
    chrome = response.get("state", {}).get("chrome")
    if not isinstance(chrome, dict):
        raise RuntimeError("production chrome state observation is absent")
    return chrome


def require_action(socket, action_id, query=""):
    matches = [action for action in actions(socket, query) if action["id"] == action_id]
    if len(matches) != 1:
        raise RuntimeError(f"expected one offered action {action_id}, got {len(matches)}")
    return matches[0]


def public_action_observation(row):
    """Retain routing metadata, never arguments or private editor content."""
    fields = ("provider", "id", "application", "room", "record", "resource",
              "operation", "semantic_identity")
    return {key: row.get(key) for key in fields}


def offered_chat_view(socket, room, operation):
    """Retain exactly one published primary Chat operation in this room."""
    matches = [row for row in actions(socket)
               if row.get("application") == "so.tilo.chat"
               and row.get("room") == room and row.get("record") == ""
               and row.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError("ambiguous canonical Chat operation")
    return matches[0] if matches else None

def chat_accessibility():
    """Observe one primary Chat window on this fixture's real accessibility bus."""
    address = atspi.atspi_address()
    counts = {"matched_chat_processes": 0, "primary_windows": 0,
              "navigation_controls": 0, "stale_tree": False}
    ACCESSIBILITY_OBSERVATION.clear()
    ACCESSIBILITY_OBSERVATION.update(counts)
    matches = []
    for bus, proxy_pid in atspi.bus_pids(address).items():
        pid = atspi.app_process(proxy_pid)
        try:
            program = pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0", 1)[0]
        except OSError:
            continue
        # The installed signed package path can exceed a display summary's
        # truncation limit. Read only complete argv[0], never its arguments.
        if not program or pathlib.Path(os.fsdecode(program)).name != "chat":
            continue
        counts["matched_chat_processes"] += 1
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                if not client.named({window: client.property(window, "Name")},
                                    "Tilo application window"):
                    continue
                counts["primary_windows"] += 1
                tree = primary_window_tree(client, window)
                controls = client.named(tree, "Show conversation navigation")
                counts["navigation_controls"] += len(controls)
                if controls:
                    matches.append((client, tree))
        except atspi.GateError:
            counts["stale_tree"] = True
            continue
    ACCESSIBILITY_OBSERVATION.update(counts)
    if len(matches) > 1:
        raise RuntimeError("ambiguous primary Chat accessibility window")
    return matches[0] if matches else None


def primary_window_tree(client, window):
    """Bound a complete primary subtree; never traverse editor text children."""
    pending, seen, tree = [window], set(), {}
    while pending:
        path = pending.pop(0)
        if path in seen:
            continue
        if len(seen) >= atspi.APP_TREE_NODE_LIMIT:
            raise RuntimeError("primary Chat accessibility subtree exceeded bound")
        seen.add(path)
        tree[path] = client.property(path, "Name")
        if "org.a11y.atspi.EditableText" in client.interfaces(path):
            continue
        children = client.call(path, "org.a11y.atspi.Accessible.GetChildren")
        pending.extend(child for child in atspi.PATH_RE.findall(children) if child not in seen)
    return tree


def reveal_chat_navigation():
    observation = chat_accessibility()
    if observation is None:
        return None
    client, tree = observation
    targets = client.named(tree, "Show conversation navigation")
    if len(targets) != 1:
        raise RuntimeError("shared Chat navigation control is unobservable")
    answer = client.call(targets[0], "org.a11y.atspi.Action.DoAction", "0")
    if "true" not in answer.lower():
        raise RuntimeError("shared Chat navigation activation refused")
    return {"primary_window": True, "shared_navigation_activated": True,
            "private_content_recorded": False}


def visible_category():
    observation = chat_accessibility()
    if observation is None:
        return None
    client, tree = observation
    matches = (client.named(tree, "Collapse Lifecycle proof")
               + client.named(tree, "Expand Lifecycle proof"))
    if len(matches) > 1:
        raise RuntimeError("ambiguous saved Category in primary Chat window")
    return {"primary_window": True, "saved_category_visible": True,
            "private_content_recorded": False} if matches else None


def inspect_owner(binaries, env, room, record="", *, enrollment=False, record_status=False):
    mode = ("--check-native-chat-record" if record_status else
            "--check-native-enrollment" if enrollment else "--check-native-document")
    result = subprocess.run([str(binaries / "examples/packaged_native_bootstrap"),
                             mode, env["TILO_STATE_PATH"], room, record],
                            env=env, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=5)
    if result.returncode:
        return None
    observed = json.loads(result.stdout)
    if observed.get("room") != room or type(observed.get("generation")) is not int:
        raise RuntimeError("exact normal owner observation is malformed")
    return observed


def enroll_native(socket, invitation, shell, binaries, env):
    invoke(socket, require_action(socket, "peer.connect", "connect"))
    wait_for("ordinary Connect prompt", lambda: chrome_state(socket).get(
        "peer_sync", {}).get("connect_input") is not None, shell)
    require_ok(call(socket, {"op": "type", "text": invitation["invitation"]}))
    require_ok(call(socket, {"op": "key", "key": "Enter"}))

    def joined():
        status = chrome_state(socket).get("peer_sync", {}).get("status") or ""
        if "refused" in status.lower():
            raise RuntimeError("canonical native enrollment refused")
        return status.startswith("Joined") and invitation["room"] in status

    wait_for("normal signed workspace join", joined, shell)
    require_ok(call(socket, {"op": "key", "key": "Escape"}))
    observed = wait_for("actual canonical native owner", lambda: inspect_owner(
        binaries, env, invitation["room"], enrollment=True), shell)
    if (observed.get("admitted") is not True
            or observed.get("governor_is_current_device") is not True
            or observed.get("pinned_host") is not False
            or observed.get("workspace_administration_permitted") is not True):
        raise RuntimeError("scratch join did not retain genuine governor custody")
    return observed


def proof(binaries, output, size):
    env, profile = scratch_session_env()
    processes, logs = [], []
    runtime_owner = tempfile.TemporaryDirectory(prefix="tilo-proof-run-")
    try:
        # AccessKit exports its endpoint when assistive technology is enabled.
        # This is the proof's own dbus-run-session, not the operator's session.
        atspi.command("gdbus", "call", "--session", "--dest", "org.a11y.Bus",
                      "--object-path", "/org/a11y/bus", "--method",
                      "org.freedesktop.DBus.Properties.Set", "org.a11y.Status",
                      "IsEnabled", "<true>")
        status = atspi.command("gdbus", "call", "--session", "--dest", "org.a11y.Bus",
                               "--object-path", "/org/a11y/bus", "--method",
                               "org.freedesktop.DBus.Properties.Get", "org.a11y.Status",
                               "IsEnabled")
        if "true" not in status.lower():
            raise RuntimeError("isolated accessibility session is not enabled")
        (output / "accessibility-enabled.json").write_text(json.dumps({
            "isolated_session": True, "enabled": True,
        }, indent=2))
        inputs = {
            "shell": binaries / "shell", "launcher": binaries / "tilo-launcher",
            "record_source": REPO / "crates/sdk/ui-core/src/record.rs",
            "framework_source": REPO / "crates/sdk/tilo-app/src/framework/app.rs",
            "admission_source": REPO / "crates/sdk/ui-core/src/collaboration/native_initial.rs",
            "proof_source": pathlib.Path(__file__).resolve(),
        }
        (output / "input-digests.json").write_text(json.dumps(
            {name: digest_file(path) for name, path in inputs.items()}, indent=2))
        runtime = pathlib.Path(runtime_owner.name)
        socket = str(runtime / "control.sock")
        env.update({"XDG_RUNTIME_DIR": str(runtime), "TILO_TEST_CONTROL": socket,
                    "TILO_APPS_DATA_DIR": str(profile / "apps"),
                    "TILO_RUNTIME_BUNDLE_DIR": str(profile / "runtimes"),
                    "TILO_LAUNCHER_BIN": str(binaries / "tilo-launcher"),
                    "TILO_STATE_PATH": str(profile / "config/tilo/state.json"),
                    "TILO_HEADLESS_SIZE": size})
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "TILO_APPS_SYSTEM_DIR"):
            env.pop(name, None)
        version, packaged_chat = install_packages(binaries, profile, env)
        inputs["chat"] = packaged_chat
        # The normal SDK packager may rebuild the thin entrypoint. Bind the
        # launch evidence to its actual completed output, not a prior binary.
        (output / "packaged-input-digests.json").write_text(json.dumps({
            **{name: digest_file(path) for name, path in inputs.items()},
            "signed_chat_package": digest_file(profile / "chat.tapp"),
            "signed_runtime_manifest": digest_file(
                profile / "runtimes" / version / "bundle.toml"),
        }, indent=2))
        (output / "runtime-version.txt").write_text(version + "\n")
        installed = subprocess.check_output([str(binaries / "tilo-install"), "list"],
                                            env=env, text=True)
        (output / "installed-packages.txt").write_text(installed)
        if "so.tilo.chat" not in installed:
            raise RuntimeError("normal installer did not publish signed Chat")
        invitation_path = profile / "native-invitation.json"
        governor_log = open(output / "governor.log", "w")
        logs.append(governor_log)
        governor = subprocess.Popen(
            [str(binaries / "examples/packaged_native_governor"),
             str(profile), str(invitation_path)],
            env=env, stdout=governor_log, stderr=governor_log,
            start_new_session=True, preexec_fn=die_with_parent())
        processes.append(governor)
        wait_for("genuine signed governor invitation", invitation_path.exists, governor)
        invitation = json.loads(invitation_path.read_text())
        if invitation.get("empty_app_records") is not True:
            raise RuntimeError("fixture must not seed app records")
        log = open(output / "shell.log", "w")
        logs.append(log)
        shell_env = {**env, "RUST_LOG": "warn,tilo_action_broker::collab_broker_capnp=debug"}
        shell = subprocess.Popen([str(binaries / "shell"), "--backend", "headless",
                                  "--enable-peer-sync"], env=shell_env,
                                 stdout=log, stderr=log, start_new_session=True,
                                 preexec_fn=die_with_parent())
        processes.append(shell)
        wait_for("scratch compositor socket", lambda: pathlib.Path(socket).exists(), shell)
        if not call(socket, {"op": "provision_test_identity"}).get(
                "answer", {}).get("throwaway"):
            raise RuntimeError("normal scratch identity provisioning failed")
        enrollment = enroll_native(socket, invitation, shell, binaries, env)
        (output / "native-enrollment.json").write_text(json.dumps(enrollment, indent=2))

        def authorship_surface():
            rows = [row for row in actions(socket)
                    if row.get("application") == "so.tilo.desktop"
                    and row.get("room") == enrollment["room"]
                    and row.get("operation") == "grant-app-authorship"]
            if len(rows) > 1:
                raise RuntimeError("ambiguous native app authorship surface")
            return rows[0] if rows else None

        # Observe normal startup readiness only. The Applications route must
        # invoke the published grant itself; this fixture cannot grant authority.
        ready = wait_for("native app authorship surface", authorship_surface, shell)
        (output / "authorship-ready.json").write_text(json.dumps(
            public_action_observation(ready), indent=2))
        chat = wait_for("signed Chat application", lambda: next((
            row for row in actions(socket, "Chat")
            if row.get("provider") == "applications" and row.get("id") == "so.tilo.chat"
        ), None), shell)
        open_ack = invoke(socket, chat)
        (output / "chat-open-ack.json").write_text(json.dumps(open_ack, indent=2))
        open_terminal = wait_terminal(socket, open_ack, shell,
                                     public_failure_path=output / "failed-open.json")
        (output / "chat-open-terminal.json").write_text(json.dumps(open_terminal, indent=2))

        def painted_chat():
            frame = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
            if frame is None:
                return None
            buffer, rect = frame.get("buffer"), frame.get("rect")
            if (not isinstance(buffer, dict) or not isinstance(rect, dict)
                    or any(type(buffer.get(k)) is not int or buffer[k] <= 0 for k in ("w", "h"))
                    or any(type(rect.get(k)) not in (int, float)
                           or not math.isfinite(rect[k]) or rect[k] <= 0 for k in ("w", "h"))):
                raise RuntimeError("primary native pixels/placement unobservable")
            return frame

        before = wait_for("actual signed Chat presentation", painted_chat, shell)
        workspace = wait_for("actual canonical Chat workspace View", lambda: offered_chat_view(
            socket, enrollment["room"], "open-workspace"), shell)
        wait_terminal(socket, invoke(socket, workspace), shell)
        navigation = wait_for("shared primary Chat navigation activation",
                              reveal_chat_navigation, shell)
        (output / "navigation-observation.json").write_text(json.dumps(navigation, indent=2))
        time.sleep(0.5)
        require_ok(call(socket, {"op": "snapshot", "path": str(output / f"before-category-{size}.png")}))

        def category_door():
            rows = [row for row in actions(socket)
                    if row.get("application") == "so.tilo.chat"
                    and row.get("room") == enrollment["room"]
                    and row.get("record") == "" and row.get("operation") == "new-category"]
            if len(rows) > 1:
                raise RuntimeError("ambiguous canonical Chat workspace door")
            return rows[0] if rows else None

        category = wait_for("actual original creator Category write door", category_door, shell)
        ack = invoke(socket, category, {"value": "Lifecycle proof"})
        (output / "category-ack.json").write_text(json.dumps(ack, indent=2))
        receipt = wait_terminal(socket, ack, shell,
                                public_failure_path=output / "failed-category-terminal.json")
        (output / "category-terminal.json").write_text(json.dumps(receipt, indent=2))
        observed = wait_for("durable native Category and exact outgoing graph",
                           lambda: inspect_owner(binaries, env, enrollment["room"]), shell)
        if (observed.get("app") != "so.tilo.chat" or observed.get("record") != ""
                or observed.get("category_durable") is not True
                or observed.get("outgoing_publication_retained") is not True):
            raise RuntimeError("canonical Category durable publication absent")
        (output / "category-durable.json").write_text(json.dumps(observed, indent=2))
        category_view = wait_for("actual saved Category View", lambda: offered_chat_view(
            socket, enrollment["room"], "open-category"), shell)
        (output / "category-view-source.json").write_text(json.dumps(
            public_action_observation(category_view), indent=2))
        wait_terminal(socket, invoke(socket, category_view), shell)
        visible = wait_for("saved Category in primary Chat navigation", visible_category, shell)
        (output / "category-visible.json").write_text(json.dumps(visible, indent=2))
        require_ok(call(socket, {"op": "snapshot", "path": str(output / f"after-category-{size}.png")}))
        def membership_door():
            rows = [row for row in actions(socket)
                    if row.get("application") == "so.tilo.desktop"
                    and row.get("room") == enrollment["room"]
                    and row.get("operation") == "add-agent"]
            if len(rows) > 1:
                raise RuntimeError("ambiguous native Add agent door")
            return rows[0] if rows else None

        membership = wait_for("normal published Add agent door", membership_door, shell)
        member_terminal = wait_terminal(socket, invoke(socket, membership, {
            "agent": "bootstrap-proof", "preset": "operator",
        }), shell, public_failure_path=output / "failed-membership-terminal.json")
        (output / "membership-terminal.json").write_text(json.dumps(member_terminal, indent=2))
        member_read = subprocess.run([str(binaries / "examples/packaged_native_bootstrap"),
            "--check-native-agent", env["TILO_STATE_PATH"], enrollment["room"], ""],
            env=env, capture_output=True, text=True, check=False)
        if member_read.returncode:
            category = ("membership_not_admitted" if
                        "exact agent membership was not durably admitted" in member_read.stderr
                        else "owner_inspection_refused")
            (output / "failed-membership-inspector.json").write_text(json.dumps({
                "failure_category": category, "owner_observed": False,
                "room": enrollment["room"], "agent": "bootstrap-proof",
            }, indent=2))
            raise RuntimeError(f"native membership inspection failed: {category}")
        member_observation = json.loads(member_read.stdout)
        committed_generation = member_terminal.get("update", {}).get("output", {}).get("value", {}).get("generation")
        if (member_observation.get("membership_durable") is not True
                or type(committed_generation) is not int
                or member_observation.get("generation", -1) < committed_generation):
            raise RuntimeError("native owner membership is unobservable")
        (output / "membership-durable.json").write_text(json.dumps(member_observation, indent=2))
        frame = wait_for("Category's actual presented repaint", lambda: (
            current if (current := painted_chat()) is not None
            and current["frame_seq"] > before["frame_seq"] else None), shell)
        time.sleep(0.5)
        require_ok(call(socket, {"op": "snapshot", "path": str(output / f"after-category-{size}.png")}))
        (output / "proof.json").write_text(json.dumps({
            "launch_path": "normal-published-Applications-Open", "room": enrollment["room"],
            "chat_frame_seq": frame["frame_seq"], "category_durable": True,
            "category_visible": True,
            "membership_durable": True,
            "membership_owner_generation": member_observation["generation"],
            "Agent_lifecycle_proven": False,
        }, indent=2))
        print("PASS signed Applications Open -> signed Chat -> durable native Category")
    except Exception as original:
        (output / "accessibility-observation.json").write_text(json.dumps(
            ACCESSIBILITY_OBSERVATION, indent=2))
        (output / "failed-observation.json").write_text(json.dumps({
            "exception_type": type(original).__name__,
            "native_interaction_proven": False,
        }, indent=2))
        if "shell" in locals() and shell.poll() is None:
            diagnose_owned_shell(shell, output)
            if "enrollment" in locals():
                try:
                    status = inspect_owner(binaries, env, enrollment["room"], record_status=True)
                    if status is None:
                        raise RuntimeError("canonical record status is unobservable")
                    (output / "failed-native-chat-record.json").write_text(json.dumps(status, indent=2))
                except Exception as observation:
                    (output / "failed-native-owner-observation.json").write_text(json.dumps({
                        "exception_type": type(observation).__name__, "owner_observed": False,
                    }, indent=2))
            try:
                (output / "failed-foreign-frames.json").write_text(json.dumps(
                    foreign_frames(call(socket, {"op": "foreign_frames"})), indent=2))
                require_ok(call(socket, {"op": "snapshot", "path": str(output / f"failed-{size}.png")}))
                (output / "failed-catalog.json").write_text(json.dumps([
                    public_action_observation(row) for row in actions(socket)
                ], indent=2))
            except Exception as observation:
                (output / "failed-render-observation.json").write_text(json.dumps({
                    "exception_type": type(observation).__name__,
                    "pixels_observed": False,
                }, indent=2))
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
    parser.add_argument("--size", choices=["1600x900", "800x600"], default="1600x900")
    parser.add_argument("--font-namespace", type=pathlib.Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.font_namespace is not None:
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"),
                        "verify", "/usr/share/fonts/tilo", "--scope", "os"], check=True)
        mounts = pathlib.Path("/proc/self/mountinfo").read_text().splitlines()
        if not any(row.split()[4] == "/usr/share/fonts/tilo"
                   and "ro" in row.split()[5].split(",") for row in mounts):
            raise RuntimeError("canonical OS font namespace is not read-only")
        proof(args.binaries.resolve(), args.font_namespace, args.size)
        return
    parent = REPO / "target/packaged-native-bootstrap" / args.size
    parent.mkdir(parents=True, exist_ok=True)
    output = pathlib.Path(tempfile.mkdtemp(prefix="run-", dir=parent))
    print(f"Evidence: {output}", flush=True)
    # Match the OS's actual font-catalog stage, outside the signed tiny app.
    # A tmpfs mount hides the host font directory only in our owned namespace;
    # no mountpoint or configuration file is created on the operator's host.
    with tempfile.TemporaryDirectory(prefix="tilo-proof-fonts-") as scratch:
        fonts = pathlib.Path(scratch) / "catalog"
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"),
                        "prepare", str(REPO / "crates/sdk/ui-core/assets/fonts"),
                        str(fonts), "--scope", "os"], check=True)
        manifest = (fonts / "manifest.toml").read_bytes()
        (output / "font-namespace.json").write_text(json.dumps({
            "installer": "tools/font-catalog.py prepare --scope os",
            "manifest_sha256": hashlib.sha256(manifest).hexdigest(),
            "mount": "/usr/share/fonts/tilo", "read_only": True,
            "host_files_modified": False,
        }, indent=2))
        subprocess.run(["bwrap", "--die-with-parent", "--bind", "/", "/",
                        "--dev-bind", "/dev", "/dev",
                        "--tmpfs", "/usr/share/fonts", "--ro-bind", str(fonts),
                        "/usr/share/fonts/tilo", "--", sys.executable, "-B",
                        str(pathlib.Path(__file__).resolve()), "--binaries",
                        str(args.binaries.resolve()), "--size", args.size,
                        "--font-namespace", str(output)], check=True)


if __name__ == "__main__":
    main()
