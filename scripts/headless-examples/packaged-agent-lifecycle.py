#!/usr/bin/env python3
"""Proof through signed packages, strict launcher and the existing control socket.

This never uses the operator's app store, identity, documents or compositor.
Build the normal binaries with rcargo first, then run the shell example with
rcargo --on-builder --fetch packaged-agent-lifecycle run --locked -p shell
--example packaged_agent_lifecycle -- --stage bootstrap. Bootstrap is only
package/roster evidence; it explicitly does not claim detachable Chat works.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
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
import threading
import time
import uuid

from _control import call, require_ok
from reaper import die_with_parent
from tilo_client import scratch_session_env

REPO = pathlib.Path(__file__).resolve().parents[2]

# Reuse the installed-image gate's external accessibility client. This is
# the proof's isolated real bus, not an in-process AccessKit tree or a second
# desktop-control protocol. Route discovery never reads draft content; the
# private round-trip check compares only its own fixture in memory.
sys.path.insert(0, str(REPO / "tools"))
import atspi_external_client as atspi


def accessible_subtree(client, root, limit):
    """Walk one bounded window tree without reading editable descendants."""
    pending = [root]
    seen = set()
    result = {}
    while pending and len(seen) < limit:
        path = pending.pop(0)
        if path in seen:
            continue
        seen.add(path)
        result[path] = client.property(path, "Name")
        if "org.a11y.atspi.EditableText" in client.interfaces(path):
            continue
        children = client.call(path, "org.a11y.atspi.Accessible.GetChildren")
        pending.extend(child for child in atspi.PATH_RE.findall(children) if child not in seen)
    if pending:
        raise atspi.GateError("the bounded application accessibility subtree is incomplete")
    return result


def process_program(pid):
    """Return the complete executable basename, never a truncated comm."""
    try:
        argv0 = pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0", 1)[0]
    except OSError:
        return ""
    return pathlib.Path(os.fsdecode(argv0)).name


def foreign_frames(response):
    """Require the existing control operation's complete frame observation."""
    frames = response.get("answer", {}).get("frames")
    if frames is None:
        frames = response.get("foreign_frames", {}).get("frames")
    if not isinstance(frames, list) or any(not isinstance(row, dict) for row in frames):
        raise RuntimeError("foreign_frames observation is absent or malformed")
    return frames


def agent_conversation_accessibility(agent_name, *, detached=False, diagnostic_path=None):
    """Observe the actual Chat route without reading private editor content."""
    address = atspi.atspi_address()
    peers = atspi.bus_pids(address)
    observed = {
        "bus_connections": len(peers),
        "chat_processes": 0,
        "window_roots": 0,
        "complete_windows": 0,
        "heading_matches": 0,
        "composer_matches": 0,
        "control_matches": 0,
        "drag_handle_matches": 0,
        "private_content_recorded": False,
    }

    def record():
        if diagnostic_path is not None:
            diagnostic_path.write_text(json.dumps(observed, indent=2) + "\n")

    for bus, proxy_pid in peers.items():
        pid = atspi.app_process(proxy_pid)
        if process_program(pid) != "chat":
            continue
        observed["chat_processes"] += 1
        client = atspi.AtspiClient(address, bus)
        try:
            # A suspended secondary window can retain a stale accessible tree.
            # Match a single real window subtree, not unrelated nodes across
            # the application's bus. Native presentation is checked separately.
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            control_name = "Return conversation" if detached else "Float conversation"
            matches = {}
            windows = dict.fromkeys(atspi.PATH_RE.findall(children))
            observed["window_roots"] += len(windows)
            for window in windows:
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                observed["complete_windows"] += 1
                headings = client.named({path: name.casefold() for path, name in tree.items()},
                                        f"{agent_name.casefold()}, agent")
                composers = [path for path in client.named(tree, "Message composer")
                             if "org.a11y.atspi.EditableText" in client.interfaces(path)]
                controls = client.named(tree, control_name)
                drag_handles = client.named(tree, "Drag conversation")
                observed["heading_matches"] += len(headings)
                observed["composer_matches"] += len(composers)
                observed["control_matches"] += len(controls)
                observed["drag_handle_matches"] += len(drag_handles)
                if (len(headings) == 1 and len(composers) == 1
                        and len(controls) == 1 and len(drag_handles) == 1):
                    key = (headings[0], composers[0], controls[0], drag_handles[0])
                    matches.setdefault(key, {"bus": bus, "process": pid, "window": window,
                                             "heading": headings[0], "composer": composers[0],
                                             "control": controls[0], "drag": drag_handles[0],
                                             "agent": agent_name, "detached": detached,
                                             "private_content_recorded": False})
            if len(matches) == 1:
                record()
                return next(iter(matches.values()))
        except atspi.GateError:
            # A stale object during a real tree replacement is not success;
            # the bounded caller retries the complete observation.
            continue
    record()
    return None


def capture_owned_chat_accessibility_transport(shell, path):
    """Probe only this shell's signed Chat launch and filtered bus endpoints.

    This is transport diagnostics, not accessibility success: it never walks
    an accessible tree, changes AT state, or records an address, PID, command,
    environment value, D-Bus reply, or error string.  The exact launcher
    ancestor selects its production per-launch proxy socket names; unrelated
    desktop processes and buses are never queried.
    """
    evidence = {
        "phase": "after_signed_chat_paint",
        "owned_chat_processes": 0,
        "owned_launcher_ancestors": 0,
        "owned_proxy_descendants": 0,
        "session_proxy_socket": False,
        "atspi_proxy_socket": False,
        "session_env_present": False,
        "session_env_is_sandbox_endpoint": False,
        "atspi_env_present": False,
        "atspi_env_is_sandbox_endpoint": False,
        "status_get_all": False,
        "status_enabled": False,
        "address_get": False,
        "address_is_unix": False,
        "atspi_connect": False,
        "registry_events_query": False,
        "transport_complete": False,
        "private_content_recorded": False,
    }

    def process_rows():
        rows = {}
        try:
            entries = pathlib.Path("/proc").iterdir()
        except OSError:
            return rows
        for entry in entries:
            if not entry.name.isdigit():
                continue
            try:
                fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
                parent = int(fields[1])
                program = process_program(int(entry.name))
            except (OSError, IndexError, ValueError):
                continue
            rows[int(entry.name)] = (parent, program)
        return rows

    def descendants(rows, root):
        children = {}
        for process, (parent, _) in rows.items():
            children.setdefault(parent, []).append(process)
        owned = set()
        pending = list(children.get(root, ()))
        while pending:
            process = pending.pop()
            if process in owned:
                continue
            owned.add(process)
            pending.extend(children.get(process, ()))
        return owned

    def launcher_ancestors(rows, chats):
        launchers = set()
        for chat in chats:
            process = chat
            seen = set()
            while process in rows and process not in seen and process != shell.pid:
                seen.add(process)
                parent, program = rows[process]
                if program == "tilo-launcher":
                    launchers.add(process)
                process = parent
        return launchers

    def selected_environment(process):
        selected = {}
        try:
            fields = pathlib.Path(f"/proc/{process}/environ").read_bytes().split(b"\0")
        except OSError:
            return selected
        for field in fields:
            key, separator, value = field.partition(b"=")
            if separator and key in (b"DBUS_SESSION_BUS_ADDRESS", b"AT_SPI_BUS_ADDRESS"):
                selected[key.decode()] = value.decode(errors="surrogateescape")
        return selected

    def dbus_call(address, destination, object_path, method, *arguments):
        try:
            result = subprocess.run(
                ["gdbus", "call", f"--address={address}", "--dest", destination,
                 "--object-path", object_path, "--method", method, *arguments],
                text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                timeout=2, check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            return False, ""
        return result.returncode == 0, result.stdout

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and shell.poll() is None:
        rows = process_rows()
        owned = descendants(rows, shell.pid)
        chats = {process for process in owned if rows[process][1] == "chat"}
        launchers = launcher_ancestors(rows, chats)
        evidence["owned_chat_processes"] = len(chats)
        evidence["owned_launcher_ancestors"] = len(launchers)
        if len(chats) != 1 or len(launchers) != 1:
            time.sleep(0.05)
            continue

        launcher = next(iter(launchers))
        launcher_owned = descendants(rows, launcher)
        proxies = {process for process in launcher_owned
                   if rows[process][1] == "xdg-dbus-proxy"}
        evidence["owned_proxy_descendants"] = len(proxies)
        runtime = pathlib.Path(f"/run/user/{os.getuid()}/tilo/dbus-proxy")
        session_socket = runtime / f"{launcher}.sock"
        atspi_socket = runtime / f"{launcher}-atspi.sock"
        evidence["session_proxy_socket"] = session_socket.is_socket()
        evidence["atspi_proxy_socket"] = atspi_socket.is_socket()

        environment = selected_environment(next(iter(chats)))
        session_env = environment.get("DBUS_SESSION_BUS_ADDRESS")
        atspi_env = environment.get("AT_SPI_BUS_ADDRESS")
        evidence["session_env_present"] = session_env is not None
        evidence["session_env_is_sandbox_endpoint"] = (
            session_env == f"unix:path=/run/user/{os.getuid()}/bus"
        )
        evidence["atspi_env_present"] = atspi_env is not None
        evidence["atspi_env_is_sandbox_endpoint"] = (
            atspi_env == f"unix:path=/run/user/{os.getuid()}/at-spi/bus"
        )
        if not session_socket.is_socket() or not atspi_socket.is_socket():
            time.sleep(0.05)
            continue

        session_address = f"unix:path={session_socket}"
        status_ok, status = dbus_call(
            session_address, "org.a11y.Bus", "/org/a11y/bus",
            "org.freedesktop.DBus.Properties.GetAll", "org.a11y.Status",
        )
        evidence["status_get_all"] = status_ok
        evidence["status_enabled"] = status_ok and bool(
            re.search(r"['\"]IsEnabled['\"]:\s*<true>", status)
        )
        address_ok, address = dbus_call(
            session_address, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus.GetAddress",
        )
        evidence["address_get"] = address_ok
        evidence["address_is_unix"] = address_ok and "unix:" in address
        atspi_ok, _ = dbus_call(
            f"unix:path={atspi_socket}", "org.a11y.atspi.Registry",
            "/org/a11y/atspi/registry", "org.a11y.atspi.Registry.GetRegisteredEvents",
        )
        evidence["atspi_connect"] = atspi_ok
        evidence["registry_events_query"] = atspi_ok
        evidence["transport_complete"] = all([
            evidence["session_env_is_sandbox_endpoint"],
            evidence["atspi_env_is_sandbox_endpoint"], evidence["status_enabled"],
            evidence["address_is_unix"], evidence["registry_events_query"],
        ])
        if evidence["transport_complete"]:
            break
        time.sleep(0.05)

    path.write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def editor_accessibility_focus(access):
    """Observe the editor's AccessKit focus; this does not focus the Wayland seat."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    reply = client.call(access["composer"], "org.a11y.atspi.Accessible.GetState")
    # AT-SPI's state set is two uint32 words. The shared external client uses
    # this same Focused bit, which AccessKit sets only for the active window.
    words = re.fullmatch(r"\(\[(?:uint32 )?(\d+),\s*(?:uint32 )?(\d+)\],\)", reply.strip())
    if words is None or any(int(word) > 0xFFFFFFFF for word in words.groups()):
        raise RuntimeError("the actual editor keyboard focus is unobservable")
    return bool(int(words.group(1)) & atspi.FOCUSED_STATE_BIT)


def task_phase_accessibility(access, label):
    """Observe one exact public status label in the mounted Chat window."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
    return len(client.named(tree, label)) == 1


def _finite_rect(value, label):
    if not isinstance(value, dict) or set(value) != {"x", "y", "w", "h"}:
        raise RuntimeError(f"the presented application {label} has malformed geometry")
    if any(type(value[key]) not in (int, float) or not math.isfinite(value[key])
           for key in value):
        raise RuntimeError(f"the presented application {label} has non-finite geometry")
    if value["w"] <= 2 or value["h"] <= 2:
        raise RuntimeError(f"the presented application {label} has no clickable content")
    return value


def presented_content_geometry(frame):
    """Require the exact compositor transform used for foreign input routing."""
    if not isinstance(frame, dict):
        raise RuntimeError("the presented application frame is malformed")
    placement = _finite_rect(frame.get("rect"), "frame")
    content = _finite_rect(frame.get("content_rect"), "content box")
    origin = frame.get("content_origin")
    if not isinstance(origin, dict) or set(origin) != {"x", "y"}:
        raise RuntimeError("the presented application content origin is malformed")
    if any(type(origin[key]) not in (int, float) or not math.isfinite(origin[key])
           for key in origin):
        raise RuntimeError("the presented application content origin is non-finite")
    if type(frame.get("keyboard_focused")) is not bool:
        raise RuntimeError("the presented application keyboard focus is unobservable")
    return {"placement": placement, "content": content, "origin": origin}


def physically_click_editor(socket, access, geometry):
    """Give the editor both pointer/widget focus and the compositor's Wayland seat."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    rect = atspi.extents_reply(client.call(
        # CoordType.Window is deliberately client-local. Wayland clients do
        # not know their compositor-global placement; the authenticated
        # foreign-frame observation supplies that translation below.
        access["composer"], "org.a11y.atspi.Component.GetExtents", "1",
    ))
    if rect is None or rect[2] <= 2 or rect[3] <= 2:
        raise RuntimeError("the actual editor has no clickable on-screen extent")
    x, y, width, height = rect
    local_x, local_y = x + width / 2, y + height / 2
    global_x = geometry["origin"]["x"] + local_x
    global_y = geometry["origin"]["y"] + local_y
    content = geometry["content"]
    placement = geometry["placement"]
    inside = lambda box: (box["x"] <= global_x < box["x"] + box["w"]
                          and box["y"] <= global_y < box["y"] + box["h"])
    if not inside(content) or not inside(placement):
        raise RuntimeError("the actual editor extent is outside its routed content box")
    require_ok(call(socket, {
        "op": "click_at",
        "x": global_x,
        "y": global_y,
    }))


def component_global_center(access, path, geometry, label):
    """Translate one real client-local AccessKit extent through the compositor route."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    rect = atspi.extents_reply(client.call(
        path, "org.a11y.atspi.Component.GetExtents", "1",
    ))
    if rect is None or rect[2] <= 2 or rect[3] <= 2:
        raise RuntimeError(f"the actual {label} has no clickable on-screen extent")
    x, y, width, height = rect
    global_x = geometry["origin"]["x"] + x + width / 2
    global_y = geometry["origin"]["y"] + y + height / 2
    content = geometry["content"]
    placement = geometry["placement"]
    inside = lambda box: (box["x"] <= global_x < box["x"] + box["w"]
                          and box["y"] <= global_y < box["y"] + box["h"])
    if not inside(content) or not inside(placement):
        raise RuntimeError(f"the actual {label} is outside its routed content box")
    return global_x, global_y


def set_physical_modifiers(socket, *, ctrl=False, shift=False, alt=False, logo=False):
    require_ok(call(socket, {
        "op": "modifiers", "ctrl": ctrl, "shift": shift,
        "alt": alt, "logo": logo,
    }))


def physical_chord(socket, key, **modifiers):
    set_physical_modifiers(socket, **modifiers)
    try:
        require_ok(call(socket, {"op": "key", "key": key}))
    finally:
        set_physical_modifiers(socket)


def require_chat_keyboard_focus_after_click(socket, output):
    """Refuse key injection unless the exact painted Chat surface owns the seat."""
    frame = presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
    )
    if frame is None:
        reason = "presented_chat_missing"
        focused = False
    else:
        # This also rejects malformed geometry/focus fields immediately.
        presented_content_geometry(frame)
        focused = frame["keyboard_focused"]
        reason = "focused" if focused else "chat_not_keyboard_focused"
    if not focused:
        require_ok(call(socket, {
            "op": "snapshot",
            "path": str(output / "managed-task-keyboard-focus-refused.png"),
        }))
        (output / "managed-task-keyboard-focus-refused.json").write_text(json.dumps({
            "phase": "after_physical_chat_click",
            "presented_chat": frame is not None,
            "keyboard_focused": False,
            "reason": reason,
            "private_content_recorded": False,
        }, indent=2) + "\n")
        raise RuntimeError("the physical click did not give the presented Chat surface keyboard focus")
    return True


def private_editor_observation(access, expected):
    """Compare a test-owned draft locally; export neither its value nor hash."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    path = access["composer"]
    reply = client.call(path, "org.a11y.atspi.Text.GetText", "--", "0", "-1")
    value = atspi.text_reply(reply)
    if value is None or value != expected:
        raise RuntimeError("the actual private editor does not retain the expected test draft")
    caret = client.call(path, "org.freedesktop.DBus.Properties.Get",
                        "org.a11y.atspi.Text", "CaretOffset")
    matched = re.fullmatch(r"\(<(?:int32 )?(-?\d+)>,\)", caret.strip())
    if matched is None:
        raise RuntimeError("the actual private editor caret is unobservable")
    caret_offset = int(matched.group(1))
    if not 0 <= caret_offset <= len(value):
        raise RuntimeError("the actual private editor caret is invalid")
    count = client.call(path, "org.a11y.atspi.Text.GetNSelections")
    matched_count = re.fullmatch(r"\((?:int32 )?(\d+),\)", count.strip())
    if matched_count is None or int(matched_count.group(1)) > 1:
        raise RuntimeError("the actual private editor selection is unobservable")
    selection = None
    if int(matched_count.group(1)) == 1:
        reply = client.call(path, "org.a11y.atspi.Text.GetSelection", "0")
        selected = re.fullmatch(r"\((?:int32 )?(-?\d+),\s*(?:int32 )?(-?\d+)\)", reply.strip())
        if selected is None:
            raise RuntimeError("the actual private editor selection endpoints are unobservable")
        selection = [int(selected.group(1)), int(selected.group(2))]
        if not 0 <= selection[0] <= selection[1] <= len(value):
            raise RuntimeError("the actual private editor selection endpoints are invalid")
    return {"draft_matches": True, "caret": caret_offset, "selection": selection,
            "accessibility_focused": editor_accessibility_focus(access)}


def private_editor_value_matches(access, expected):
    """Observe only whether this fixture's in-memory value reached the editor."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    reply = client.call(
        access["composer"], "org.a11y.atspi.Text.GetText", "--", "0", "-1",
        check=False,
    )
    return atspi.text_reply(reply) == expected


def activate_agent_conversation_control(access, label):
    """Activate the exact control paired with the observed private editor."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    actions = client.call(
        access["control"], "org.a11y.atspi.Action.GetActions", check=False
    )
    if "'click'" not in actions:
        raise RuntimeError(f"the actual {label} control has no accessibility action")
    result = client.call(
        access["control"], "org.a11y.atspi.Action.DoAction", "0", check=False
    )
    if result.strip() != "(true,)":
        raise RuntimeError(f"the actual {label} control refused activation")
    return {"accessibility_action": label, "accepted": True}


def presented_primary_frame(response, app_id):
    """A retained buffer or another presentation is not the launched window."""
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == app_id and row.get("floating_id") is None
            and row.get("presented") is True and row.get("last_error") is None
            and type(row.get("frame_seq")) is int and row["frame_seq"] > 0]
    if len(rows) > 1:
        raise RuntimeError("ambiguous presented primary application window")
    return rows[0] if rows else None


def presented_floating_frame(response, app_id):
    """Require one actually painted detached surface, not a retained buffer."""
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == app_id
            and isinstance(row.get("floating_id"), str) and row["floating_id"]
            and row.get("presented") is True and row.get("pixels_ready") is True
            and row.get("last_error") is None
            and type(row.get("frame_seq")) is int and row["frame_seq"] > 0]
    if len(rows) > 1:
        raise RuntimeError("ambiguous presented detached application window")
    return rows[0] if rows else None


def floating_surface_presenting(response, app_id, floating_id):
    """Return is proved by withdrawal, not failed or temporarily missing paint."""
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == app_id and row.get("floating_id") == floating_id]
    if len(rows) > 1:
        raise RuntimeError("ambiguous detached surface presentation")
    if rows and type(rows[0].get("presented")) is not bool:
        raise RuntimeError("detached surface presentation is unobservable")
    return bool(rows and rows[0]["presented"])


def native_float_return(socket, room, record, output, size, process):
    """Exercise the actual seat, IME and native surface handoff end to end."""
    draft = "Keep this private Tide draft"
    access = wait_for("attached Tide editor", lambda: agent_conversation_accessibility("Tide"), process)
    primary = wait_for("Chat's presented primary frame", lambda: presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), process)
    geometry = presented_content_geometry(primary)
    physically_click_editor(socket, access, geometry)
    require_chat_keyboard_focus_after_click(socket, output)
    wait_for("attached Tide editor's accessibility focus",
             lambda: editor_accessibility_focus(access), process)
    require_ok(call(socket, {"op": "type", "text": draft}))
    wait_for(
        "physical typing to reach the actual local editor",
        lambda: private_editor_value_matches(access, draft),
        process,
    )

    # Prove a real physical selection and undo before exercising IME. The
    # control socket emits the compositor's ordinary key path; it never writes
    # the editor or its CRDT directly.
    set_physical_modifiers(socket, shift=True)
    try:
        for _ in range(5):
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    finally:
        set_physical_modifiers(socket)
    selected = private_editor_observation(access, draft)
    if selected["selection"] != [len(draft) - 5, len(draft)]:
        raise RuntimeError("physical Shift+ArrowLeft did not select the expected editor range")
    require_ok(call(socket, {"op": "key", "key": "ArrowRight"}))
    require_ok(call(socket, {"op": "type", "text": "!"}))
    wait_for("physical edit before undo",
             lambda: private_editor_value_matches(access, draft + "!"), process)
    physical_chord(socket, "z", ctrl=True)
    wait_for("physical Ctrl+Z to restore the private draft",
             lambda: private_editor_value_matches(access, draft), process)

    # The desktop emoji picker is the production input-method route:
    # Super+. → query → Enter commits through zwp_input_method_v2/text_input_v3.
    physical_chord(socket, ".", logo=True)
    require_ok(call(socket, {"op": "type", "text": "thumbs"}))
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    draft += "👍"
    wait_for("real input-method commit in the private editor",
             lambda: private_editor_value_matches(access, draft), process)
    set_physical_modifiers(socket, shift=True)
    try:
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    finally:
        set_physical_modifiers(socket)
    before = private_editor_observation(access, draft)
    if before["selection"] != [len(draft) - 1, len(draft)]:
        raise RuntimeError("the IME result could not be selected through physical input")
    if before["accessibility_focused"] is not True:
        raise RuntimeError("the actual local editor lost accessibility focus before Float")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"before-float-{size}.png")}))
    # Editing publishes fresh AccessKit transactions. Resolve the current
    # drag handle beside the same editor instead of using a retained node.
    access = wait_for(
        "current attached Tide editor and drag handle",
        lambda: agent_conversation_accessibility("Tide"),
        process,
    )
    if private_editor_observation(access, draft) != before:
        raise RuntimeError("refreshing Float changed private editor state")
    primary = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
    if primary is None:
        raise RuntimeError("Chat's primary presentation disappeared before drag")
    geometry = presented_content_geometry(primary)
    drag_x, drag_y = component_global_center(access, access["drag"], geometry, "drag handle")
    desktop_width, desktop_height = (int(value) for value in size.split("x", 1))
    placement = geometry["placement"]
    if placement["x"] >= 140:
        drag_to_x = max(8.0, placement["x"] - 96.0)
    else:
        drag_to_x = min(desktop_width - 8.0, placement["x"] + placement["w"] + 96.0)
    drag_to_y = min(desktop_height - 8.0, max(8.0, drag_y + 48.0))
    require_ok(call(socket, {
        "op": "drag", "x_from": drag_x, "y_from": drag_y,
        "x_to": drag_to_x, "y_to": drag_to_y, "steps": 16,
    }))

    def painted_float():
        return presented_floating_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")

    frame = wait_for("Tide's actual detached native pixels", painted_float, process)
    access = wait_for("detached Tide editor", lambda: agent_conversation_accessibility("Tide", detached=True), process)
    detached = private_editor_observation(access, draft)
    if detached != before:
        raise RuntimeError("Float changed private editor content, caret, selection or keyboard focus")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"after-float-{size}.png")}))
    before_return = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
    if before_return is None:
        raise RuntimeError("Chat's primary presentation disappeared before Return")
    float_geometry = presented_content_geometry(frame)
    return_x, return_y = component_global_center(
        access, access["control"], float_geometry, "Return conversation control"
    )
    require_ok(call(socket, {"op": "click_at", "x": return_x, "y": return_y}))
    wait_for("the detached Chat window to stop presenting", lambda: not floating_surface_presenting(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat", frame["floating_id"]), process)

    def repainted_primary():
        primary = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
        return primary if primary is not None and primary["frame_seq"] > before_return["frame_seq"] else None

    primary = wait_for("Chat's primary window to repaint after Return", repainted_primary, process)
    access = wait_for("returned Tide editor", lambda: agent_conversation_accessibility("Tide"), process)
    attached = private_editor_observation(access, draft)
    if attached != before:
        raise RuntimeError("Return changed private editor content, caret, selection or keyboard focus")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"after-return-{size}.png")}))
    (output / "float-return.json").write_text(json.dumps({
        "physical_typing": True, "physical_selection": True, "physical_undo": True,
        "input_method_commit": True, "pointer_drag_detach": True,
        "pointer_return": True,
        "native_floating_id": frame["floating_id"], "frame_seq": frame["frame_seq"],
        "returned_primary_frame_seq": primary["frame_seq"],
        "private_editor": attached, "private_content_recorded": False,
        "test_owned_draft_rendered": True,
        "lifecycle_proven": False,
    }, indent=2) + "\n")
    return True


class HeldProvider:
    """Bounded host-only HTTP fixture behind the unchanged model gateway.

    Request bodies and authorization values are checked but never stored or
    logged. Only a count/event is evidence; no credentials enter state-dump.
    The fixture cannot initiate a request or invoke any desktop action.
    """
    def __init__(self):
        self.received = threading.Event()
        self.requests = 0
        self.refused_requests = 0
        self.allowed = 0
        self.exact_releases = set()
        self.waiting_ordinals = set()
        self.owner_admissions = {}
        self.owner_admission_probe = None
        self.closed = False
        self.request_lock = threading.Condition()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, _format, *_args):
                pass

            def do_POST(self):
                self.connection.settimeout(5)
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                except ValueError:
                    self.send_error(400)
                    return
                if self.path != "/v1/chat/completions" or not 0 < length <= 262144:
                    self.send_error(400)
                    return
                if self.headers.get("Authorization") != "Bearer isolated-fixture-not-a-real-provider-key":
                    self.send_error(401)
                    return
                try:
                    request = json.loads(self.rfile.read(length))
                except (json.JSONDecodeError, UnicodeDecodeError):
                    self.send_error(400)
                    return
                if not isinstance(request, dict) or request.get("model") != "bounded-fixture":
                    self.send_error(400)
                    return
                probe = owner.owner_admission_probe
                admission = probe() if probe is not None else None
                with owner.request_lock:
                    if owner.requests >= 4:
                        owner.refused_requests += 1
                        self.send_error(429)
                        return
                    owner.requests += 1
                    ordinal = owner.requests
                    owner.owner_admissions[ordinal] = admission
                    owner.request_lock.notify_all()
                owner.received.set()
                if admission is None:
                    self.send_error(409)
                    return
                with owner.request_lock:
                    owner.waiting_ordinals.add(ordinal)
                    released = owner.request_lock.wait_for(
                        lambda: owner.closed or owner.allowed >= ordinal
                        or ordinal in owner.exact_releases, timeout=120 if ordinal == 3 else 30
                    )
                    owner.waiting_ordinals.discard(ordinal)
                if not released or owner.closed:
                    self.send_error(504)
                    return
                reply = {"id": "proof", "model": "bounded-fixture", "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": None,
                    "tool_calls": [{"id": "proof-plan", "type": "function", "function": {
                        "name": "plan", "arguments": json.dumps({"reply": "Fixture response.",
                                                                  "done": True})}}]},
                    "finish_reason": "tool_calls"}]}
                if request.get("stream"):
                    message = reply["choices"][0]["message"]
                    message["tool_calls"][0]["index"] = 0
                    chunk = {"id": "proof", "model": "bounded-fixture", "choices": [{
                        "index": 0, "delta": message, "finish_reason": "tool_calls"}]}
                    body = ("data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n").encode()
                    content_type = "text/event-stream"
                else:
                    body = json.dumps(reply).encode()
                    content_type = "application/json"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def request_count(self):
        with self.request_lock:
            return self.requests

    def overflow_count(self):
        with self.request_lock:
            return self.refused_requests

    def original_third_held(self):
        with self.request_lock:
            return self.allowed == 2 and 3 in self.waiting_ordinals

    def require_owner_admission(self, probe):
        """Require a content-free durable owner phase at request admission."""
        with self.request_lock:
            if self.requests != 0 or self.owner_admission_probe is not None:
                raise RuntimeError("provider owner admission was configured too late")
            self.owner_admission_probe = probe

    def owner_admission(self, ordinal):
        with self.request_lock:
            value = self.owner_admissions.get(ordinal)
            return dict(value) if isinstance(value, dict) else None

    def require_fourth_message_admission(self, probe):
        """Replace only the post-Stop fourth request's content-free probe."""
        with self.request_lock:
            if self.requests != 3 or self.allowed != 2 or self.owner_admission_probe is None:
                raise RuntimeError("fourth Message admission requires the original held third turn")
            self.owner_admission_probe = probe

    def release_exact(self, ordinal):
        """Release the supplemental Message without releasing the stale Task turn."""
        with self.request_lock:
            if ordinal != 4 or self.requests != 4 or self.allowed != 2:
                raise RuntimeError("exact release requires the fourth turn and held original third")
            self.exact_releases.add(ordinal)
            self.request_lock.notify_all()

    def release_through(self, ordinal):
        with self.request_lock:
            if not 0 <= ordinal <= self.requests:
                raise RuntimeError("cannot release an unobserved provider request")
            self.allowed = max(self.allowed, ordinal)
            self.request_lock.notify_all()

    def close(self):
        with self.request_lock:
            self.closed = True
            self.request_lock.notify_all()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("owned provider fixture did not stop")


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


def enable_isolated_accessibility(output, env, processes, logs):
    """Enable AT-SPI only on this proof's private D-Bus session."""
    # Fedora's SELinux policy does not permit the private dbus-daemon domain
    # to transition into gnome_atspi_exec_t. Start the ordinary system
    # launcher as this fixture's unconfined user instead, on the already
    # isolated session bus, and retain it for the whole proof.
    launcher_path = pathlib.Path("/usr/libexec/at-spi-bus-launcher")
    if not launcher_path.is_file() or not os.access(launcher_path, os.X_OK):
        raise RuntimeError("the system AT-SPI bus launcher is unavailable")
    launcher_log = open(output / "at-spi-bus.log", "w")
    logs.append(launcher_log)
    launcher = subprocess.Popen(
        [str(launcher_path), "--launch-immediately"],
        env=env, stdout=launcher_log, stderr=launcher_log,
        start_new_session=True, preexec_fn=die_with_parent(),
    )
    processes.append(launcher)
    endpoint = (
        "--session", "--dest", "org.a11y.Bus", "--object-path", "/org/a11y/bus",
        "--method",
    )

    def accessibility_status():
        try:
            return atspi.command(
                "gdbus", "call", *endpoint,
                "org.freedesktop.DBus.Properties.Get", "org.a11y.Status", "IsEnabled",
            )
        except atspi.GateError:
            return None

    wait_for(
        "the isolated system AT-SPI service",
        accessibility_status,
        launcher,
    )
    atspi.command(
        "gdbus", "call", *endpoint,
        "org.freedesktop.DBus.Properties.Set", "org.a11y.Status", "IsEnabled", "<true>",
    )
    status = accessibility_status()
    if status is None or "true" not in status.casefold():
        raise RuntimeError("isolated AT-SPI session did not enable accessibility")
    address = atspi.atspi_address()
    if not address.startswith("unix:path="):
        raise RuntimeError("isolated AT-SPI service returned an unsupported bus address")
    (output / "accessibility-session.json").write_text(json.dumps({
        "isolated_session": True,
        "enabled": True,
    }, indent=2) + "\n")


def start_isolated_secret_service(output, env, processes, logs):
    """Own one unlocked Secret Service for this disposable login session."""
    if os.geteuid() == 0:
        raise RuntimeError(
            "the native proof must run as its ordinary non-root fixture user"
        )
    daemon_path = pathlib.Path("/usr/bin/gnome-keyring-daemon")
    if not daemon_path.is_file() or not os.access(daemon_path, os.X_OK):
        raise RuntimeError("the system Secret Service daemon is unavailable")
    # D-Bus activation inherits the enclosing test session's environment,
    # before the product gets its still-shorter scratch runtime directory.
    # Put the control socket where that same private bus can discover it and
    # explicitly hand the path to every product child.
    control = pathlib.Path(os.environ["XDG_RUNTIME_DIR"]) / "keyring"
    control.mkdir(mode=0o700)
    env["GNOME_KEYRING_CONTROL"] = str(control)
    keyring_log = open(output / "secret-service.log", "w")
    logs.append(keyring_log)
    keyring = subprocess.Popen(
        [str(daemon_path), "--foreground", "--login", "--components=secrets",
         "--control-directory", str(control)],
        env=env, stdin=subprocess.PIPE, stdout=keyring_log, stderr=keyring_log,
        text=True, start_new_session=True, preexec_fn=die_with_parent(),
    )
    processes.append(keyring)
    if keyring.stdin is None:
        raise RuntimeError("the isolated Secret Service has no login input")
    keyring.stdin.write("\n")
    keyring.stdin.close()

    wait_for(
        "the isolated keyring control socket",
        lambda: (control / "control").is_socket(),
        keyring,
    )

    def unlocked_default_collection():
        try:
            atspi.command(
                "gdbus", "call", "--session", "--dest", "org.freedesktop.secrets",
                "--object-path", "/org/freedesktop/secrets", "--method",
                "org.freedesktop.DBus.Peer.Ping",
            )
            alias = atspi.command(
                "gdbus", "call", "--session", "--dest", "org.freedesktop.secrets",
                "--object-path", "/org/freedesktop/secrets", "--method",
                "org.freedesktop.Secret.Service.ReadAlias", "default",
            )
            match = re.search(r"objectpath '([^']+)'", alias)
            if match is None or match.group(1) == "/":
                return None
            locked = atspi.command(
                "gdbus", "call", "--session", "--dest", "org.freedesktop.secrets",
                "--object-path", match.group(1), "--method",
                "org.freedesktop.DBus.Properties.Get",
                "org.freedesktop.Secret.Collection", "Locked",
            )
            return match.group(1) if "false" in locked.casefold() else None
        except atspi.GateError:
            return None

    wait_for("the isolated unlocked system Secret Service",
             unlocked_default_collection, keyring)
    (output / "secret-service-session.json").write_text(json.dumps({
        "isolated_session": True,
        "unlocked_default_collection": True,
    }, indent=2) + "\n")


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


def require_locked_cargo_metadata(env, profile):
    """Keep source metadata inspection read-only inside the proof worktree."""
    real = pathlib.Path(env.get("CARGO", ""))
    if not real.is_absolute() or not real.is_file():
        raise RuntimeError("the proof requires an absolute existing Cargo binary")
    shim = profile / "locked-cargo-metadata"
    shim.write_text(
        "#!/usr/bin/env python3\n"
        "import os, sys\n"
        f"real = {str(real)!r}\n"
        "args = sys.argv[1:]\n"
        "if args and args[0] == 'metadata':\n"
        "    args.insert(1, '--locked')\n"
        "os.execv(real, [real, *args])\n"
    )
    shim.chmod(0o700)
    env["CARGO"] = str(shim)


def install_packages(binaries, profile, env, *, include_store=False):
    identity = profile / "publisher.json"
    package = binaries / "tilo-app-package"
    run(package, ["keygen", "--out", identity], env)
    attestor = profile / "attestor.json"
    run(package, ["keygen", "--out", attestor], env)
    # The proof shell and governor are dev fixtures: throwaway identity and
    # test control are deliberately unavailable in release builds. Package
    # only the production-profile applications from the matched sibling graph,
    # as the Store lifecycle proof does; never replace them with fat dev apps.
    apps = binaries.parent / "thin-app"
    rust_libraries = subprocess.check_output(["rustc", "--print", "target-libdir"], text=True).strip()
    packages = [("chat", "chat"), ("agent", "tilo-agent")]
    if include_store:
        packages.append(("store", "store"))
    includes = shared_libraries([apps / executable for _, executable in packages],
                                f"{apps}:{rust_libraries}")
    version_file = profile / "runtime-version.txt"
    args = ["build", "--release", "2026.07", "--out-root", profile / "runtimes",
            "--version-out", version_file, "--identity", identity]
    for library in includes:
        args.extend(["--include", library])
    run(binaries / "tilo-runtime-bundle", args, env)
    version = version_file.read_text().strip()
    for directory, executable in packages:
        payload = profile / f"payload-{directory}"
        (payload / "bin").mkdir(parents=True)
        shutil.copy2(apps / executable, payload / "bin" / executable)
        # Native development builds retain debug symbols; the signed product
        # profile strips these too. Strip only the owned payload copy, leaving
        # builder artifacts untouched and enforcing the ordinary tapp ceiling.
        run(pathlib.Path("strip"), ["--strip-all", payload / "bin" / executable], env)
        # Production packaging binds a thin app to the exact runtime graph it
        # was linked against. Change only this signed runtime reference; all
        # app code, sandbox declarations and services remain the authored ones.
        manifest = profile / f"{directory}-manifest.toml"
        authored = (REPO / "apps" / directory / "tilo-app.toml").read_text()
        reference = '"tilo-runtime/2026.07"'
        if authored.count(reference) != 1:
            raise RuntimeError("expected exactly one authored runtime reference")
        manifest.write_text(authored.replace(reference, f'"tilo-runtime/{version}"'))
        tapp = profile / f"{directory}.tapp"
        proof_args = [REPO / "apps" / directory, manifest, payload, identity, tapp]
        if directory == "agent":
            proof_args.append(attestor)
        run(binaries / "examples/proof_package", proof_args, env)
        env["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = str(profile / "runtime-publishers.txt")
        trust = ["--require-attestor", "isolated-desktop-proof"] if directory == "agent" else ["--allow-self-signed"]
        run(binaries / "tilo-install", ["install", tapp, *trust], env)


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


def offered_agent_action(socket, room, record, agent, operation):
    """Use the authenticated published hierarchy, never construct opaque ids."""
    matches = [action for action in actions(socket)
               if action.get("application") == "so.tilo.chat"
               and action.get("room") == room
               and action.get("record") == record
               and action.get("semantic_identity") == {"kind": "chat.agent", "key": f"chat.person:{agent}"}
               and action.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError("ambiguous canonical packaged Chat agent action")
    return matches[0] if matches else None


def public_action_observation(row):
    """Retain routing metadata, never arguments or private editor content."""
    fields = ("provider", "id", "application", "room", "record", "resource",
              "operation", "semantic_identity")
    observed = {key: row.get(key) for key in fields}
    identity = row.get("semantic_identity")
    observed["semantic_identity"] = (
        {key: identity.get(key) for key in ("kind", "key")}
        if isinstance(identity, dict) else None)
    return observed


def terminal_invocation(receipt):
    update = receipt.get("update")
    invocation = update.get("invocation") if isinstance(update, dict) else None
    if not isinstance(invocation, str) or not invocation:
        raise RuntimeError("terminal receipt lost its requested invocation")
    return invocation


def terminal_value(receipt):
    update = receipt.get("update")
    output = update.get("output") if isinstance(update, dict) else None
    value = output.get("value") if isinstance(output, dict) else None
    if not isinstance(value, dict):
        raise RuntimeError("terminal receipt has no typed object result")
    return value


def offered_desktop_action(socket, room, record, resource, operation):
    """Select one owner-published desktop action without decoding aliases."""
    matches = [row for row in actions(socket)
               if row.get("application") == "so.tilo.desktop"
               and row.get("room") == room
               and row.get("record") == record
               and row.get("resource") == resource
               and row.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError(f"ambiguous desktop action {resource}#{operation}")
    return matches[0] if matches else None


def read_task(socket, room, task_id, process):
    action = wait_for("owner-published task read", lambda: offered_desktop_action(
        socket, room, "tasks", f"desktop:task:{task_id}", "read-task"), process)
    return terminal_value(wait_terminal(socket, invoke(socket, action), process))


def current_agent_tasks(socket, room, agent, process):
    action = wait_for("owner-published current task read", lambda: offered_desktop_action(
        socket, room, "tasks", "desktop:tasks", "read-current-tasks"), process)
    return terminal_value(wait_terminal(
        socket, invoke(socket, action, {"agent": agent}), process
    ))


def latest_chat_task_frontier(output, task_id, run_epoch, phase):
    """Read the opt-in native diagnostic without retaining private UI state."""
    path = output / "shell.log"
    if not path.exists():
        return None
    marker = "tilo-physical-input "
    for line in reversed(path.read_text(errors="replace").splitlines()):
        if marker not in line:
            continue
        try:
            row = json.loads(line.split(marker, 1)[1])
        except json.JSONDecodeError:
            continue
        state = row.get("after")
        task = state.get("agent_task") if isinstance(state, dict) else None
        execution = task.get("execution") if isinstance(task, dict) else None
        if not isinstance(execution, dict):
            continue
        if task.get("task_id") == task_id \
                and task.get("run_epoch") == run_epoch \
                and execution.get("run_epoch") == run_epoch \
                and execution.get("phase") == phase:
            return {
                "task_id": task_id,
                "run_epoch": run_epoch,
                "sequence": execution.get("sequence"),
                "phase": phase,
                "private_content_recorded": False,
            }
    return None


def chat_input_observer_enabled(output):
    """Read only the native runner's fixed observer-enabled boolean."""
    path = output / "shell.log"
    if not path.exists():
        return None
    marker = "tilo-physical-input-observer "
    for line in reversed(path.read_text(errors="replace").splitlines()):
        if marker not in line:
            continue
        shell_bound_chat = line.startswith("[app so.tilo.chat stderr] ")
        try:
            row = json.loads(line.split(marker, 1)[1])
        except json.JSONDecodeError:
            continue
        if not shell_bound_chat:
            continue
        app_id = row.get("app_id")
        if app_id not in (None, "so.tilo.chat"):
            continue
        return row.get("enabled") if type(row.get("enabled")) is bool else None
    return None


def observe_chat_task_frontier(socket, output, task_id, run_epoch, phase, process):
    """Require Chat's retained frontier, not only Task Center's owner read."""
    sampled = False

    def observed():
        nonlocal sampled
        observer = chat_input_observer_enabled(output)
        if observer is False:
            raise RuntimeError("Chat's fixed physical-input observer is disabled")
        if observer is None:
            return None
        frame = presented_primary_frame(
            call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
        )
        if frame is None:
            return None
        geometry = presented_content_geometry(frame)
        if not sampled:
            access = agent_conversation_accessibility(
                "Tide",
                diagnostic_path=output / "managed-task-accessibility.json",
            )
            if access is None:
                return None
            physically_click_editor(socket, access, geometry)
            if not editor_accessibility_focus(access):
                return None
            require_chat_keyboard_focus_after_click(socket, output)
            if not task_phase_accessibility(access, "Planning: Lifecycle task"):
                return None
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
            sampled = True
        return latest_chat_task_frontier(output, task_id, run_epoch, phase)

    try:
        return wait_for("Chat's authenticated current-task frontier", observed, process)
    except RuntimeError as error:
        if str(error).startswith("timed out waiting for"):
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / "managed-task-frontier-timeout.png"
            )}))
        raise


def start_managed_agent(socket, room, agent, process):
    """Start through the public owner action, never a fixture-only fallback."""
    published = wait_for(
        "owner-published managed Agent start",
        lambda: offered_desktop_action(
            socket, room, "tasks", "desktop:tasks", "start-agent"
        ),
        process,
    )
    receipt = wait_terminal(
        socket, invoke(socket, published, {"agent": agent}), process
    )
    value = terminal_value(receipt)
    if value.get("startup_requested") is not True:
        raise RuntimeError("managed Agent start did not acknowledge only its launch request")
    return {"semantic": True, "invocation": terminal_invocation(receipt)}


def agent_reply_observation(socket, room, record, parent, agent, process):
    """Read the canonical reply in memory and return content-free evidence."""
    reads = [row for row in actions(socket)
             if row.get("application") == "so.tilo.chat"
             and row.get("room") == room
             and row.get("record") == record
             and row.get("operation") == "read-messages"]
    for read in reads:
        receipt = wait_terminal(
            socket, invoke(socket, read, {"pageSize": 32}), process
        )
        value = terminal_value(receipt)
        content = value.get("content")
        messages = content.get("messages") if isinstance(content, dict) else None
        if not isinstance(messages, list):
            continue
        matches = [message for message in messages
                   if isinstance(message, dict)
                   and message.get("parent") == parent
                   and message.get("author") == f"agent:{agent}"]
        if len(matches) > 1:
            raise RuntimeError("managed Agent produced duplicate canonical replies")
        if matches:
            reply = matches[0]
            if not isinstance(reply.get("id"), str) or not reply["id"] \
                    or reply.get("agentIntent") is not None:
                raise RuntimeError("managed Agent reply metadata is malformed")
            return {"typed_actor": reply["author"], "parent": parent,
                    "reply_id": reply["id"]}
    return None


def canonical_physical_message(socket, room, record, expected, process):
    """Derive the original from an actual saved row; keep fixture text in memory."""
    saved = {}
    for read in actions(socket):
        if read.get("application") != "so.tilo.chat" or read.get("room") != room \
                or read.get("record") != record or read.get("operation") != "read-messages":
            continue
        receipt = wait_terminal(socket, invoke(socket, read, {"pageSize": 32}), process)
        content = terminal_value(receipt).get("content")
        messages = content.get("messages") if isinstance(content, dict) else None
        if not isinstance(messages, list):
            continue
        count_hits = sum(isinstance(row, dict) and row.get("text") == expected for row in messages)
        if count_hits > 1:
            raise RuntimeError("physical Send duplicated the canonical request in one channel")
        for row in messages:
            if not isinstance(row, dict) or row.get("text") != expected:
                continue
            row_id = row.get("id")
            if not isinstance(row_id, str) or re.fullmatch(r"m_[0-9a-f]{32}", row_id) is None \
                    or row.get("parent") not in (None, "") \
                    or row.get("agentIntent") != "message" \
                    or not isinstance(row.get("author"), str) \
                    or not row["author"] or row["author"].startswith("agent:") \
                    or row.get("complete") is not True:
                raise RuntimeError("physical Send saved malformed canonical Person request metadata")
            original = uuid.UUID(hex=row_id[2:])
            if original.int == 0:
                raise RuntimeError("physical Send saved a nil original")
            saved[row_id] = {"invocation": str(original), "request_row": row_id,
                             "channel": content.get("channel")}
    if len(saved) > 1:
        raise RuntimeError("physical Send saved duplicate canonical requests")
    return next(iter(saved.values())) if saved else None


def latest_chat_input_state(output):
    """Read only the latest fixed Chat snapshot, never other app diagnostics."""
    log = output / "shell.log"
    if not log.exists():
        return None
    for line in reversed(log.read_text(errors="replace").splitlines()):
        if not line.startswith("[app so.tilo.chat stderr] "):
            continue
        for marker, key in [("tilo-physical-input ", "after"),
                            ("tilo-physical-frame ", "state")]:
            if marker not in line:
                continue
            try:
                row = json.loads(line.split(marker, 1)[1])
            except json.JSONDecodeError:
                continue
            state = row.get(key) if isinstance(row, dict) else None
            if isinstance(state, dict) and state.get("app") == "so.tilo.chat":
                return state
    return None


def chat_public_message_progress(state, original, phase):
    """Require one fresh mounted-reader milestone for this saved canonical parent."""
    entries = state.get("agent_message_progress") if isinstance(state, dict) else None
    if not isinstance(entries, list) or len(entries) > 32:
        return None
    matches = [entry for entry in entries if isinstance(entry, dict)
               and entry.get("original_invocation") == original["invocation"]]
    if len(matches) > 1:
        raise RuntimeError("Chat projected duplicate progress for one original")
    if not matches:
        return None
    entry = matches[0]
    allowed = {"original_invocation", "request_row", "generation", "phase",
               "unavailable", "reply_row"}
    if set(entry) - allowed or entry.get("request_row") != original["request_row"] \
            or type(entry.get("unavailable")) is not bool:
        raise RuntimeError("Chat public progress correlation is malformed")
    if entry.get("generation") is None and entry.get("phase") is None and "reply_row" not in entry:
        return None  # An initial/unknown read is not a delivery milestone.
    if type(entry.get("generation")) is not int or entry["generation"] <= 0:
        raise RuntimeError("Chat public progress generation is malformed")
    if entry["unavailable"]:
        raise RuntimeError("Chat's public original milestone is unavailable, not current proof")
    if entry.get("phase") == "needs_attention":
        raise RuntimeError("the actual original Message needs attention")
    if entry.get("phase") != phase:
        return None
    if phase == "committed":
        if not isinstance(entry.get("reply_row"), str) \
                or re.fullmatch(r"m_[0-9a-f]{32}", entry["reply_row"]) is None \
                or uuid.UUID(hex=entry["reply_row"][2:]).int == 0:
            raise RuntimeError("Chat's committed milestone lacks a canonical reply row")
    elif "reply_row" in entry:
        raise RuntimeError("an uncommitted Chat milestone asserts a reply row")
    return dict(entry)


def pending_draft_revision(state):
    """Observe exact submission/editor correlation without exporting the draft."""
    pending = state.get("agent_message_submission") if isinstance(state, dict) else None
    if pending is None:
        return None
    if not isinstance(pending, dict) \
            or set(pending) != {"draft_revision", "current_revision", "draft_empty"} \
            or type(pending["draft_revision"]) is not int \
            or type(pending["current_revision"]) is not int \
            or pending["draft_revision"] < 0 \
            or pending["draft_revision"] != pending["current_revision"] \
            or pending["draft_empty"] is not False:
        raise RuntimeError("physical Send did not retain the exact current private draft revision")
    return pending["draft_revision"]


def physical_send_release_revision(output, byte_offset):
    """Use the real Send release's synchronous snapshot, not a later raced frame."""
    log = output / "shell.log"
    if not log.exists():
        return None
    with log.open("rb") as stream:
        stream.seek(byte_offset)
        lines = stream.read().decode(errors="replace").splitlines()
    releases = []
    marker = "tilo-physical-input "
    for line in lines:
        if not line.startswith("[app so.tilo.chat stderr] ") or marker not in line:
            continue
        try:
            row = json.loads(line.split(marker, 1)[1])
        except json.JSONDecodeError:
            continue
        if isinstance(row, dict) and row.get("kind") == "pointer_released":
            releases.append(row)
    if len(releases) > 1:
        raise RuntimeError("physical Send release observation is ambiguous")
    if not releases:
        return None
    row = releases[0]
    before, after = row.get("before"), row.get("after")
    if not isinstance(before, dict) or before.get("app") != "so.tilo.chat" \
            or before.get("agent_message_submission") is not None \
            or not isinstance(after, dict) or after.get("app") != "so.tilo.chat":
        raise RuntimeError("physical Send release does not bind a new Chat submission")
    value = pending_draft_revision(after)
    return {"revision": value} if value is not None else None


def owned_agent_identity(shell_pid):
    """Compare an owned actual process and kernel starttime only in memory."""
    rows = {}
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            rows[int(entry.name)] = (int(fields[1]), fields[19])
        except (OSError, IndexError, ValueError):
            continue
    owned = []
    for pid, (_, started) in rows.items():
        if process_program(pid) != "agent":
            continue
        ancestor, seen = pid, set()
        while ancestor in rows and ancestor not in seen and ancestor != shell_pid:
            seen.add(ancestor)
            ancestor = rows[ancestor][0]
        if ancestor == shell_pid:
            owned.append((pid, started))
    if len(owned) != 1:
        raise RuntimeError("the proof cannot observe one owned managed Agent process")
    return owned[0]


def physical_send_after_stop(socket, room, record, provider, output, size, process, identity):
    """Supplement, never replace, the semantic lifecycle and physical editor journey."""
    if provider.request_count() != 3 or not provider.original_third_held() \
            or owned_agent_identity(process.pid) != identity:
        raise RuntimeError("physical Send requires the same live Agent and original held third turn")
    draft = "Reply after Stop using this physical Send."

    def admission():
        if owned_agent_identity(process.pid) != identity:
            return None
        # The authenticated model POST is the execution boundary. UI polling
        # may legitimately lag it; require mounted progress before release,
        # not as new authority to admit model execution.
        return canonical_physical_message(socket, room, record, draft, process)

    provider.require_fourth_message_admission(admission)
    access = wait_for("returned physical Send editor",
                      lambda: agent_conversation_accessibility("Tide"), process)
    frame = wait_for("physical Send presented Chat", lambda: presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), process)
    physically_click_editor(socket, access, presented_content_geometry(frame))
    require_chat_keyboard_focus_after_click(socket, output)
    physical_chord(socket, "a", ctrl=True)
    require_ok(call(socket, {"op": "type", "text": draft}))
    wait_for("physical Send fixture draft", lambda: private_editor_value_matches(access, draft), process)
    access = wait_for("current physical Send view", lambda: agent_conversation_accessibility("Tide"), process)
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
    buttons = client.named(tree, "Send message")
    if len(buttons) != 1:
        raise RuntimeError("the exact direct-agent editor has no unique physical Send control")
    frame = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
    if frame is None:
        raise RuntimeError("physical Send's presentation disappeared")
    x, y = component_global_center(access, buttons[0], presented_content_geometry(frame), "Send message")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"before-physical-send-{size}.png")}))
    log = output / "shell.log"
    if not log.is_file() or chat_input_observer_enabled(output) is not True:
        raise RuntimeError("physical Send's actual Chat input observer is unavailable")
    send_log_offset = log.stat().st_size
    require_ok(call(socket, {"op": "click_at", "x": x, "y": y}))
    revision = wait_for("actual Send-release private draft revision", lambda: physical_send_release_revision(
        output, send_log_offset), process)["revision"]

    def sample():
        current = agent_conversation_accessibility("Tide")
        current_frame = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
        if current is None or current_frame is None:
            return None
        physically_click_editor(socket, current, presented_content_geometry(current_frame))
        require_chat_keyboard_focus_after_click(socket, output)
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
        return latest_chat_input_state(output)

    original = wait_for("physical Send's actual canonical request",
                        lambda: canonical_physical_message(socket, room, record, draft, process), process)
    wait_for("new authenticated model request after Stop", lambda: provider.request_count() == 4, process)
    admitted = provider.owner_admission(4)
    if admitted != original or not provider.original_third_held():
        raise RuntimeError("fourth model request did not retain this original while the third stayed held")
    wait_for("exact owner receipt clears the captured private draft",
             lambda: private_editor_value_matches(access, ""), process)
    waiting = wait_for("mounted Chat's original WaitingForReply",
                       lambda: chat_public_message_progress(sample(), original, "waiting_for_reply"), process)
    if not task_phase_accessibility(access, "Waiting for reply"):
        raise RuntimeError("the mounted original Message has no actual Waiting for reply label")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"physical-send-waiting-{size}.png")}))
    provider.release_exact(4)
    reply = wait_for("same Agent's canonical physical Send reply", lambda: agent_reply_observation(
        socket, room, record, original["request_row"], "tide", process), process)
    committed = wait_for("mounted Chat's exact committed reply", lambda: chat_public_message_progress(
        sample(), original, "committed"), process)
    if committed["reply_row"] != reply["reply_id"] or not provider.original_third_held() \
            or owned_agent_identity(process.pid) != identity \
            or provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("physical Send replayed work, changed process or lost exact canonical reply custody")
    if not private_editor_value_matches(access, ""):
        raise RuntimeError("physical Send changed the empty acknowledged private draft")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"physical-send-replied-{size}.png")}))
    provider.release_through(3)
    evidence = {"physical_send": True, "same_managed_process": True,
                "original_invocation": original["invocation"], "request_row": original["request_row"],
                "captured_draft_revision": revision, "exact_draft_cleared": True,
                "reply_actor": reply["typed_actor"], "reply_parent": reply["parent"],
                "reply_row": reply["reply_id"], "waiting": waiting, "committed": committed,
                "old_task_response_still_held_until_reply": True, "provider_request_count": 4,
                "private_content_recorded": False, "test_owned_draft_rendered": True}
    (output / "physical-send-after-stop.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def managed_child_diagnostic(profile):
    """Classify bounded managed output without retaining task/model content."""
    logs = list(profile.rglob("launcher-debug-output.log"))
    if len(logs) != 1:
        return {"log_count": len(logs), "line_count": 0, "first_error_kind": None}
    lines = logs[0].read_text(errors="replace").splitlines()
    folded_lines = [line.casefold() for line in lines]
    error_kind = None
    classifiers = [
        ("managed_context_schema_refused", lambda line: "unsupported managed context fields" in line),
        ("managed_context_binding_refused", lambda line: "managed context does not bind" in line),
        ("managed_process_binding_refused", lambda line: "managed process binding refused" in line),
        ("managed_collaboration_env_unset", lambda line: "environment variable not found" in line),
        ("managed_collaboration_service_fd_unavailable", lambda line: (
            "service fd env var missing or fd is not open" in line
        )),
        ("managed_collaboration_fd_closed", lambda line: "bad file descriptor" in line),
        ("managed_collaboration_handshake_eof", lambda line: "failed to fill whole buffer" in line),
        ("managed_collaboration_handshake_timeout", lambda line: (
            "resource temporarily unavailable" in line or "timed out" in line
        )),
        ("managed_collaboration_peer_closed", lambda line: (
            "connection reset" in line or "broken pipe" in line
        )),
        ("managed_collaboration_primary_timeout", lambda line: (
            "collaboration primary route timed out" in line
        )),
        ("managed_action_catalogue_subscribe_refused", lambda line: (
            "subscribing to action catalogue failed" in line
        )),
        ("managed_action_catalogue_initial_read_refused", lambda line: (
            "reading initial action catalogue failed" in line
        )),
        ("managed_action_catalogue_initial_decode_refused", lambda line: (
            "decoding initial action catalogue failed" in line
        )),
        ("managed_join_identity_missing", lambda line: (
            "requires a certified device presentation" in line
            or "workspace identity has no certified device" in line
        )),
        ("managed_join_worker_identity_mismatch", lambda line: (
            "worker descriptor route or certificate mismatch" in line
            or "registered worker exact certificate changed" in line
        )),
        ("managed_join_person_kind_refused", lambda line: (
            "managed agent process must join as its assigned agent" in line
        )),
        ("managed_join_agent_binding_refused", lambda line: (
            "agent collaboration join rejected" in line
            or "requested agent is not an active workspace member" in line
            or "agent session has no workspace authority" in line
        )),
        ("managed_join_process_route_refused", lambda line: (
            "managed process and admitted agent route differ" in line
            or "managed agent custody belongs to another current route" in line
            or "agent has no actual managed process custody" in line
            or "connection managed process custody changed" in line
        )),
        ("managed_join_person_authority_refused", lambda line: (
            "only a workspace administrator may launch a delegated agent session" in line
            or "certified collaboration identity is not a workspace member" in line
        )),
        ("enrolled_report_sequence_refused", lambda line: (
            "enrolled task report sequence" in line
            or "enrolled report sequence" in line
        )),
        ("enrolled_report_binding_refused", lambda line: (
            "enrolled report" in line
            or "accepted enrolled task" in line
            or "enrolled task subscription closed" in line
        )),
        ("enrolled_owner_update_refused", lambda line: (
            "owner did not retain" in line
            or "canonical task" in line
            or "task run changed before" in line
        )),
        ("enrolled_acknowledgement_uncertain", lambda line: (
            "enrolled rpc acknowledgement uncertain" in line
        )),
        ("managed_join_identity_refused", lambda line: (
            "collaboration join identity rejected" in line
            or "host certificate requires a private host worker descriptor" in line
            or "collaboration join proof was signed by an unknown device" in line
        )),
        ("managed_collaboration_rpc_refused", lambda line: "capnp error" in line),
        ("managed_collaboration_descriptor_failed", lambda line: (
            "tilo_svc_collab_fd" in line or "managed collaboration descriptor" in line
        )),
        ("managed_collaboration_client_failed", lambda line: "start typed collaboration client" in line),
        ("managed_cli_context_refused", lambda line: "managed agent requires task mode" in line),
        ("managed_runtime_link_failed", lambda line: "error while loading shared libraries" in line),
        ("task_subscription_refused", lambda line: (
            "subscribe" in line and ("refus" in line or "error" in line)
        )),
        ("managed_context_invalid", lambda line: (
            "managed context" in line and ("invalid" in line or "missing" in line)
        )),
        ("collaboration_join_refused", lambda line: (
            "collaboration" in line and ("refus" in line or "error" in line)
        )),
        ("model_gateway_unavailable", lambda line: (
            "model gateway" in line and ("unavailable" in line or "error" in line)
        )),
    ]
    for kind, matches in classifiers:
        if any(matches(line) for line in folded_lines):
            error_kind = kind
            break
    if error_kind is None and any("permission denied" in line for line in folded_lines):
        error_kind = "managed_sandbox_permission_refused"
    if error_kind is None and any("no such file or directory" in line for line in folded_lines):
        error_kind = "managed_required_file_missing"
    if error_kind is None and any(
            "error" in line or "panicked" in line for line in folded_lines):
        error_kind = "other_managed_error"
    return {
        "log_count": 1,
        "line_count": len(lines),
        "channel_joined_logged": any(
            "managed agent collaboration channel joined" in line for line in folded_lines
        ),
        "catalogue_subscribed_logged": any(
            "managed agent action catalogue subscribed" in line for line in folded_lines
        ),
        "joined_room_logged": any("joined the room" in line for line in lines),
        "waiting_for_task_runs_logged": any(
            "waiting for broker-addressed task runs" in line for line in lines
        ),
        "subscription_acknowledged": any(
            "managed agent enrolled subscription acknowledged" in line
            for line in folded_lines
        ),
        "stream_delivered": any(
            "managed agent enrolled task stream delivered" in line
            for line in folded_lines
        ),
        "stream_received": any(
            "managed agent enrolled task stream received" in line
            for line in folded_lines
        ),
        "stream_read_refused": any(
            "managed agent enrolled task stream read refused" in line
            for line in folded_lines
        ),
        "event_delivered": any(
            "managed agent enrolled task event delivered" in line
            for line in folded_lines
        ),
        "event_delivery_failed": any(
            "managed agent enrolled task event delivery failed" in line
            for line in folded_lines
        ),
        "event_observed": any(
            "managed agent enrolled task event observed" in line
            for line in folded_lines
        ),
        "acceptance_acknowledged": any(
            "managed agent enrolled task acceptance acknowledged" in line
            for line in folded_lines
        ),
        "acceptance_rpc_started": any(
            "managed agent task acceptance rpc started" in line
            for line in folded_lines
        ),
        "acceptance_rpc_acknowledged": any(
            "managed agent task acceptance rpc acknowledged" in line
            for line in folded_lines
        ),
        "acceptance_rpc_refused": any(
            "managed agent task acceptance rpc refused" in line
            for line in folded_lines
        ),
        "acceptance_reply_delivery_failed": any(
            "managed agent task acceptance reply delivery failed" in line
            for line in folded_lines
        ),
        "execution_entered": any(
            "managed agent enrolled task execution entered" in line
            for line in folded_lines
        ),
        "progress_acknowledged": any(
            "managed agent enrolled progress acknowledged" in line
            for line in folded_lines
        ),
        "catalogue_observed": any(
            "managed agent task catalogue observed" in line
            for line in folded_lines
        ),
        "catalogue_unavailable": any(
            "managed agent task catalogue unavailable" in line
            for line in folded_lines
        ),
        "model_turn_entered": any(
            "managed agent model turn entered" in line
            for line in folded_lines
        ),
        "first_error_kind": error_kind,
    }


NATIVE_DELIVERY_STAGES = {
    "source_stage", "prepare", "sign", "upload_open", "upload_write", "owner_send",
}
NATIVE_DELIVERY_OUTCOMES = {
    "in_flight", "complete", "definitive_refusal", "uncertain", "saved",
}


def native_delivery_diagnostic(output):
    """Return only closed native-delivery state relayed from ordinary Chat."""
    log = output / "shell.log"
    if not log.is_file():
        return {"log_count": 0, "marker_count": 0,
                "markers": [], "stage": None, "outcome": "unobserved"}
    with log.open("rb") as stream:
        stream.seek(0, os.SEEK_END)
        length = stream.tell()
        stream.seek(max(0, length - 256 * 1024))
        tail = stream.read(256 * 1024).decode(errors="replace")
    observed = []
    ansi = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
    marker = re.compile(
        r"^\[app so\.tilo\.chat (?:stdout|stderr)\]\s+"
        r"(?:\d{4}-\d\d-\d\dT\S+\s+)?DEBUG\s+tilo_agent_delivery:\s+"
        r"native-delivery:([a-z_]+):([a-z_]+)\s*$"
    )
    for line in tail.splitlines()[-4096:]:
        matched = marker.fullmatch(ansi.sub("", line))
        if matched is None:
            continue
        stage, outcome = matched.groups()
        if stage in NATIVE_DELIVERY_STAGES and outcome in NATIVE_DELIVERY_OUTCOMES:
            observed.append((stage, outcome))
    if not observed:
        return {"log_count": 1, "marker_count": 0,
                "markers": [], "stage": None, "outcome": "unobserved"}
    stage, outcome = observed[-1]
    return {"log_count": 1, "marker_count": len(observed),
            "markers": [{"stage": marker[0], "outcome": marker[1]}
                        for marker in observed[-32:]],
            "stage": stage, "outcome": outcome}


def finalize_fixture(profile, output, processes, logs, provider, runtime_owner):
    """Save the closed classifier, then always tear down test-owned state."""
    try:
        try:
            diagnostic = native_delivery_diagnostic(output)
            (output / "native-message-delivery-final.json").write_text(
                json.dumps(diagnostic, indent=2) + "\n"
            )
        except OSError:
            raise RuntimeError("native delivery diagnostic unavailable") from None
    finally:
        for process in reversed(processes):
            # Each test-owned process gets its own session. Confined launcher
            # descendants inherit this group; terminate the whole owned group,
            # not a pattern that might match an operator's real desktop.
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
        if provider is not None:
            provider.close()
        if runtime_owner is not None:
            runtime_owner.cleanup()
        shutil.rmtree(profile)


def managed_task_and_message_lifecycle(
        socket, room, record, provider, output, process, profile, task_only=False):
    """Prove one installed Agent services Task and Message concurrently."""
    agent = "tide"
    task_title = "Lifecycle task"
    queue = wait_for("owner-published Queue task action", lambda: offered_desktop_action(
        socket, room, "tasks", "desktop:tasks", "queue"), process)
    queue_receipt = wait_terminal(socket, invoke(socket, queue, {
        "instruction": "Hold this task while responding to the direct message.",
        "title": task_title,
        "agent": agent,
        "route": "model",
    }), process)
    queue_value = terminal_value(queue_receipt)
    queued = queue_value.get("result", {}).get("queued")
    task_id = queued.get("task_id") if isinstance(queued, dict) else None
    if not isinstance(task_id, str) or not task_id \
            or queue_value.get("delivery") != "queued":
        raise RuntimeError("offline Queue did not return one durable queued task")

    # Persist the durable owner result before starting the managed process so
    # a later execution failure cannot erase proof that Queue itself settled.
    # The instruction and title intentionally never enter this artifact.
    (output / "managed-queue-terminal.json").write_text(json.dumps({
        "invocation": terminal_invocation(queue_receipt),
        "task_id": task_id,
        "delivery": queue_value["delivery"],
        "private_fixture_content_recorded": False,
    }, indent=2) + "\n")

    def exact_active_task():
        current = current_agent_tasks(socket, room, agent, process)
        tasks = current.get("tasks")
        if current.get("complete") is not True or not isinstance(tasks, list):
            raise RuntimeError("owner current-task projection is incomplete")
        matches = [task for task in tasks
                   if isinstance(task, dict) and task.get("task_id") == task_id]
        if len(matches) > 1:
            raise RuntimeError("owner projected the queued task more than once")
        return matches[0] if matches else None

    def owner_phase_at_model_admission():
        task = exact_active_task()
        execution = task.get("execution") if task else None
        if not isinstance(execution, dict):
            return None
        epoch = task.get("run_epoch")
        if type(epoch) is not int or epoch <= 0 or execution.get("run_epoch") != epoch:
            return None
        phase = execution.get("phase")
        sequence = execution.get("sequence")
        if (phase == "accepted" and sequence == 0) \
                or (phase == "planning" and type(sequence) is int and sequence > 0):
            return {"phase": phase, "sequence": sequence, "run_epoch": epoch}
        return None

    # The provider checks the durable owner phase at the actual request
    # admission boundary. A Planning phase is causal evidence of prior
    # Accepted for the same epoch: the owner state machine refuses Planning
    # before Accepted and refuses epoch changes. No prompt or task content is
    # retained by this fixture.
    provider.require_owner_admission(owner_phase_at_model_admission)
    startup = start_managed_agent(socket, room, agent, process)

    # Start acknowledges only the authenticated launch request. Keep it
    # distinct from later evidence that the process subscribed or ran work.
    (output / "managed-start-terminal.json").write_text(json.dumps({
        "invocation": startup["invocation"],
        "startup_requested": startup["semantic"],
        "private_fixture_content_recorded": False,
    }, indent=2) + "\n")

    def accepted_before_request():
        task = exact_active_task()
        execution = task.get("execution") if task else None
        if isinstance(execution, dict) and execution.get("phase") == "accepted":
            if execution.get("sequence") != 0:
                raise RuntimeError("initial owner acceptance did not retain sequence zero")
            return task
        if provider.request_count() < 1:
            return None
        admission = provider.owner_admission(1)
        if admission is None:
            raise RuntimeError("provider request lacked durable owner acceptance custody")
        if not task or task.get("run_epoch") != admission.get("run_epoch"):
            raise RuntimeError("provider request changed the accepted owner run epoch")
        return task

    accepted = wait_for(
        "owner acceptance before the first model request", accepted_before_request, process
    )
    accepted_epoch = accepted.get("run_epoch")
    if type(accepted_epoch) is not int or accepted_epoch <= 0:
        raise RuntimeError("accepted task has no exact owner run epoch")
    try:
        wait_for("managed Agent's first real model request",
                 lambda: provider.request_count() >= 1, process)
    except RuntimeError:
        # Distinguish a process that never subscribed from a delivered run
        # that stopped before the model boundary. This owner projection is
        # deliberately content-free: no instruction, title or model data.
        current = current_agent_tasks(socket, room, agent, process)
        tasks = current.get("tasks")
        pending = current.get("pending")
        rows = []
        if isinstance(tasks, list):
            rows.extend(tasks)
        if isinstance(pending, list):
            rows.extend(pending)
        matches = [task for task in rows
                   if isinstance(task, dict) and task.get("task_id") == task_id]
        task = matches[0] if len(matches) == 1 else None
        (output / "failed-managed-first-request.json").write_text(json.dumps({
            "queue_offered": True,
            "queued_receipt": True,
            "start_acknowledged": True,
            "provider_request_count": provider.request_count(),
            "owner_task_status": task.get("status") if task else None,
            "owner_run_epoch_present": bool(
                task and isinstance(task.get("run_epoch"), int)
                and task["run_epoch"] > 0
            ),
            "owner_projection_complete": current.get("complete") is True,
            "owner_pending_projection_complete": current.get("pending_complete") is True,
            "managed_child": managed_child_diagnostic(profile),
            "private_fixture_content_recorded": False,
        }, indent=2) + "\n")
        raise
    provider_admission = provider.owner_admission(1)
    if provider_admission is None or provider_admission.get("run_epoch") != accepted_epoch:
        raise RuntimeError("provider admission did not retain the accepted owner epoch")
    (output / "managed-provider-admission.json").write_text(json.dumps({
        "owner_phase": provider_admission["phase"],
        "owner_sequence": provider_admission["sequence"],
        "owner_run_epoch": provider_admission["run_epoch"],
        "private_fixture_content_recorded": False,
    }, indent=2) + "\n")

    def planning_frontier():
        task = exact_active_task()
        execution = task.get("execution") if task else None
        if not isinstance(execution, dict) or execution.get("phase") != "planning":
            return None
        if task.get("run_epoch") != accepted_epoch \
                or execution.get("run_epoch") != accepted_epoch:
            raise RuntimeError("Planning changed the accepted owner run epoch")
        sequence = execution.get("sequence")
        if type(sequence) is not int or sequence <= 0:
            raise RuntimeError("Planning did not retain an authenticated report sequence")
        return task

    active = wait_for(
        "queued task's authenticated Planning phase", planning_frontier, process
    )
    epoch = active.get("run_epoch")
    after = active.get("after_revision")
    if type(epoch) is not int or epoch <= 0 or type(after) is not int:
        raise RuntimeError("active task has no exact owner frontier")
    planning_sequence = active["execution"]["sequence"]
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / "managed-task-owner-planning.png"
    )}))
    chat_frontier = observe_chat_task_frontier(
        socket, output, task_id, epoch, "planning", process
    )
    if chat_frontier.get("sequence") != planning_sequence:
        raise RuntimeError("Chat retained a different authenticated Planning sequence")
    (output / "managed-chat-task-frontier.json").write_text(
        json.dumps(chat_frontier, indent=2) + "\n"
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / "managed-task-planning.png"
    )}))

    if task_only:
        provider.release_through(1)

        def completed_task():
            task = read_task(socket, room, task_id, process)
            status = task.get("status")
            if status in {"failed", "cancelled"}:
                raise RuntimeError(f"managed task reached unexpected terminal status {status}")
            return task if status == "done" else None

        terminal = wait_for("managed task's durable owner completion", completed_task, process)
        evidence = {
            "queue_invocation": terminal_invocation(queue_receipt),
            "task_id": task_id,
            "queued_delivery": queue_value["delivery"],
            "start_invocation": startup["invocation"],
            "semantic_start": startup["semantic"],
            "run_epoch": epoch,
            "stable_task_target_observed": True,
            "chat_planning_frontier_observed": True,
            "execution_phases": [
                {"phase": "accepted", "sequence": 0},
                {"phase": "planning", "sequence": planning_sequence},
            ],
            "provider_request_count": provider.request_count(),
            "terminal_status": terminal["status"],
            "private_fixture_content_recorded": False,
        }
        (output / "managed-task-lifecycle.json").write_text(
            json.dumps(evidence, indent=2) + "\n"
        )
        return evidence

    message_action = wait_for("authenticated direct Message action", lambda: offered_agent_action(
        socket, room, record, agent, "message"), process)
    message_receipt = wait_terminal(socket, invoke(socket, message_action, {
        "value": "Reply while the queued task remains active.",
    }), process)
    message_invocation = terminal_invocation(message_receipt)
    if re.fullmatch(r"[0-9a-fA-F-]{36}", message_invocation) is None:
        raise RuntimeError("direct Message lost its canonical invocation UUID")
    parent = "m_" + message_invocation.replace("-", "").lower()
    wait_for("parallel direct Message model request",
             lambda: provider.request_count() >= 2, process)

    direction = wait_for("exact-run Direction action", lambda: offered_desktop_action(
        socket, room, "tasks", f"desktop:task:{task_id}", "direct"), process)
    direction_receipt = wait_terminal(socket, invoke(socket, direction, {
        "run_epoch": epoch,
        "after_revision": after,
        "text": "Keep the current task active after acknowledging this direction.",
    }), process)
    direction_value = terminal_value(direction_receipt)
    added = direction_value.get("result", {}).get("direction_added")
    revision = added.get("revision") if isinstance(added, dict) else None
    if added is None or added.get("task_id") != task_id \
            or type(revision) is not int or revision <= after:
        raise RuntimeError("Direction did not save the exact next task revision")

    # Let only the stale task turn settle. The direct Message turn stays held,
    # and the rebuilt task turn remains held later so Stop is observable while
    # genuine work is still current.
    provider.release_through(1)

    def applied_frontier():
        task = read_task(socket, room, task_id, process)
        if task.get("run_epoch") != epoch:
            raise RuntimeError("task run epoch changed before Direction acknowledgement")
        return task if task.get("applied_revision") == revision else None

    applied = wait_for("Agent's exact Direction applied acknowledgement",
                       applied_frontier, process)
    wait_for("rebuilt task model request", lambda: provider.request_count() >= 3, process)
    provider.release_through(2)
    reply = wait_for("Agent's canonical direct Message reply", lambda: agent_reply_observation(
        socket, room, record, parent, agent, process), process)

    stopped_from = read_task(socket, room, task_id, process)
    stop_after = stopped_from.get("after_revision")
    if stopped_from.get("status") != "in_progress" \
            or stopped_from.get("run_epoch") != epoch or stop_after != revision:
        raise RuntimeError("Stop did not bind the acknowledged current frontier")
    stop = wait_for("exact-run Stop action", lambda: offered_desktop_action(
        socket, room, "tasks", f"desktop:task:{task_id}", "stop"), process)
    before_stop_identity = owned_agent_identity(process.pid)
    stop_receipt = wait_terminal(socket, invoke(socket, stop, {
        "run_epoch": epoch,
        "after_revision": stop_after,
    }), process)

    def cancelled_frontier():
        task = read_task(socket, room, task_id, process)
        return task if task.get("status") == "cancelled" else None

    cancelled = wait_for("actual Agent cancellation acknowledgement",
                         cancelled_frontier, process)
    evidence = {
        "queue_invocation": terminal_invocation(queue_receipt),
        "task_id": task_id,
        "queued_delivery": queue_value["delivery"],
        "start_invocation": startup["invocation"],
        "semantic_start": startup["semantic"],
        "run_epoch": epoch,
        "message_invocation": message_invocation,
        "reply_actor": reply["typed_actor"],
        "reply_parent": reply["parent"],
        "reply_while_task_working": True,
        "direction_invocation": terminal_invocation(direction_receipt),
        "saved_revision": revision,
        "applied_revision": applied["applied_revision"],
        "stop_invocation": terminal_invocation(stop_receipt),
        "terminal_status": cancelled["status"],
        "private_fixture_content_recorded": False,
    }
    (output / "managed-task-message-lifecycle.json").write_text(
        json.dumps(evidence, indent=2) + "\n"
    )
    # Inert process correlation is kept only in memory, never the evidence file.
    evidence["_owned_process_identity"] = before_stop_identity
    return evidence


def offered_store_chat(socket, room, record=""):
    """Select the published installed-app door; its parent stays SDK-owned."""
    matches = [row for row in actions(socket)
               if row.get("application") == "so.tilo.store"
               and row.get("room") == room
               and row.get("record") == record
               and row.get("semantic_identity") == {"kind": "store.app", "key": "app:so.tilo.chat"}
               and row.get("operation") == "open"]
    if len(matches) > 1:
        raise RuntimeError("ambiguous canonical Store installed Chat action")
    return matches[0] if matches else None


def admit_native_agents(socket, room, record, process, output):
    """Add real governed Agent members, then observe Chat's admitted roster.

    The preset is membership, not action authority. No app record, person row,
    task, conversation or provider output is fabricated by this fixture.
    """
    def membership_action(resource, operation):
        matches = [row for row in actions(socket)
                   if row.get("application") == "so.tilo.desktop"
                   and row.get("room") == room
                   and row.get("record") == "membership"
                   and row.get("resource") == resource
                   and row.get("operation") == operation]
        if len(matches) > 1:
            raise RuntimeError("ambiguous native workspace membership action")
        return matches[0] if matches else None

    receipts = []
    for agent in ["tide", "reed"]:
        if offered_agent_action(socket, room, record, agent, "open-agent-conversation"):
            raise RuntimeError("scratch Chat improperly started with an admitted fixture agent")
        add = wait_for("normal native Add agent action", lambda: membership_action(
            "desktop:workspace", "add-agent"), process)
        receipt = wait_terminal(socket, invoke(socket, add, {
            "agent": agent, "preset": "operator",
        }), process, public_failure_path=output / f"failed-agent-{agent}-membership-terminal.json")
        # Retain the exact completed step even if the next consumer fails.
        # This is a public action receipt, never a draft or channel capability.
        (output / f"agent-{agent}-membership-terminal.json").write_text(json.dumps({
            "agent": agent, "preset": "operator", "terminal": receipt,
        }, indent=2) + "\n")
        # A terminal receipt alone does not prove that the authoritative roster
        # and its consumers actually changed. Observe both published doors.
        remove = wait_for("governed agent membership readback", lambda: membership_action(
            f"desktop:agent:{agent}", "remove"), process)
        opened = wait_for("Chat's authenticated agent roster readback", lambda: offered_agent_action(
            socket, room, record, agent, "open-agent-conversation"), process)
        receipts.append({"agent": agent, "preset": "operator", "terminal": receipt,
                         "membership_resource": remove["resource"],
                         "chat_semantic_identity": opened["semantic_identity"]})
    # Ensure admitting the second agent did not replace the first.
    for agent in ["tide", "reed"]:
        if not membership_action(f"desktop:agent:{agent}", "remove") \
                or not offered_agent_action(socket, room, record, agent, "open-agent-conversation"):
            raise RuntimeError("native two-agent roster did not retain both members")
    return receipts


def inspect_native_owner(binaries, env, room, *, enrollment=False):
    mode = "--check-native-enrollment" if enrollment else "--check-native-document"
    result = subprocess.run([
        str(binaries / "examples/packaged_agent_lifecycle"), mode,
        env["TILO_STATE_PATH"], room, "",
    ], env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    if result.returncode:
        return None
    observed = json.loads(result.stdout)
    if observed.get("room") != room or type(observed.get("generation")) is not int:
        raise RuntimeError("exact normal owner observation is malformed")
    return observed


def enroll_native(socket, invitation, shell, binaries, env):
    """Redeem the actual signed invitation through the ordinary Connect UI."""
    invoke(socket, require_action(socket, "peer.connect", "connect"))

    def connect_prompt():
        chrome = chrome_state(socket)
        if "peer_sync" not in chrome:
            raise RuntimeError("ordinary Connect prompt observation is absent")
        return chrome["peer_sync"].get("connect_input") is not None

    wait_for("ordinary Connect prompt", connect_prompt, shell)
    require_ok(call(socket, {"op": "type", "text": invitation["invitation"]}))
    require_ok(call(socket, {"op": "key", "key": "Enter"}))

    def admitted():
        chrome = chrome_state(socket)
        status = chrome.get("peer_sync", {}).get("status") or ""
        if "refused" in status.lower():
            raise RuntimeError("canonical native enrollment was refused")
        return status.startswith("Joined") and invitation["room"] in status

    wait_for("acknowledged canonical native enrollment", admitted, shell)
    require_ok(call(socket, {"op": "key", "key": "Escape"}))
    # The status label acknowledges the signed join before the owner snapshot
    # is durably installed. Read the exact owner record without publishing.
    native = wait_for("exact current-device native room owner", lambda: inspect_native_owner(
        binaries, env, invitation["room"], enrollment=True), shell)
    if (native.get("admitted") is not True
            or native.get("governor_is_current_device") is not True
            or native.get("pinned_host") is not False
            or native.get("workspace_administration_permitted") is not True):
        raise RuntimeError("signed join did not retain genuine current-device owner custody")
    return native


def bootstrap(binaries, output, native=False, size="1600x900", lifecycle=False,
              launch_path="applications", task_only=False):
    if launch_path not in ("applications", "store") or (launch_path == "store" and not native):
        raise RuntimeError("Store launch proof requires the native signed workspace")
    env, profile = scratch_session_env()
    processes = []
    logs = []
    provider = None
    runtime_owner = None
    lifecycle_evidence = None
    float_return_proven = False
    physical_send_evidence = None
    try:
        # Keep Unix socket paths short enough for the production private IME
        # displays. This is still an exclusively owned mode-0700 directory,
        # never a shared socket name or the operator's session runtime.
        runtime_owner = tempfile.TemporaryDirectory(prefix="tilo-proof-run-")
        runtime = pathlib.Path(runtime_owner.name)
        socket = str(runtime / "control.sock")
        env.update({"XDG_RUNTIME_DIR": str(runtime), "TILO_TEST_CONTROL": socket,
                    "TILO_APPS_DATA_DIR": str(profile / "apps"),
                    "TILO_RUNTIME_BUNDLE_DIR": str(profile / "runtimes"),
                    "TILO_LAUNCHER_BIN": str(binaries / "tilo-launcher"),
                    "TILO_STATE_PATH": str(profile / "config/tilo/state.json"),
                    "TILO_HEADLESS_SIZE": size})
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        enable_isolated_accessibility(output, env, processes, logs)
        start_isolated_secret_service(output, env, processes, logs)
        require_locked_cargo_metadata(env, profile)
        install_packages(binaries, profile, env, include_store=launch_path == "store")
        installed = subprocess.check_output([str(binaries / "tilo-install"), "list"], env=env, text=True)
        (output / "installed-packages.txt").write_text(installed)
        if "so.tilo.chat" not in installed or "so.tilo.agent" not in installed:
            raise RuntimeError("ordinary signed installation did not publish both package ids")
        if launch_path == "store" and "so.tilo.store" not in installed:
            raise RuntimeError("ordinary signed installation did not publish Store")
        invitation = None
        if native:
            invitation_path = profile / "native-invitation.json"
            governor_log = open(output / "governor.log", "w")
            logs.append(governor_log)
            governor = subprocess.Popen(
                [str(binaries / "examples/packaged_native_governor"), str(profile), str(invitation_path)],
                env=env, stdout=governor_log, stderr=governor_log,
                start_new_session=True, preexec_fn=die_with_parent())
            processes.append(governor)
            wait_for("bounded signed native governor fixture", invitation_path.exists, governor)
            invitation = json.loads(invitation_path.read_text())
            if invitation.get("empty_app_records") is not True:
                raise RuntimeError("native fixture improperly seeded application records")
        provider = HeldProvider()
        run(binaries / "examples/proof_provider", [profile / "config", provider.endpoint], env)
        gateway_socket = runtime / "tilo/services/model-gateway.sock"
        gateway_socket.parent.mkdir(parents=True, mode=0o700)
        gateway_log = open(output / "gateway.log", "w")
        logs.append(gateway_log)
        gateway = subprocess.Popen([str(binaries / "tilo-model-gateway"), "serve", "--socket", str(gateway_socket)],
                                   env=env, stdout=gateway_log, stderr=gateway_log,
                                   start_new_session=True, preexec_fn=die_with_parent())
        processes.append(gateway)
        wait_for("normal model-gateway listener", gateway_socket.exists, gateway)
        log = open(output / "shell.log", "w")
        logs.append(log)
        shell_args = [str(binaries / "shell"), "--backend", "headless"]
        if native:
            shell_args.append("--enable-peer-sync")
        shell_env = env.copy()
        # Observe only the ordinary broker's bounded lifecycle diagnostics;
        # this scratch proof never enables payload/credential tracing.
        shell_env["RUST_LOG"] = (
            "warn,tilo_action_broker::collab_broker_capnp=debug,"
            "tilo_agent=info,"
            "tilo_agent_delivery=debug,"
            "shell::shell::search::providers::live_app_actions=debug,"
            "shell::shell::search::action_api=debug,tilo_native_action_gate=debug"
        )
        # Existing bounded launcher diagnostic: managed child output otherwise
        # goes to /dev/null. It changes no sandbox, custody or service route.
        shell_env["TILO_LAUNCHER_DEBUG_STDERR"] = "1"
        shell_env["TILO_PROFILE_PRESENTATION"] = "1"
        shell = subprocess.Popen(shell_args,
                                 env=shell_env, stdout=log, stderr=log, start_new_session=True,
                                 preexec_fn=die_with_parent())
        processes.append(shell)
        wait_for("isolated compositor control socket", lambda: pathlib.Path(socket).exists(), shell)
        provision = call(socket, {"op": "provision_test_identity"})
        if not provision.get("answer", {}).get("throwaway"):
            raise RuntimeError("scratch session did not provision a throwaway identity")
        if native:
            enrollment = enroll_native(socket, invitation, shell, binaries, env)
            (output / "native-enrollment.json").write_text(json.dumps(enrollment, indent=2) + "\n")
            def app_authorship_published():
                matches = [row for row in actions(socket)
                           if row.get("application") == "so.tilo.desktop"
                           and row.get("room") == enrollment["room"]
                           and row.get("operation") == "grant-app-authorship"]
                if len(matches) > 1:
                    raise RuntimeError("native app authorship publication is ambiguous")
                return matches[0] if matches else None
            # Merely observe the normal broker door. The ordinary Applications
            # action below must invoke it itself; this harness never grants.
            try:
                wait_for("native governor's ordinary app authorship surface", app_authorship_published, shell)
            except RuntimeError:
                fields = ["provider", "id", "application", "room", "record", "resource",
                          "operation", "semantic_identity"]
                observed = [{key: row.get(key) for key in fields} for row in actions(socket)]
                (output / "failed-authorship-catalog.json").write_text(
                    json.dumps(observed, indent=2) + "\n")
                (output / "failed-authorship-native-room.json").write_text(
                    json.dumps(chrome_state(socket).get("native_room"), indent=2) + "\n")
                raise
        launch_application = "so.tilo.store" if launch_path == "store" else "so.tilo.chat"
        def offered_application():
            published = actions(socket, "Store" if launch_path == "store" else "Chat")
            app_rows = [{key: row.get(key) for key in ["provider", "id", "title", "kind"]}
                        for row in published if row["provider"] == "applications"]
            (output / "offered-applications.json").write_text(json.dumps(app_rows, indent=2) + "\n")
            matches = [action for action in published
                       if action["provider"] == "applications" and action["id"] == launch_application]
            if len(matches) > 1:
                raise RuntimeError("signed application launch is ambiguous")
            return matches[0] if matches else None

        application = wait_for("indexed normal signed application", offered_application, shell)
        opening = invoke(socket, application)
        (output / "application-open-ack.json").write_text(json.dumps(opening, indent=2) + "\n")
        if launch_path == "store":
            store_chat = wait_for("Store's real installed Chat Open action", lambda: offered_store_chat(
                socket, enrollment["room"]), shell)
            # The real Store effect carries its actual invoking Person through
            # the normal SDK launch channel. Never manufacture a parent token,
            # caller, permission grant or alternate RPC client here.
            opening = invoke(socket, store_chat)
            (output / "store-chat-open-ack.json").write_text(json.dumps(opening, indent=2) + "\n")

        def painted_chat():
            return presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")

        try:
            frame = wait_for("normal signed Chat to paint", painted_chat, shell)
        except RuntimeError:
            state = chrome_state(socket)
            receipts = []
            for job in actions(socket, "jobs"):
                if job.get("provider") == "app-actions" and job.get("kind") == "action-job":
                    receipts.append(invoke(socket, job))
            (output / "failed-app-opening.json").write_text(json.dumps({
                "native_room": state.get("native_room"),
                "job_receipts": receipts,
                "notifications": [{key: notice.get(key) for key in ["title", "severity"]}
                                  for notice in state.get("notifications", [])],
            }, indent=2) + "\n")
            raise
        capture_owned_chat_accessibility_transport(
            shell, output / "chat-accessibility-transport.json"
        )
        def published_chat():
            rows = [row for row in actions(socket)
                    if row.get("application") == "so.tilo.chat"]
            if not rows:
                return None
            if native and any(row.get("room") != enrollment["room"] for row in rows):
                raise RuntimeError("Chat published a foreign native room resource")
            if native and not any(row.get("operation") == "new-category"
                                  for row in rows):
                return None
            fields = ["provider", "id", "application", "room", "record", "resource", "operation", "semantic_identity"]
            return [{key: row.get(key) for key in fields} for row in rows]

        try:
            published = wait_for("Chat's ordinary semantic publication", published_chat, shell)
        except RuntimeError:
            fields = ["provider", "id", "application", "room", "record", "resource", "operation", "semantic_identity"]
            observed = [{key: row.get(key) for key in fields} for row in actions(socket)
                        if row.get("application") == "so.tilo.chat"]
            (output / "failed-semantic-publication.json").write_text(json.dumps(observed, indent=2) + "\n")
            require_ok(call(socket, {"op": "snapshot", "path": str(output / f"failed-bootstrap-{size}.png")}))
            raise
        (output / "semantic-ready.json").write_text(json.dumps(published, indent=2) + "\n")
        if native:
            category = [row for row in actions(socket)
                        if row.get("application") == "so.tilo.chat"
                        and row.get("room") == enrollment["room"]
                        and row.get("operation") == "new-category"]
            if len(category) != 1:
                raise RuntimeError("original native creator did not publish one actionable workspace")
            before_category_frame = painted_chat()
            if before_category_frame is None:
                raise RuntimeError("Chat's presented surface disappeared before Category edit")
            category_ack = invoke(socket, category[0], {"value": "Lifecycle proof"})
            (output / "category-ack.json").write_text(json.dumps(category_ack, indent=2) + "\n")
            try:
                category_receipt = wait_terminal(socket, category_ack, shell)
            except RuntimeError:
                invocation_id = category_ack.get("receipt", {}).get("broker_invocation_id")
                if isinstance(invocation_id, int) and not isinstance(invocation_id, bool):
                    observed = call(socket, {"op": "actions_status", "invocation_id": invocation_id})
                    (output / "failed-category-status.json").write_text(json.dumps(observed, indent=2) + "\n")
                require_ok(call(socket, {"op": "snapshot", "path": str(output / f"failed-category-{size}.png")}))
                raise
            (output / "category-terminal.json").write_text(json.dumps(category_receipt, indent=2) + "\n")
            def created_category():
                matches = [row for row in actions(socket)
                           if row.get("application") == "so.tilo.chat"
                           and row.get("room") == enrollment["room"]
                           and row.get("record") == category[0].get("record")
                           and row.get("operation") == "open-category"]
                return bool(matches)
            wait_for("normal native Category action's resulting public resource", created_category, shell)
            durable_refusal = ["inspector has not completed"]
            def durable_category():
                result = subprocess.run([
                    str(binaries / "examples/packaged_agent_lifecycle"),
                    "--check-native-document", env["TILO_STATE_PATH"],
                    enrollment["room"], category[0]["record"],
                ], env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    timeout=5)
                if result.returncode:
                    durable_refusal[0] = result.stderr.strip()[:512] or "inspector refused without detail"
                    (output / "category-durable-refusal.txt").write_text(result.stderr)
                    return None
                return json.loads(result.stdout)

            try:
                evidence = wait_for("actual durable Category and exact outgoing publication",
                                    durable_category, shell)
            except RuntimeError as error:
                raise RuntimeError(f"{error}: {durable_refusal[0]}") from error
            if (evidence.get("category_durable") is not True
                    or evidence.get("outgoing_publication_retained") is not True
                    or evidence.get("room") != enrollment["room"]
                    or evidence.get("app") != "so.tilo.chat"
                    or evidence.get("record") != category[0]["record"]):
                raise RuntimeError("actual canonical Category publication proof is absent")
            (output / "category-durable.json").write_text(json.dumps(evidence, indent=2) + "\n")
            def category_painted():
                presented = painted_chat()
                return presented if presented is not None and presented["frame_seq"] > before_category_frame["frame_seq"] else None
            frame = wait_for("Category edit's actual presented Chat frame", category_painted, shell)
        # The standard full-loop harness waits 500ms for chrome transitions;
        # a first Wayland buffer alone is not a settled desktop capture.
        time.sleep(0.5)
        frame = painted_chat()
        if frame is None:
            raise RuntimeError("Chat's presented surface disappeared before settled capture")
        invited = []
        if native:
            require_ok(call(socket, {"op": "snapshot", "path": str(output / f"before-agents-{size}.png")}))
            try:
                membership = admit_native_agents(socket, enrollment["room"], category[0]["record"], shell, output)
            except RuntimeError:
                fields = ["provider", "id", "application", "room", "record", "resource",
                          "operation", "semantic_identity"]
                observed = [{key: row.get(key) for key in fields} for row in actions(socket)
                            if row.get("room") == enrollment["room"]]
                (output / "failed-native-agent-catalog.json").write_text(
                    json.dumps(observed, indent=2) + "\n")
                require_ok(call(socket, {"op": "snapshot", "path": str(output / f"failed-native-agents-{size}.png")}))
                raise
            (output / "native-agent-membership.json").write_text(json.dumps(membership, indent=2) + "\n")
            invited = [entry["agent"] for entry in membership]
            open_agent = offered_agent_action(socket, enrollment["room"], category[0]["record"],
                                             "tide", "open-agent-conversation")
            if open_agent is None:
                raise RuntimeError("admitted Tide conversation disappeared before invocation")
            before_open = painted_chat()
            if before_open is None:
                raise RuntimeError("Chat's presented surface disappeared before agent conversation")
            open_ack = invoke(socket, open_agent)
            (output / "agent-conversation-open-source.json").write_text(
                json.dumps(public_action_observation(open_agent), indent=2) + "\n")
            (output / "agent-conversation-open-ack.json").write_text(json.dumps(open_ack, indent=2) + "\n")
            try:
                opened = wait_terminal(socket, open_ack, shell,
                                       public_failure_path=output / "failed-agent-conversation-open-terminal.json")
            except RuntimeError:
                observed = [public_action_observation(row) for row in actions(socket)
                            if row.get("application") == "so.tilo.chat"
                            and row.get("room") == enrollment["room"]
                            and row.get("record") == category[0]["record"]]
                (output / "failed-agent-conversation-open-catalog.json").write_text(
                    json.dumps(observed, indent=2) + "\n")
                require_ok(call(socket, {"op": "snapshot", "path": str(output / f"failed-agent-conversation-{size}.png")}))
                raise
            (output / "agent-conversation-open-terminal.json").write_text(json.dumps(opened, indent=2) + "\n")
            wait_for("agent conversation's actual presented Chat frame", lambda: (
                presented if (presented := painted_chat()) is not None
                and presented["frame_seq"] > before_open["frame_seq"] else None), shell)
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / f"agent-conversation-open-{size}.png")}))
            if lifecycle or task_only:
                lifecycle_evidence = managed_task_and_message_lifecycle(
                    socket, enrollment["room"], category[0]["record"], provider, output, shell,
                    profile, task_only=task_only,
                )
            if task_only:
                proven = bool(
                    lifecycle_evidence is not None
                    and lifecycle_evidence.get("semantic_start") is True
                    and lifecycle_evidence.get("provider_request_count", 0) >= 1
                    and lifecycle_evidence.get("terminal_status") == "done"
                )
                (output / "bootstrap.json").write_text(json.dumps({
                    "stage": "managed-task",
                    "native_join_acknowledged": True,
                    "chat_surface_id": frame["surface_id"],
                    "viewport": size,
                    "launch_path": launch_path,
                    "chat_frame_seq": frame["frame_seq"],
                    "agent_invite": invited,
                    "managed_task_proven": proven,
                    "full_lifecycle_proven": False,
                }, indent=2) + "\n")
                if proven:
                    print("PASS one managed Agent completed an owner-admitted Task")
                return proven
            access = wait_for(
                "Tide's actual accessible Chat conversation and editor",
                lambda: agent_conversation_accessibility(
                    "Tide", diagnostic_path=output / "agent-conversation-accessibility.json"
                ),
                shell,
            )
            (output / "agent-conversation-accessibility.json").write_text(
                json.dumps(access, indent=2) + "\n")
            if lifecycle:
                float_return_proven = native_float_return(
                    socket, enrollment["room"], category[0]["record"], output, size, shell
                )
                physical_send_evidence = physical_send_after_stop(
                    socket, enrollment["room"], category[0]["record"], provider, output,
                    size, shell, lifecycle_evidence["_owned_process_identity"],
                )
            time.sleep(0.5)
        else:
            invite = require_action(socket, "agent.invite", "agent")
            for name in ["Tide", "Reed"]:
                invoke(socket, invite, {"name": name, "preset": "Observer"})
                invited.append(name.lower())
        require_ok(call(socket, {"op": "snapshot", "path": str(output / f"bootstrap-{size}.png")}))
        lifecycle_proven = bool(
            lifecycle
            and float_return_proven
            and lifecycle_evidence is not None
            and lifecycle_evidence.get("semantic_start") is True
            and lifecycle_evidence.get("reply_actor") == "agent:tide"
            and lifecycle_evidence.get("applied_revision")
                == lifecycle_evidence.get("saved_revision")
            and lifecycle_evidence.get("terminal_status") == "cancelled"
            and physical_send_evidence is not None
            and physical_send_evidence.get("same_managed_process") is True
            and physical_send_evidence.get("reply_actor") == "agent:tide"
            and physical_send_evidence.get("provider_request_count") == 4
        )
        (output / "bootstrap.json").write_text(json.dumps({
            "stage": "native-bootstrap-only" if native else "legacy-bootstrap-only",
            "native_join_acknowledged": native, "chat_surface_id": frame["surface_id"],
            "viewport": size,
            "launch_path": launch_path,
            "chat_frame_seq": frame["frame_seq"], "agent_invite": invited,
            "lifecycle_proven": lifecycle_proven,
        }, indent=2) + "\n")
        print("PASS signed normal Chat launch" + (
            " and governed two-Agent roster/Chat conversation" if native
            else " and legacy room agent.invite actions"))
        if lifecycle_proven:
            print("PASS one managed Agent handled Queue, Message, Direction and Stop")
        else:
            print("NOT PROVEN: the managed Agent start capability has no semantic action door")
        return lifecycle_proven
    finally:
        finalize_fixture(profile, output, processes, logs, provider, runtime_owner)


def evidence_directory(stage, size):
    """Keep each run's receipts and renders separate, including failed runs."""
    parent = REPO / "target/packaged-agent-lifecycle" / f"{stage}-{size}"
    parent.mkdir(parents=True, exist_ok=True)
    return pathlib.Path(tempfile.mkdtemp(prefix="run-", dir=parent))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, required=True)
    parser.add_argument("--stage", choices=["bootstrap", "native-bootstrap", "managed-task", "lifecycle"], default="lifecycle")
    parser.add_argument("--size", choices=["1600x900", "800x600"], default="1600x900")
    parser.add_argument("--launch-path", choices=["applications", "store"], default="applications")
    args = parser.parse_args()
    binaries = args.binaries.resolve()
    evidence_stage = args.stage if args.launch_path == "applications" else f"{args.stage}-store"
    output = evidence_directory(evidence_stage, args.size)
    print(f"Evidence: {output}", flush=True)
    (output / "proof-source.json").write_text(json.dumps({
        "script_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
        "private_content_recorded": False,
    }, indent=2) + "\n")
    proven = bootstrap(
        binaries,
        output,
        native=args.stage != "bootstrap",
        size=args.size,
        lifecycle=args.stage == "lifecycle",
        launch_path=args.launch_path,
        task_only=args.stage == "managed-task",
    )
    if args.stage in {"managed-task", "lifecycle"} and not proven:
        raise RuntimeError(
            "the requested managed-Agent lifecycle proof did not complete"
        )


if __name__ == "__main__":
    main()
