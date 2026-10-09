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
import collections
import contextlib
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

# AT-SPI's stable wire value for AtspiRole::Heading. The external client
# intentionally exposes numeric roles because AccessKit has no role-name call.
AT_SPI_ROLE_HEADING = 83
# atspi_common::State::Enabled is ordinal 8 in the two-word state set.
AT_SPI_ENABLED_STATE_BIT = 1 << 8


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


def process_program(pid, proc_root=pathlib.Path("/proc")):
    """Return the complete executable basename, never a truncated comm."""
    try:
        argv0 = (proc_root / str(pid) / "cmdline").read_bytes().split(b"\0", 1)[0]
    except OSError:
        return ""
    return pathlib.Path(os.fsdecode(argv0)).name


def process_rows(proc_root=pathlib.Path("/proc")):
    """Read only ancestry, process group and kernel start time."""
    rows = {}
    try:
        entries = proc_root.iterdir()
    except OSError:
        return rows
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            rows[int(entry.name)] = {
                "parent": int(fields[1]),
                "group": int(fields[2]),
                "started": fields[19],
            }
        except (OSError, IndexError, ValueError):
            continue
    return rows


def process_descendants(rows, root):
    children = {}
    for process, row in rows.items():
        children.setdefault(row["parent"], []).append(process)
    owned = set()
    pending = list(children.get(root, ()))
    while pending:
        process = pending.pop()
        if process in owned:
            continue
        owned.add(process)
        pending.extend(children.get(process, ()))
    return owned


def process_executable(pid, proc_root=pathlib.Path("/proc")):
    try:
        return (proc_root / str(pid) / "exe").resolve(strict=True)
    except OSError:
        return None


def owned_fcitx_identity(shell_pid, proc_root=pathlib.Path("/proc")):
    """Require the compositor-owned composer and its exact isolated group."""
    rows = process_rows(proc_root)
    matches = []
    for pid in process_descendants(rows, shell_pid):
        row = rows[pid]
        if process_executable(pid, proc_root) != pathlib.Path("/usr/bin/fcitx5"):
            continue
        if row["group"] != pid:
            raise RuntimeError("the compositor-owned input method has no isolated process group")
        matches.append({"pid": pid, "group": row["group"], "started": row["started"]})
    if len(matches) != 1:
        return None
    return matches[0]


def same_fcitx_process(identity, proc_root=pathlib.Path("/proc")):
    if not isinstance(identity, dict):
        return False
    pid = identity.get("pid")
    if type(pid) is not int or pid <= 0:
        return False
    row = process_rows(proc_root).get(pid)
    return bool(
        row is not None
        and row["group"] == identity.get("group") == pid
        and row["started"] == identity.get("started")
        and process_executable(pid, proc_root) == pathlib.Path("/usr/bin/fcitx5")
    )


def cleanup_owned_fcitx(identity, proc_root=pathlib.Path("/proc")):
    """Reap only the exact composer group recorded under this Shell."""
    if not same_fcitx_process(identity, proc_root):
        return
    try:
        os.killpg(identity["group"], signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        if not same_fcitx_process(identity, proc_root):
            return
        time.sleep(0.05)
    if same_fcitx_process(identity, proc_root):
        try:
            os.killpg(identity["group"], signal.SIGKILL)
        except ProcessLookupError:
            pass


def foreign_frames(response):
    """Require the existing control operation's complete frame observation."""
    frames = response.get("answer", {}).get("frames")
    if frames is None:
        frames = response.get("foreign_frames", {}).get("frames")
    if not isinstance(frames, list) or any(not isinstance(row, dict) for row in frames):
        raise RuntimeError("foreign_frames observation is absent or malformed")
    return frames


def agent_conversation_accessibility(agent_name, *, detached=False, diagnostic_path=None,
                                     composer_name="Message composer", stale_only=False,
                                     include_send=False):
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
                headings = [
                    path
                    for path in client.named(
                        {path: name.casefold() for path, name in tree.items()},
                        f"{agent_name.casefold()}, agent",
                    )
                    if client.role(path) == AT_SPI_ROLE_HEADING
                ]
                composers = [path for path in client.named(tree, composer_name)
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
                    if include_send:
                        matches[key]["send_controls"] = client.named(tree, "Send message")
            if len(matches) == 1:
                record()
                return next(iter(matches.values()))
        except atspi.GateError as error:
            if stale_only and not stale_editor_transport(error):
                raise
            # A stale object during a real tree replacement is not success;
            # the bounded caller retries the complete observation.
            continue
    record()
    return None


def shell_accessibility_control(label):
    """Find one compositor-owned control without reading application content."""
    address = atspi.atspi_address()
    matches = []
    for bus, proxy_pid in atspi.bus_pids(address).items():
        pid = atspi.app_process(proxy_pid)
        if process_program(pid) != "shell":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                for path in client.named(tree, label):
                    matches.append({"bus": bus, "window": window, "control": path})
        except atspi.GateError:
            continue
    if len(matches) > 1:
        raise RuntimeError(f"the compositor published {len(matches)} copies of {label}")
    return matches[0] if matches else None


def agent_quick_composer_accessibility(agent, *, windows=None, observation=None):
    """Find the one exact compositor composer without reading its draft."""
    expected = {
        "message": "Message",
        "queue": "Queue",
        "steer": "Steer",
        "send": "Send",
        "close": "Close",
        "open_conversation": "Open conversation",
        "open_tasks": "Open Task Center",
    }
    observed = {
        "owned_root_count": 0, "readable_root_count": 0, "read_failure_count": 0,
        "direct_child_count": 0, "composer_root_count": 0, "editor_count": 0,
        "agent_count": 0, "kind_count": 0, "task_count": 0, "match_count": 0,
        "controls": dict.fromkeys(expected, 0),
    }
    if observation is not None:
        observation.update(observed)
    matches = []
    for owned in owned_shell_accessibility_windows() if windows is None else windows:
        observed["owned_root_count"] += 1
        bus = owned["bus"]
        client = atspi.AtspiClient(owned["address"], bus)
        try:
            for window in [owned["window"]]:
                children = client.call(owned["root"], "org.a11y.atspi.Accessible.GetChildren")
                observed["readable_root_count"] += 1
                paths = list(dict.fromkeys(atspi.PATH_RE.findall(children)))
                observed["direct_child_count"] += len(paths)
                if len(paths) > atspi.APP_TREE_NODE_LIMIT:
                    raise RuntimeError("the compositor composer children exceed the observation bound")
                tree = {path: client.property(path, "Name") for path in paths}
                for root in client.named(tree, "Agent quick composer"):
                    observed["composer_root_count"] += 1
                    subtree = accessible_subtree(client, root, atspi.APP_TREE_NODE_LIMIT)
                    controls = {}
                    complete = True
                    for key, label in expected.items():
                        nodes = client.named(subtree, label)
                        observed["controls"][key] += len(nodes)
                        if len(nodes) != 1:
                            complete = False
                        else:
                            controls[key] = nodes[0]
                    editors = [
                        path for path in client.named(subtree, "Message or instruction")
                        if "org.a11y.atspi.EditableText" in client.interfaces(path)
                    ]
                    agents = client.named(
                        {path: name.casefold() for path, name in subtree.items()},
                        agent.casefold(),
                    )
                    kinds = client.named(subtree, "Agent")
                    tasks = client.named(subtree, "Task")
                    observed["editor_count"] += len(editors)
                    observed["agent_count"] += len(agents)
                    observed["kind_count"] += len(kinds)
                    observed["task_count"] += len(tasks)
                    if len(tasks) > 1:
                        raise RuntimeError("the Agent quick composer published duplicate task pickers")
                    if complete and len(editors) == 1 and len(agents) == 1 and len(kinds) == 1:
                        matches.append({
                            **owned, "bus": bus, "window": window, "root": root,
                            "composer": editors[0], "controls": controls,
                            "task": tasks[0] if tasks else None,
                            "private_content_recorded": False,
                        })
        except atspi.GateError:
            observed["read_failure_count"] += 1
            continue
        finally:
            observed["match_count"] = len(matches)
            if observation is not None:
                observation.update(observed)
    if len(matches) > 1:
        raise RuntimeError("the compositor published duplicate exact Agent quick composers")
    return matches[0] if matches else None


def activate_agent_quick_composer_control(access, control):
    """Activate one named control inside the already-bound exact composer."""
    path = access["controls"].get(control)
    if path is None:
        raise RuntimeError(f"the Agent quick composer has no {control} control")
    activate_shell_accessibility_control(
        {"bus": access["bus"], "control": path}, control,
    )


def agent_quick_composer_status(agent, labels):
    """Return a known localized status from the exact composer subtree."""
    access = agent_quick_composer_accessibility(agent)
    if access is None:
        return None
    return quick_composer_status_at(access, labels)


def quick_composer_status_at(access, labels):
    """Read known statuses from this same bound composer, never another popup."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    subtree = accessible_subtree(client, access["root"], atspi.APP_TREE_NODE_LIMIT)
    present = [label for label in labels if len(client.named(subtree, label)) == 1]
    if len(present) > 1:
        raise RuntimeError("the Agent quick composer published conflicting task receipts")
    return present[0] if present else None


def owned_shell_accessibility_windows():
    """Prebind only actual windows on this proof's compositor AT connection."""
    address = atspi.atspi_address()
    windows = []
    for bus, proxy_pid in atspi.bus_pids(address).items():
        pid = atspi.app_process(proxy_pid)
        if process_program(pid) != "shell":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                children = client.call(window, "org.a11y.atspi.Accessible.GetChildren")
                paths = list(dict.fromkeys(atspi.PATH_RE.findall(children)))
                if len(paths) > atspi.APP_TREE_NODE_LIMIT:
                    raise RuntimeError("the compositor window children exceed the observation bound")
                names = {path: client.property(path, "Name") for path in paths}
                roots = client.named(names, "Tilo workspace")
                if len(roots) > 1:
                    raise RuntimeError("the compositor window has ambiguous workspace accessibility")
                if roots:
                    windows.append({"address": address, "bus": bus,
                                    "window": window, "root": roots[0]})
        except atspi.GateError:
            continue
    return windows


def agent_marker_accessibility(agent, *, windows=None, observation=None):
    """Read live marker children directly, decoding their actual wire Names."""
    pattern = re.compile(rf"^Agent action: {re.escape(agent)}(?: · Agent)?$", re.IGNORECASE)
    matches = []
    windows = owned_shell_accessibility_windows() if windows is None else windows
    counts = {"owned_root_count": len(windows), "readable_root_count": 0,
              "root_read_failure_count": 0, "direct_child_count": 0,
              "child_name_read_failure_count": 0, "exact_marker_candidate_count": 0}
    for owned in windows:
        client = atspi.AtspiClient(owned["address"], owned["bus"])
        root_read = False
        try:
            children = client.call(owned["root"], "org.a11y.atspi.Accessible.GetChildren")
            root_read = True
            counts["readable_root_count"] += 1
            paths = list(dict.fromkeys(atspi.PATH_RE.findall(children)))
            counts["direct_child_count"] += len(paths)
            if len(paths) > atspi.APP_TREE_NODE_LIMIT:
                raise RuntimeError("the compositor marker children exceed the observation bound")
            for path in paths:
                name = atspi.property_text(client.property(path, "Name"))
                if name is not None and pattern.fullmatch(name):
                    matches.append({**owned, "control": path, "label": name})
        except atspi.GateError:
            counts["child_name_read_failure_count" if root_read
                   else "root_read_failure_count"] += 1
            continue
    counts["exact_marker_candidate_count"] = len(matches)
    if observation is not None:
        observation.update(counts)
    if len(matches) > 1:
        raise RuntimeError("the compositor published duplicate exact Agent markers")
    return matches[0] if matches else None


def chat_category_accessibility(label):
    """Find one exact current Chat category toggle without reading messages."""
    names = {f"Collapse {label}", f"Expand {label}"}
    address = atspi.atspi_address()
    matches = []
    for bus, proxy_pid in atspi.bus_pids(address).items():
        if process_program(atspi.app_process(proxy_pid)) != "chat":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                for name in names:
                    for path in client.named(tree, name):
                        matches.append({
                            "bus": bus, "window": window, "control": path, "label": name,
                        })
        except atspi.GateError:
            continue
    if len(matches) > 1:
        raise RuntimeError("Chat published duplicate exact category toggles")
    return matches[0] if matches else None


def chat_accessibility_control(label):
    """Find one exact control in the mounted Chat process."""
    address = atspi.atspi_address()
    matches = []
    for bus, proxy_pid in atspi.bus_pids(address).items():
        if process_program(atspi.app_process(proxy_pid)) != "chat":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                for path in client.named(tree, label):
                    matches.append({
                        "bus": bus, "window": window, "control": path, "label": label,
                    })
        except atspi.GateError:
            continue
    if len(matches) > 1:
        raise RuntimeError(f"Chat published duplicate {label} controls")
    return matches[0] if matches else None


def ensure_chat_category_visible(socket, label, process):
    """Reveal compact navigation through Chat's real shared control when needed."""
    category = chat_category_accessibility(label)
    if category is not None:
        return category
    reveal = wait_for(
        "Chat's shared conversation navigation control",
        lambda: chat_accessibility_control("Show conversation navigation"),
        process,
    )
    activate_accessibility_control(reveal, reveal["label"])
    return wait_for(
        "Chat's exact visible marker category",
        lambda: chat_category_accessibility(label),
        process,
    )


def marker_binds_presented_category(marker, category, geometry):
    """Require the real marker bounds to intersect its named presented control."""
    category_target = component_global_target(
        category, category["control"], geometry, "Chat category header"
    )
    client_rect = category_target["client_rect"]
    origin = geometry["origin"]
    category_rect = {
        "x": origin["x"] + client_rect["x"],
        "y": origin["y"] + client_rect["y"],
        "w": client_rect["w"],
        "h": client_rect["h"],
    }
    marker_client = atspi.AtspiClient(marker.get("address") or atspi.atspi_address(), marker["bus"])
    marker_rect = atspi.extents_reply(marker_client.call(
        marker["control"], "org.a11y.atspi.Component.GetExtents", "1",
    ))
    if marker_rect is None or marker_rect[2] <= 2 or marker_rect[3] <= 2:
        raise RuntimeError("the actual Agent marker has no on-screen extent")
    marker_rect = dict(zip(("x", "y", "w", "h"), marker_rect, strict=True))
    overlap_x = min(marker_rect["x"] + marker_rect["w"],
                    category_rect["x"] + category_rect["w"]) \
        - max(marker_rect["x"], category_rect["x"])
    overlap_y = min(marker_rect["y"] + marker_rect["h"],
                    category_rect["y"] + category_rect["h"]) \
        - max(marker_rect["y"], category_rect["y"])
    return {
        "marker_rect": marker_rect,
        "category_rect": category_rect,
        "category_bound": overlap_x > 0 and overlap_y > 0,
        "private_content_recorded": False,
    }


def activate_accessibility_control(access, label):
    """Focus and activate one exact mounted node through its public AT door."""
    client = atspi.AtspiClient(access.get("address") or atspi.atspi_address(), access["bus"])
    focused = client.call(
        access["control"], "org.a11y.atspi.Component.GrabFocus", check=False
    )
    if focused.strip() != "(true,)":
        raise RuntimeError(f"the mounted {label} refused accessibility focus")
    actions = client.call(
        access["control"], "org.a11y.atspi.Action.GetActions", check=False
    )
    if "'click'" not in actions:
        raise RuntimeError(f"the mounted {label} has no accessibility action")
    activated = client.call(
        access["control"], "org.a11y.atspi.Action.DoAction", "0", check=False
    )
    if activated.strip() != "(true,)":
        raise RuntimeError(f"the mounted {label} refused accessibility activation")
    return True


def activate_shell_accessibility_control(access, label):
    """Activate one exact compositor-owned control through the shared AT door."""
    return activate_accessibility_control(access, label)


def agent_read_only_accessibility(agent_name, *, detached=False):
    """Observe one mounted revoked conversation without reading its private draft."""
    address = atspi.atspi_address()
    matches = {}
    control_name = "Return conversation" if detached else "Float conversation"
    forbidden = {"Send message", "Queue task", "Send direction", "Stop current task"}
    for bus, proxy_pid in atspi.bus_pids(address).items():
        pid = atspi.app_process(proxy_pid)
        if process_program(pid) != "chat":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                headings = client.named(
                    {path: name.casefold() for path, name in tree.items()},
                    f"{agent_name.casefold()}, agent",
                )
                notices = client.named(
                    tree,
                    "This agent is no longer available here. Your draft is kept on this device.",
                )
                controls = client.named(tree, control_name)
                mutable = [path for path, name in decoded_accessible_names(tree).items()
                           if name in forbidden]
                editable = [path for path in tree
                            if "org.a11y.atspi.EditableText" in client.interfaces(path)]
                if (len(headings) == 1 and len(notices) == 1 and len(controls) == 1
                        and not mutable and not editable):
                    key = (headings[0], notices[0], controls[0])
                    matches.setdefault(key, {
                        "bus": bus, "process": pid, "window": window,
                        "heading": headings[0], "notice": notices[0],
                        "control": controls[0], "agent": agent_name,
                        "detached": detached, "mutable_controls": 0,
                        "editable_composers": 0, "private_content_recorded": False,
                    })
        except atspi.GateError:
            continue
    return next(iter(matches.values())) if len(matches) == 1 else None


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


def stale_editor_transport(error):
    text = str(error)
    return "org.freedesktop.DBus.Error.UnknownObject" in text or (
        "org.freedesktop.DBus.Error.UnknownInterface" in text
        and "org.a11y.atspi.Text" in text)


def chat_editor_contract(access):
    client = atspi.AtspiClient(access.get("address") or atspi.atspi_address(), access["bus"])
    name = atspi.property_text(client.property(access["composer"], "Name"))
    interfaces = client.interfaces(access["composer"])
    return name == "Message composer" and all(interface in interfaces for interface in (
        "org.a11y.atspi.Text", "org.a11y.atspi.EditableText"))


def owned_editor_read(access, read, deadline=None):
    """Refresh retired transport or semantic targets, never a readable draft mismatch."""
    deadline = time.monotonic() + 30 if deadline is None else deadline
    while True:
        try:
            # Other callers use the shell's separately observed quick composer.
            # Only conversation descriptors carry this exact owned Chat route.
            chat_route = all(key in access for key in ("process", "agent", "detached"))
            if not chat_route or chat_editor_contract(access):
                return read()
            # A reused path naming another Text node is a retired semantic
            # target. Do not consume that unrelated node's private payload.
        except atspi.GateError as error:
            if not stale_editor_transport(error):
                raise
        if type(access.get("process")) is not int or not access.get("agent") \
                or type(access.get("detached")) is not bool:
            raise RuntimeError("stale private editor has no exact owned Chat route") from None
        while time.monotonic() < deadline:
            if process_program(access["process"]) != "chat":
                raise RuntimeError("stale private editor lost its original Chat process")
            fresh = agent_conversation_accessibility(
                access["agent"], detached=access["detached"], stale_only=True)
            if fresh is not None:
                if any(fresh.get(key) != access[key] for key in ("process", "agent", "detached")):
                    raise RuntimeError("refreshed private editor belongs to a foreign Chat route")
                try:
                    if not chat_editor_contract(fresh):
                        # The full walk observed this exact owned editor, but
                        # its object can retire before the separate contract
                        # query. Discard it without changing the cached access.
                        time.sleep(0.1)
                        continue
                except atspi.GateError as error:
                    if not stale_editor_transport(error):
                        raise
                else:
                    access.clear()
                    access.update(fresh)
                    break
            time.sleep(0.1)
        else:
            raise ProofTimeout("timed out refreshing the exact owned private editor")


def editor_accessibility_focus(access):
    return owned_editor_read(access, lambda: _editor_accessibility_focus(access))


def _editor_accessibility_focus(access):
    """Observe the editor's AccessKit focus; this does not focus the Wayland seat."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    reply = client.call(access["composer"], "org.a11y.atspi.Accessible.GetState")
    # AT-SPI's state set is two uint32 words. The shared external client uses
    # this same Focused bit, which AccessKit sets only for the active window.
    words = re.fullmatch(r"\(\[(?:uint32 )?(\d+),\s*(?:uint32 )?(\d+)\],\)", reply.strip())
    if words is None or any(int(word) > 0xFFFFFFFF for word in words.groups()):
        raise RuntimeError("the actual editor keyboard focus is unobservable")
    return bool(int(words.group(1)) & atspi.FOCUSED_STATE_BIT)


def accessibility_control_enabled(access, control):
    """Observe one shared control's actual AT enabled state."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    reply = client.call(control, "org.a11y.atspi.Accessible.GetState")
    words = re.fullmatch(r"\(\[(?:uint32 )?(\d+),\s*(?:uint32 )?(\d+)\],\)", reply.strip())
    if words is None or any(int(word) > 0xFFFFFFFF for word in words.groups()):
        raise RuntimeError("the actual control enabled state is unobservable")
    return bool(int(words.group(1)) & AT_SPI_ENABLED_STATE_BIT)


def task_phase_accessibility(access, label):
    """Observe one exact public status label in the mounted Chat window."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    try:
        tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
    except atspi.GateError as error:
        if not re.search(
                r" failed: Error: GDBus\.Error:org\.freedesktop\.DBus\.Error\.UnknownObject:",
                str(error)):
            raise
        # A replaced node invalidates this entire snapshot. The enclosing
        # wait retries a fresh walk under its original deadline; absence is
        # never evidence that the requested task phase has been observed.
        return False
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


def floating_numeric_geometry(frame):
    """Export only the compositor's closed numeric geometry, never identity or text."""
    def fields(value, names):
        if not isinstance(value, dict):
            raise RuntimeError("detached numeric geometry is malformed")
        result = {name: value.get(name) for name in names}
        if any(type(number) not in (int, float) or not math.isfinite(number)
               for number in result.values()):
            raise RuntimeError("detached numeric geometry is non-finite or unavailable")
        if any(result[name] < 0 for name in ("w", "h") if name in result):
            raise RuntimeError("detached numeric geometry has negative dimensions")
        return result

    def extent(value):
        return fields(value, ("w", "h"))

    def rect(value):
        return fields(value, ("x", "y", "w", "h"))

    if not isinstance(frame, dict) or type(frame.get("frame_seq")) is not int \
            or frame["frame_seq"] <= 0:
        raise RuntimeError("detached numeric geometry has no frame sequence")
    result = {name: rect(frame.get(name))
              for name in ("rect", "content_rect", "target_rect")}
    result.update(fields(frame, ("device_scale",)))
    if result["device_scale"] <= 0:
        raise RuntimeError("detached numeric geometry has no device scale")
    result["frame_seq"] = frame["frame_seq"]
    if "frame_geometry" not in frame:
        raise RuntimeError("detached numeric frame geometry is unavailable")
    geometry = frame["frame_geometry"]
    result["frame_geometry"] = None
    if geometry is not None:
        if not isinstance(geometry, dict) or any(
                name not in geometry for name in ("configured_extent", "window_geometry")):
            raise RuntimeError("detached numeric frame geometry is malformed")
        result["frame_geometry"] = {
            "raw_buffer": extent(geometry.get("raw_buffer")),
            "content_crop": rect(geometry.get("content_crop")),
            "min_size": extent(geometry.get("min_size")),
            "max_size": extent(geometry.get("max_size")),
            "configured_extent": None if geometry["configured_extent"] is None
            else extent(geometry["configured_extent"]),
            "window_geometry": None,
        }
        window = geometry["window_geometry"]
        if window is not None:
            if not isinstance(window, dict) or "viewport_destination" not in window:
                raise RuntimeError("detached numeric window geometry is malformed")
            observed_window = rect(window)
            observed_window.update(fields(window, ("buffer_scale",)))
            if observed_window["buffer_scale"] <= 0:
                raise RuntimeError("detached numeric window geometry has no buffer scale")
            observed_window["viewport_destination"] = None \
                if window["viewport_destination"] is None \
                else extent(window["viewport_destination"])
            result["frame_geometry"]["window_geometry"] = observed_window
    return result


def stable_floating_commit_geometry(before, after):
    """A fixed-size authenticated child must not rebound to stale transfer hints."""
    before = floating_numeric_geometry(before)
    after = floating_numeric_geometry(after)
    if any(before[name] != after[name]
           for name in ("rect", "content_rect", "target_rect", "device_scale")):
        return False
    for observed in (before, after):
        geometry = observed["frame_geometry"]
        if geometry is None or geometry["configured_extent"] is None:
            return False
        target = observed["target_rect"]
        for axis in ("w", "h"):
            logical = target[axis]
            if logical <= 0 or abs(observed["rect"][axis] - logical) > 1 \
                    or abs(observed["content_rect"][axis] - logical) > 1:
                return False
            # Physical buffers may round fractional logical sizes once to pixels.
            physical = logical * observed["device_scale"]
            if any(abs(geometry[name][axis] - physical) > 1
                   for name in ("raw_buffer", "content_crop")):
                return False
            if any(abs(geometry[name][axis] - logical) > 1
                   for name in ("configured_extent", "min_size", "max_size")):
                return False
    return True


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


def component_global_target(access, path, geometry, label):
    """Translate one real client-local AccessKit extent through the compositor route."""
    client = atspi.AtspiClient(access.get("address") or atspi.atspi_address(), access["bus"])
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
    return {
        "client_rect": {"x": x, "y": y, "w": width, "h": height},
        "global_center": {"x": global_x, "y": global_y},
    }


def component_global_center(access, path, geometry, label):
    target = component_global_target(access, path, geometry, label)
    center = target["global_center"]
    return center["x"], center["y"]


@contextlib.contextmanager
def hold_physical_modifiers(socket, *, ctrl=False, shift=False, alt=False, logo=False):
    """Hold real seat modifier keys and release accepted keys in reverse order."""
    requested = [
        key for key, enabled in (
            ("Control", ctrl), ("Shift", shift), ("Alt", alt), ("Super", logo)
        ) if enabled
    ]
    held = []
    body_error = None
    try:
        for key in requested:
            require_ok(call(socket, {"op": "key_down", "key": key}))
            held.append(key)
        yield
    except BaseException as error:
        body_error = error
        raise
    finally:
        release_error = None
        while held:
            try:
                require_ok(call(socket, {"op": "key_up", "key": held.pop()}))
            except BaseException as error:
                if release_error is None:
                    release_error = error
        if body_error is None and release_error is not None:
            raise release_error


def physical_chord(socket, key, **modifiers):
    with hold_physical_modifiers(socket, **modifiers):
        require_ok(call(socket, {"op": "key", "key": key}))


def require_chat_keyboard_focus_after_click(socket, output, *, detached=False):
    """Refuse key injection unless the exact painted Chat surface owns the seat."""
    frames = call(socket, {"op": "foreign_frames"})
    frame = (presented_floating_frame(frames, "so.tilo.chat") if detached
             else presented_primary_frame(frames, "so.tilo.chat"))
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
            "phase": "after_physical_detached_chat_click" if detached
            else "after_physical_chat_click",
            "presented_chat": frame is not None,
            "keyboard_focused": False,
            "reason": reason,
            "private_content_recorded": False,
        }, indent=2) + "\n")
        raise RuntimeError("the physical click did not give the presented Chat surface keyboard focus")
    return True


def private_editor_observation(access, expected, *, deadline=None):
    return owned_editor_read(access, lambda: _private_editor_observation(access, expected), deadline)


def closed_editor_continuity(before, after):
    """Export field equality only, never private content or caret/selection offsets."""
    return {
        "content_matches": before.get("draft_matches") is True and after.get("draft_matches") is True,
        "caret_equal": before.get("caret") == after.get("caret"),
        "selection_equal": before.get("selection") == after.get("selection"),
        "accessibility_focus_equal": before.get("accessibility_focused") == after.get("accessibility_focused"),
        "accessibility_focused_before": before.get("accessibility_focused") is True,
        "accessibility_focused_after": after.get("accessibility_focused") is True,
    }


def _private_editor_observation(access, expected):
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
            "accessibility_focused": _editor_accessibility_focus(access)}


def private_editor_value_matches(access, expected):
    """Observe only whether this fixture's in-memory value reached the editor."""
    def read():
        client = atspi.AtspiClient(access.get("address") or atspi.atspi_address(), access["bus"])
        reply = client.call(access["composer"], "org.a11y.atspi.Text.GetText", "--", "0", "-1")
        return atspi.text_reply(reply) == expected
    return owned_editor_read(access, read)


def closed_return_editor_observation(access, before, after, same_chat):
    """Classify a private editor in memory; export neither text nor transport errors."""
    observed = dict.fromkeys(("editor_observed", "same_chat_process", "composer_name_matches",
                              "transport_success", "unknown_object", "unparseable",
                              "baseline_matches", "chinese_matches", "direct_latin_matches",
                              "empty", "other"), False)
    observed["character_count"] = None
    if access is None:
        return observed
    observed["editor_observed"] = True
    observed["same_chat_process"] = same_chat is True
    client = atspi.AtspiClient(access.get("address") or atspi.atspi_address(), access["bus"])
    try:
        observed["composer_name_matches"] = atspi.property_text(client.property(
            access["composer"], "Name")) == "Message composer"
        reply = client.call(access["composer"], "org.a11y.atspi.Text.GetText", "--", "0", "-1")
        observed["transport_success"] = True
        value = atspi.text_reply(reply)
        observed["unparseable"] = value is None
        if value is not None:
            observed.update({
                "baseline_matches": value == before,
                "chinese_matches": value == after,
                "direct_latin_matches": value == before + "nihao",
                "empty": value == "",
                "other": value not in (before, after, before + "nihao", ""),
                "character_count": len(value),
            })
    except atspi.GateError as error:
        observed["unknown_object"] = "org.freedesktop.DBus.Error.UnknownObject" in str(error)
    return observed


def wait_for_private_selection(socket, access, expected, selection, process, output, *,
                               expected_caret=None, require_focused=False):
    """Observe dispatched physical keys; never replay them or change the assertion."""
    last = None
    deadline = time.monotonic() + 30

    def selected():
        nonlocal last
        last = private_editor_observation(access, expected, deadline=deadline)
        return last if last["selection"] == selection \
            and (expected_caret is None or last["caret"] == expected_caret) \
            and (not require_focused or last["accessibility_focused"] is True) else None

    try:
        return wait_for("physical selection in the actual private editor", selected, process)
    except RuntimeError:
        # Only bounded offsets and boolean observations leave this scope.
        # The fixture's draft and raw accessibility replies remain in memory.
        (output / "physical-selection-refused.json").write_text(json.dumps({
            "expected_selection": selection,
            "expected_caret": expected_caret,
            "observed_selection": last["selection"] if last else None,
            "observed_caret": last["caret"] if last else None,
            "draft_matches": last["draft_matches"] if last else False,
            "accessibility_focused": last["accessibility_focused"] if last else False,
            "private_content_recorded": False,
        }, indent=2) + "\n")
        try:
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / "physical-selection-refused.png")}))
        except RuntimeError:
            print("NOTE selection failure snapshot unavailable", flush=True)
        raise


def decoded_accessible_names(tree):
    """Decode actual AT-SPI property replies before exact name comparisons."""
    decoded = {}
    for path, reply in tree.items():
        name = atspi.property_text(reply)
        if name is None:
            raise RuntimeError("the accessibility node name is unobservable")
        decoded[path] = name
    return decoded


def accessible_names(access):
    """Read only the bounded names in one already-matched Chat window."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    return set(decoded_accessible_names(accessible_subtree(
        client, access["window"], atspi.APP_TREE_NODE_LIMIT
    )).values())


def activate_named_agent_control(access, label):
    """Activate one exact public control in the matched Chat presentation."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
    matches = client.named(tree, label)
    if len(matches) != 1:
        raise RuntimeError(f"the actual Agent conversation has no unique {label} control")
    focused = client.call(
        matches[0], "org.a11y.atspi.Component.GrabFocus", check=False
    )
    if focused.strip() != "(true,)":
        raise RuntimeError(f"the actual {label} control refused accessibility focus")
    activated = client.call(
        matches[0], "org.a11y.atspi.Action.DoAction", "0", check=False
    )
    if activated.strip() != "(true,)":
        raise RuntimeError(f"the actual {label} control refused accessibility activation")
    return True


def agent_write_restricted_accessibility(agent_name, expected):
    """Observe the mounted read-only Agent view without exporting its draft."""
    address = atspi.atspi_address()
    matches = {}
    forbidden = {"Send message", "Queue task", "Send direction", "Stop current task"}
    for bus, proxy_pid in atspi.bus_pids(address).items():
        pid = atspi.app_process(proxy_pid)
        if process_program(pid) != "chat":
            continue
        client = atspi.AtspiClient(address, bus)
        try:
            children = client.call(atspi.ROOT_PATH, "org.a11y.atspi.Accessible.GetChildren")
            for window in dict.fromkeys(atspi.PATH_RE.findall(children)):
                tree = accessible_subtree(client, window, atspi.APP_TREE_NODE_LIMIT)
                headings = client.named(
                    {path: name.casefold() for path, name in tree.items()},
                    f"{agent_name.casefold()}, agent",
                )
                notices = client.named(
                    tree, "You cannot send messages in this conversation."
                )
                retained = client.named(tree, expected)
                mutable = [path for path, name in decoded_accessible_names(tree).items()
                           if name in forbidden]
                editable = [path for path in tree
                            if "org.a11y.atspi.EditableText" in client.interfaces(path)]
                if (len(headings) == 1 and len(notices) == 1 and len(retained) == 1
                        and not mutable and not editable):
                    key = (headings[0], notices[0], retained[0])
                    matches.setdefault(key, {
                        "bus": bus, "process": pid, "window": window,
                        "notice_present": True, "draft_matches": True,
                        "mutable_controls": len(mutable),
                        "editable_composers": len(editable),
                        "private_content_recorded": False,
                    })
        except atspi.GateError:
            continue
    if len(matches) > 1:
        raise RuntimeError("duplicate write-restricted Agent conversations were published")
    return next(iter(matches.values())) if matches else None


def activate_agent_conversation_control(access, label, *, expected_name=None):
    """Activate the exact control paired with the observed private editor."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    actions = client.call(
        access["control"], "org.a11y.atspi.Action.GetActions", check=False
    )
    if "'click'" not in actions:
        raise RuntimeError(f"the actual {label} control has no accessibility action")
    if expected_name is not None and atspi.property_text(
            client.property(access["control"], "Name")) != expected_name:
        raise RuntimeError("the current conversation control semantic name differs")
    result = client.call(
        access["control"], "org.a11y.atspi.Action.DoAction", "0", check=False
    )
    if result.strip() != "(true,)":
        raise RuntimeError(f"the actual {label} control refused activation")
    return {"accessibility_action": label, "accepted": True}


def focus_agent_conversation_control(access, path, label):
    """Focus one published control; a later physical key remains the activator."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    focused = client.call(
        path, "org.a11y.atspi.Component.GrabFocus", check=False
    )
    if focused.strip() != "(true,)":
        raise RuntimeError(f"the actual {label} control refused keyboard focus")
    actions = client.call(
        path, "org.a11y.atspi.Action.GetActions", check=False
    )
    if "'click'" not in actions:
        raise RuntimeError(f"the actual {label} control has no activation action")
    return {"keyboard_focus": label, "accepted": True}


def current_owned_conversation_accessibility(owner):
    """Resolve current controls without replacing the captured Chat owner/route."""
    current = agent_conversation_accessibility(owner["agent"], detached=owner["detached"])
    if current is not None and any(current.get(field) != owner.get(field)
            for field in ("process", "bus", "agent", "detached")):
        raise RuntimeError("keyboard conversation control changed the owned Chat route")
    return current


def wait_for_conversation_control_focus(owner, output, process, *, detached=True, control="control"):
    """Observe, without repairing focus, the current exact owned handoff control."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = {"accessibility_observable": False, "same_owned_route": False,
                  "known_stale_object": False, "control_focused": False}
        evidence["observation_count"] += 1
        try:
            if owner.get("detached") is not detached:
                raise RuntimeError("keyboard conversation control has a malformed target route")
            current = current_owned_conversation_accessibility(owner)
            if current is None:
                return None
            sample["accessibility_observable"] = True
            sample["same_owned_route"] = all(current.get(field) == owner.get(field)
                for field in ("process", "bus", "agent", "detached")) \
                and current.get("detached") is detached
            if not sample["same_owned_route"]:
                raise RuntimeError("keyboard Return control changed the owned Chat route")
            if not isinstance(current.get(control), str) or not current[control].startswith("/"):
                raise RuntimeError("keyboard conversation control target is unobservable")
            sample["control_focused"] = _editor_accessibility_focus({
                "bus": current["bus"], "composer": current[control]})
            return current if sample["control_focused"] else None
        except atspi.GateError as error:
            if not re.search(
                    r" failed: Error: GDBus\.Error:org\.freedesktop\.DBus\.Error\.UnknownObject:",
                    str(error)):
                raise
            sample["known_stale_object"] = True
            return None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            receipt = "keyboard-return-control-focus.json" if detached else "keyboard-float-control-focus.json"
            (output / receipt).write_text(
                json.dumps(evidence, indent=2) + "\n")

    return wait_for("keyboard Return's current actual control focus", observe, process)


def wait_for_floating_return_control_focus(owner, output, process):
    return wait_for_conversation_control_focus(owner, output, process)


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
    if rows:
        native_surface_id(rows[0])
    return rows[0] if rows else None


def native_surface_id(frame):
    """Read the compositor-owned native identity, never the logical Float id."""
    surface_id = frame.get("surface_id") if isinstance(frame, dict) else None
    if type(surface_id) is not int or surface_id <= 0:
        raise RuntimeError("detached application window has no valid native surface identity")
    return surface_id


def fresh_floating_presentation(current, previous):
    """Require this logical Float on a new or freshly painted native surface."""
    if current is None or previous is None:
        return None
    if current.get("floating_id") != previous.get("floating_id"):
        return None
    if native_surface_id(current) != native_surface_id(previous):
        return current
    return current if current["frame_seq"] > previous["frame_seq"] else None


def current_same_floating_presentation(response, previous):
    """Refresh seat state without accepting another logical/native window."""
    current = presented_floating_frame(response, previous["app_id"])
    if current is None or current.get("floating_id") != previous.get("floating_id") \
            or native_surface_id(current) != native_surface_id(previous) \
            or current["frame_seq"] < previous["frame_seq"]:
        return None
    return current


def wait_for_keyboard_float_presentation(socket, output, previous, primary, process):
    """Diagnose raw native frames without changing the existing acceptance filter."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        response = call(socket, {"op": "foreign_frames"})
        rows = foreign_frames(response)
        children = [row for row in rows if row.get("app_id") == "so.tilo.chat"
                    and row.get("floating_id")]
        exact = [row for row in children if row.get("floating_id") == previous["floating_id"]]
        current = exact[0] if len(exact) == 1 else children[0] if len(children) == 1 else None
        primaries = [row for row in rows if row.get("app_id") == primary["app_id"]
                     and row.get("floating_id") is None
                     and row.get("surface_id") == native_surface_id(primary)]
        sample = {"raw_chat_child_count": min(len(children), 64),
                  "exact_logical_child_count": min(len(exact), 64),
                  "raw_child_observable": current is not None,
                  "same_logical_child": current is not None
                      and current.get("floating_id") == previous["floating_id"],
                  "same_native_child": current is not None
                      and current.get("surface_id") == native_surface_id(previous),
                  "child_native_identity_observable": current is not None
                      and type(current.get("surface_id")) is int and current["surface_id"] > 0,
                  "child_frame_observable": current is not None
                      and type(current.get("frame_seq")) is int and current["frame_seq"] > 0,
                  "exact_primary_count": min(len(primaries), 64),
                  "primary_keyboard_focused": primaries[0].get("keyboard_focused")
                      if len(primaries) == 1 and type(primaries[0].get("keyboard_focused")) is bool else None}
        for field in ("presented", "pixels_ready", "capture_pending", "suspended", "transferring"):
            sample["child_" + field] = current.get(field) \
                if current is not None and type(current.get(field)) is bool else None
        sample["native_observation_valid"] = sample["child_native_identity_observable"] \
            and sample["child_frame_observable"] \
            and all(type(sample["child_" + field]) is bool
                    for field in ("presented", "pixels_ready", "capture_pending", "suspended", "transferring"))
        comparable = sample["same_native_child"] and sample["child_frame_observable"]
        sample["child_frame_fresh"] = current["frame_seq"] > previous["frame_seq"] if comparable else None
        sample["child_frame_nonregressing"] = current["frame_seq"] >= previous["frame_seq"] if comparable else None
        sample["child_error_present"] = current.get("last_error") is not None if current is not None else None
        sample["ordinary_child_route"] = current.get("input_route") is None \
            and current.get("input_source_surface_id") is None if current is not None else None
        evidence["observation_count"] += 1
        if evidence["initial"] is None:
            evidence["initial"] = sample.copy()
        evidence["last"] = sample
        (output / "keyboard-float-native-observations.json").write_text(
            json.dumps(evidence, indent=2) + "\n")
        return fresh_floating_presentation(presented_floating_frame(response, "so.tilo.chat"), previous)

    return wait_for("keyboard-activated Tide Float", observe, process)


def current_detached_control_target(socket, previous, agent_name, draft, expected_editor):
    """Bind one fresh AT control extent to its exact current native child."""
    current = current_same_floating_presentation(
        call(socket, {"op": "foreign_frames"}), previous
    )
    if current is None:
        return None
    access = agent_conversation_accessibility(agent_name, detached=True)
    if access is None:
        return None
    if private_editor_observation(access, draft) != expected_editor:
        raise RuntimeError("refreshing Return changed private editor state")
    first_geometry = presented_content_geometry(current)
    confirmed = current_same_floating_presentation(
        call(socket, {"op": "foreign_frames"}), current
    )
    if confirmed is None:
        return None
    confirmed_geometry = presented_content_geometry(confirmed)
    if confirmed_geometry != first_geometry:
        return None
    target = component_global_target(
        access,
        access["control"],
        confirmed_geometry,
        "Return conversation control",
    )
    return confirmed, access, target


def current_ime_return_target(socket, child, owner, draft, editor, input_method):
    """Observe one fresh Return target without settling or repairing real preedit."""
    target = current_detached_control_target(socket, child, owner["agent"], draft, editor)
    if target is None:
        return None
    frame, access, _ = target
    if access.get("process") != owner["process"] or access.get("detached") is not True:
        raise RuntimeError("Rime Return replaced its exact private editor owner")
    if frame.get("keyboard_focused") is not True \
            or frame.get("suspended") is not False or frame.get("transferring") is not False \
            or frame.get("last_error") is not None \
            or frame.get("input_route") is not None \
            or frame.get("input_source_surface_id") is not None:
        raise RuntimeError("Rime Return lost its exact ordinary child seat")
    if not chat_text_input_active(socket) or not chat_input_method_preedit_active(socket) \
            or not same_fcitx_process(input_method):
        raise RuntimeError("refreshing Rime Return committed or retired the real composition")
    return target


def ime_return_withdrawal_observation(response, child, primary):
    """Closed native custody evidence; identities are compared only in memory."""
    rows = foreign_frames(response)
    children = [row for row in rows if row.get("app_id") == child["app_id"]
                and row.get("floating_id") == child["floating_id"]]
    if len(children) > 1 or children and children[0].get("surface_id") != native_surface_id(child):
        raise RuntimeError("Rime Return's exact native child became foreign or ambiguous")
    current = children[0] if children else None
    if current is not None and type(current.get("presented")) is not bool:
        raise RuntimeError("Rime Return's child withdrawal is unobservable")
    primaries = [row for row in rows if row.get("app_id") == primary["app_id"]
                 and not row.get("floating_id")
                 and row.get("surface_id") == native_surface_id(primary)]
    sample = {"exact_child_exists": current is not None,
              "exact_primary_exists": len(primaries) == 1,
              "primary_keyboard_focused": len(primaries) == 1
                  and primaries[0].get("keyboard_focused") is True,
              "withdrawn": current is None or current["presented"] is False}
    for field in ("presented", "keyboard_focused", "suspended", "transferring",
                  "pixels_ready", "capture_pending"):
        sample["child_" + field] = current is not None and current.get(field) is True
    sample["child_error_present"] = current is not None and current.get("last_error") is not None
    sample["ordinary_child_route"] = current is not None \
        and current.get("input_route") is None and current.get("input_source_surface_id") is None
    return sample


def wait_for_ime_return_withdrawal(socket, output, child, primary, input_method, process):
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = ime_return_withdrawal_observation(
            call(socket, {"op": "foreign_frames"}), child, primary)
        sample["text_input_active"] = chat_text_input_active(socket) is True
        sample["preedit_active"] = chat_input_method_preedit_active(socket) is True
        sample["same_input_method_process"] = same_fcitx_process(input_method) is True
        evidence["observation_count"] += 1
        if evidence["initial"] is None:
            evidence["initial"] = sample.copy()
        evidence["last"] = sample
        (output / "ime-return-withdrawal-observations.json").write_text(
            json.dumps(evidence, indent=2, allow_nan=False) + "\n")
        return sample["withdrawn"]

    return wait_for("Rime Float withdrawal", observe, process)


def wait_for_floating_keyboard_seat(socket, previous, output, process):
    """Require the actual same ordinary child seat before a physical Return key."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = dict.fromkeys(("exact_child_exists", "presented", "pixels_ready",
                                "keyboard_focused", "suspended", "transferring",
                                "ordinary_route", "accepted"), False)
        evidence["observation_count"] += 1
        try:
            rows = [row for row in foreign_frames(call(socket, {"op": "foreign_frames"}))
                    if row.get("app_id") == previous["app_id"]
                    and row.get("floating_id") == previous["floating_id"]
                    and row.get("surface_id") == native_surface_id(previous)]
            sample["exact_child_exists"] = len(rows) == 1
            if len(rows) != 1:
                raise RuntimeError("keyboard Return lost its exact detached native child")
            current = rows[0]
            names = ("presented", "pixels_ready", "keyboard_focused", "suspended", "transferring")
            if type(current.get("frame_seq")) is not int \
                    or current["frame_seq"] < previous["frame_seq"] \
                    or any(type(current.get(name)) is not bool for name in names):
                raise RuntimeError("keyboard Return child state is malformed or stale")
            sample.update({name: current[name] for name in names})
            sample["ordinary_route"] = current.get("input_route") is None \
                and current.get("input_source_surface_id") is None
            if not sample["ordinary_route"] or current["suspended"] \
                    or current.get("last_error") is not None:
                raise RuntimeError("keyboard Return child has no ordinary active input route")
            sample["accepted"] = current["presented"] and current["pixels_ready"] \
                and current["keyboard_focused"] and not current["transferring"]
            return current if sample["accepted"] else None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "keyboard-return-child-seat.json").write_text(
                json.dumps(evidence, indent=2) + "\n")

    return wait_for("keyboard Return's exact ordinary detached seat", observe, process)


def wait_for_return_editor_continuity(socket, output, primary, child, expected, process,
                                      keyboard=False, semantic=False):
    """Observe real Return adoption/focus publication without repairing editor state."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = dict.fromkeys(("observation_valid", "same_chat_process",
                                "exact_primary_present", "primary_keyboard_focused",
                                "primary_presented", "primary_pixels_ready",
                                "primary_capture_pending", "pending_primary_pixels",
                                "primary_error_present", "child_error_present",
                                "exact_child_exists", "child_keyboard_focused",
                                "child_presented", "child_suspended", "child_transferring",
                                "ordinary_route", "pending_adoption", "pending_focus_publication",
                                "accessibility_observable", "pending_accessibility_publication",
                                "content_matches", "caret_equal", "selection_equal",
                                "accessibility_focus_equal", "accessibility_focused_before",
                                "accessibility_focused_after", "accepted"), False)
        evidence["observation_count"] += 1
        try:
            response = call(socket, {"op": "foreign_frames"})
            current = current_same_primary_presentation(response, primary)
            children = [row for row in foreign_frames(response)
                        if row.get("app_id") == child["app_id"]
                        and row.get("floating_id") == child["floating_id"]
                        and row.get("surface_id") == native_surface_id(child)]
            sample["exact_primary_present"] = current is not None
            sample["exact_child_exists"] = len(children) == 1
            if current is None or len(children) != 1:
                raise RuntimeError("Return lost its exact native primary or child")
            returned_child = children[0]
            if any(type(current.get(name)) is not bool for name in (
                    "presented", "pixels_ready", "keyboard_focused")) \
                    or any(type(returned_child.get(name)) is not bool for name in (
                        "presented", "keyboard_focused", "suspended", "transferring")) \
                    or type(returned_child.get("frame_seq")) is not int \
                    or returned_child["frame_seq"] < child["frame_seq"]:
                raise RuntimeError("Return's native adoption state is malformed or stale")
            sample.update({
                "primary_keyboard_focused": current["keyboard_focused"],
                "primary_presented": current["presented"],
                "primary_pixels_ready": current["pixels_ready"],
                "primary_capture_pending": current.get("capture_pending") is True,
                "pending_primary_pixels": not current["pixels_ready"]
                    and current.get("capture_pending") is True,
                "primary_error_present": current.get("last_error") is not None,
                "child_error_present": returned_child.get("last_error") is not None,
                "child_keyboard_focused": returned_child["keyboard_focused"],
                "child_presented": returned_child["presented"],
                "child_suspended": returned_child["suspended"],
                "child_transferring": returned_child["transferring"],
                "ordinary_route": all(row.get("input_route") is None
                                      and row.get("input_source_surface_id") is None
                                      for row in (current, returned_child)),
            })
            if not current["presented"] \
                    or (not current["pixels_ready"] and not sample["pending_primary_pixels"]) \
                    or (current["pixels_ready"] and sample["primary_capture_pending"]) \
                    or any(row.get("last_error") is not None for row in (current, returned_child)) \
                    or not sample["ordinary_route"] or not returned_child["suspended"] \
                    or returned_child["presented"]:
                raise RuntimeError("Return has no exact ordinary native adoption route")
            if current["keyboard_focused"]:
                if returned_child["keyboard_focused"] or returned_child["transferring"]:
                    raise RuntimeError("Return's primary seat contradicts the retained child")
            else:
                if not returned_child["keyboard_focused"]:
                    raise RuntimeError("Return lost both exact native keyboard seats")
                sample["pending_adoption"] = True
            sample["observation_valid"] = True
            # An AT transaction can replace the tree during this exact native
            # handoff. Missing AT is never success: only the validated native
            # subjects above permit another read within the existing deadline.
            access = agent_conversation_accessibility(expected["access"]["agent"])
            if access is None:
                sample["pending_accessibility_publication"] = True
                return None
            sample["accessibility_observable"] = True
            sample["same_chat_process"] = access["process"] == expected["access"]["process"]
            if not sample["same_chat_process"] or access.get("detached") is not False \
                    or access.get("agent") != expected["access"]["agent"]:
                raise RuntimeError("Return replaced the private editor's native owner or route")
            attached = private_editor_observation(access, expected["draft"])
            continuity = closed_editor_continuity(expected["observation"], attached)
            sample.update(continuity)
            if not all(continuity[name] for name in (
                    "content_matches", "caret_equal", "selection_equal")):
                raise RuntimeError("Return changed private editor content, caret or selection")
            if any(type(attached.get(name)) is not bool for name in (
                    "draft_matches", "accessibility_focused")):
                raise RuntimeError("Return's private editor continuity is malformed")
            if current["keyboard_focused"]:
                sample["pending_focus_publication"] = not continuity["accessibility_focus_equal"]
                sample["accepted"] = continuity["accessibility_focus_equal"] \
                    and current["pixels_ready"]
            return (current, access, attached) if sample["accepted"] else None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            receipt = "semantic-return-editor-adoption-observations.json" if semantic \
                else "keyboard-return-editor-adoption-observations.json" if keyboard \
                else "return-editor-adoption-observations.json"
            (output / receipt).write_text(
                json.dumps(evidence, indent=2, allow_nan=False) + "\n")

    return wait_for("Return's exact native seat and retained editor focus", observe, process)


def float_return_evidence_proven(evidence):
    """Require the structured success field, not arbitrary container truthiness."""
    return isinstance(evidence, dict) and evidence.get("proven") is True


def floating_surface_presenting(response, app_id, floating_id):
    """Return is proved by withdrawal, not failed or temporarily missing paint."""
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == app_id and row.get("floating_id") == floating_id]
    if len(rows) > 1:
        raise RuntimeError("ambiguous detached surface presentation")
    if rows and type(rows[0].get("presented")) is not bool:
        raise RuntimeError("detached surface presentation is unobservable")
    return bool(rows and rows[0]["presented"])


def semantic_editor_continuity_observation(before, after, response, child, same_process):
    """Closed semantic handoff observations carry no private values or native IDs."""
    native = dict.fromkeys(("observation_valid", "same_chat_process", "exact_child_exists",
                           "child_frame_nonregressing", "child_keyboard_focused",
                           "child_presented", "child_pixels_ready", "child_suspended",
                           "child_transferring", "child_error_present", "ordinary_child_route"), False)
    native["same_chat_process"] = same_process is True
    rows = [row for row in foreign_frames(response)
            if row.get("app_id") == child["app_id"]
            and row.get("floating_id") == child["floating_id"]
            and row.get("surface_id") == native_surface_id(child)]
    native["exact_child_exists"] = len(rows) == 1
    if len(rows) == 1:
        current = rows[0]
        fields = ("keyboard_focused", "presented", "pixels_ready", "suspended", "transferring")
        native["child_frame_nonregressing"] = type(current.get("frame_seq")) is int \
            and current["frame_seq"] >= child["frame_seq"]
        native.update({"child_" + name: current.get(name) is True for name in fields})
        native["child_error_present"] = current.get("last_error") is not None
        native["ordinary_child_route"] = current.get("input_route") is None \
            and current.get("input_source_surface_id") is None
        native["observation_valid"] = native["child_frame_nonregressing"] \
            and all(type(current.get(name)) is bool for name in fields)
    return {"editor": closed_editor_continuity(before, after), "native": native}


def wait_for_semantic_float_editor_continuity(socket, output, child, expected, process):
    """Require exact adopted child ownership and original editor state without input."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = dict.fromkeys(("native_observation_valid", "exact_child_exists",
                                "child_keyboard_focused", "child_presented", "child_pixels_ready",
                                "child_frame_nonregressing", "child_capture_pending",
                                "pending_child_pixels",
                                "child_suspended", "child_transferring", "child_error_present",
                                "ordinary_child_route", "same_chat_process", "accessibility_observable",
                                "pending_accessibility_publication", "pending_focus_publication",
                                "content_matches", "caret_equal", "selection_equal",
                                "accessibility_focus_equal", "accessibility_focused_before",
                                "accessibility_focused_after", "accepted"), False)
        evidence["observation_count"] += 1
        try:
            response = call(socket, {"op": "foreign_frames"})
            rows = [row for row in foreign_frames(response)
                    if row.get("app_id") == child["app_id"]
                    and row.get("floating_id") == child["floating_id"]]
            sample["exact_child_exists"] = len(rows) == 1 \
                and rows[0].get("surface_id") == native_surface_id(child)
            if not sample["exact_child_exists"]:
                raise RuntimeError("semantic Float lost its exact presented native child")
            current = rows[0]
            sample["child_frame_nonregressing"] = type(current.get("frame_seq")) is int \
                and current["frame_seq"] >= child["frame_seq"]
            if not sample["child_frame_nonregressing"]:
                raise RuntimeError("semantic Float's exact child frame regressed or is malformed")
            fields = ("keyboard_focused", "presented", "pixels_ready", "suspended", "transferring")
            if any(type(current.get(name)) is not bool for name in fields):
                raise RuntimeError("semantic Float's native ownership is malformed")
            if "capture_pending" in current and type(current["capture_pending"]) is not bool:
                raise RuntimeError("semantic Float's capture state is malformed")
            sample.update({"child_" + name: current[name] for name in fields})
            sample["child_capture_pending"] = current.get("capture_pending") is True
            sample["pending_child_pixels"] = not current["pixels_ready"] \
                and sample["child_capture_pending"]
            sample["child_error_present"] = current.get("last_error") is not None
            sample["ordinary_child_route"] = current.get("input_route") is None \
                and current.get("input_source_surface_id") is None
            if not current["keyboard_focused"] or not current["presented"] \
                    or current["suspended"] \
                    or current["transferring"] or sample["child_error_present"] \
                    or not sample["ordinary_child_route"]:
                raise RuntimeError("semantic Float has no exact adopted ordinary child seat")
            if current["pixels_ready"] and sample["child_capture_pending"] \
                    or not current["pixels_ready"] and not sample["pending_child_pixels"]:
                raise RuntimeError("semantic Float's exact child pixels are contradictory or unavailable")
            sample["native_observation_valid"] = True
            access = agent_conversation_accessibility(expected["access"]["agent"], detached=True)
            if access is None:
                sample["pending_accessibility_publication"] = True
                return None
            sample["accessibility_observable"] = True
            sample["same_chat_process"] = access["process"] == expected["access"]["process"]
            if not sample["same_chat_process"] or access.get("detached") is not True \
                    or access.get("agent") != expected["access"]["agent"]:
                raise RuntimeError("semantic Float replaced the exact private editor owner or route")
            detached = private_editor_observation(access, expected["draft"])
            continuity = closed_editor_continuity(expected["observation"], detached)
            sample.update(continuity)
            if not all(continuity[name] for name in (
                    "content_matches", "caret_equal", "selection_equal")):
                raise RuntimeError("semantic Float changed private editor content, caret or selection")
            if type(detached.get("accessibility_focused")) is not bool:
                raise RuntimeError("semantic Float's editor focus is unobservable")
            editor_ready = detached == expected["observation"] \
                and detached["accessibility_focused"] is True
            sample["pending_focus_publication"] = not editor_ready
            sample["accepted"] = current["pixels_ready"] and editor_ready
            return (current, access, detached) if sample["accepted"] else None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "semantic-float-editor-adoption-observations.json").write_text(
                json.dumps(evidence, indent=2, allow_nan=False) + "\n")

    return wait_for("semantic Float's exact retained editor focus publication", observe, process)


def native_semantic_float_return(socket, room, record, draft, previous, process, output):
    """Exercise the published automation door without replacing native paint proof."""
    access = wait_for("semantic Float's attached editor",
                      lambda: agent_conversation_accessibility("Tide"), process)
    before = private_editor_observation(access, draft)
    chat_process = access["process"]
    expected = {"access": access, "draft": draft, "observation": before}
    float_action = wait_for("published Tide Float action", lambda: offered_agent_action(
        socket, room, record, "tide", "float-agent-conversation"), process)
    wait_terminal(socket, invoke(socket, float_action, {}), process)
    frame = wait_for("semantic Float's freshly presented native pixels", lambda:
                     fresh_floating_presentation(presented_floating_frame(
                         call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), previous),
                     process)
    frame, access, detached = wait_for_semantic_float_editor_continuity(
        socket, output, frame, expected, process,
    )
    (output / "semantic-float-editor-continuity.json").write_text(json.dumps(
        semantic_editor_continuity_observation(
            before, detached, call(socket, {"op": "foreign_frames"}), frame,
            access["process"] == chat_process), indent=2) + "\n")
    if detached != before:
        raise RuntimeError("semantic Float changed the retained private editor state")
    return_action = wait_for("published Tide Return action", lambda: offered_agent_action(
        socket, room, record, "tide", "return-agent-conversation"), process)
    primary = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
    if primary is None:
        raise RuntimeError("semantic Return cannot observe the primary surface")
    wait_terminal(socket, invoke(socket, return_action, {}), process)
    wait_for("semantic Return's native withdrawal", lambda: not floating_surface_presenting(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat", frame["floating_id"]), process)
    returned_primary = wait_for("semantic Return's primary repaint", lambda:
             fresh_primary_presentation(call(socket, {"op": "foreign_frames"}), primary), process)
    if native_surface_id(returned_primary) != native_surface_id(primary):
        raise RuntimeError("semantic Return replaced Chat's exact native primary surface")
    returned_primary, access, returned = wait_for_return_editor_continuity(
        socket, output, returned_primary, frame, expected, process, semantic=True,
    )
    (output / "semantic-return-editor-continuity.json").write_text(json.dumps(
        semantic_editor_continuity_observation(
            before, returned, call(socket, {"op": "foreign_frames"}), frame,
            access["process"] == chat_process), indent=2) + "\n")
    if returned != before:
        raise RuntimeError("semantic Return changed the retained private editor state")
    return {"semantic_float": True, "semantic_return": True}


def fresh_primary_presentation(response, previous):
    current = presented_primary_frame(response, "so.tilo.chat")
    return current if current is not None and current["frame_seq"] > previous["frame_seq"] else None


def current_same_primary_presentation(response, previous):
    """Observe current seat state only on the exact retained native source."""
    current = presented_primary_frame(response, previous["app_id"])
    if current is None or native_surface_id(current) != native_surface_id(previous) \
            or current["frame_seq"] < previous["frame_seq"]:
        return None
    return current


def detached_ime_input_route(response, previous_child, previous_source):
    """Require either the exact child seat or its authenticated live-preedit source."""
    child = current_same_floating_presentation(response, previous_child)
    if child is None:
        raise RuntimeError("Float's exact native destination is no longer presented")
    if type(child.get("keyboard_focused")) is not bool:
        raise RuntimeError("Float's keyboard focus is unobservable")
    if child.get("suspended") is not False or child.get("transferring") is not False:
        raise RuntimeError("Float's input destination is suspended or still transferring")
    route = child.get("input_route")
    source_id = child.get("input_source_surface_id")
    if child["keyboard_focused"] is True:
        if route is not None or source_id is not None:
            raise RuntimeError("focused Float exposes a contradictory input continuation")
        return child, {
            "child_keyboard_focused": True,
            "source_preedit_continuation": False,
        }
    if route != "floating-preedit-source" or type(source_id) is not int:
        raise RuntimeError("Float has neither its keyboard seat nor an authenticated preedit continuation")
    if source_id != native_surface_id(previous_source):
        raise RuntimeError("Float's preedit continuation names a foreign native source")
    source = current_same_primary_presentation(response, previous_source)
    if source is None or native_surface_id(source) != source_id:
        raise RuntimeError("Float's preedit continuation source is stale or no longer presented")
    if type(source.get("keyboard_focused")) is not bool:
        raise RuntimeError("Float's preedit continuation source focus is unobservable")
    if source["keyboard_focused"] is not True:
        raise RuntimeError("Float's preedit continuation source no longer owns the keyboard seat")
    return child, {
        "child_keyboard_focused": False,
        "source_preedit_continuation": True,
    }


def returned_ime_input_route(response, previous_primary, previous_child):
    """Require primary focus or its exact authenticated child preedit proxy."""
    primary = current_same_primary_presentation(response, previous_primary)
    if primary is None:
        raise RuntimeError("Return's exact native destination is no longer presented")
    if type(primary.get("keyboard_focused")) is not bool:
        raise RuntimeError("Return's primary keyboard focus is unobservable")
    route = primary.get("input_route")
    source_id = primary.get("input_source_surface_id")
    if primary["keyboard_focused"] is True:
        if route is not None or source_id is not None:
            raise RuntimeError("focused Return destination exposes a contradictory continuation")
        return primary, {
            "primary_keyboard_focused": True,
            "child_preedit_continuation": False,
        }
    if route != "floating-preedit-child" or type(source_id) is not int:
        raise RuntimeError("Return has neither its keyboard seat nor an authenticated preedit continuation")
    if source_id != native_surface_id(previous_child):
        raise RuntimeError("Return's preedit continuation names a foreign native child")
    children = [row for row in foreign_frames(response)
                if row.get("app_id") == previous_child.get("app_id")
                and row.get("floating_id") == previous_child.get("floating_id")
                and row.get("surface_id") == source_id]
    if len(children) != 1:
        raise RuntimeError("Return's exact preedit child is absent or ambiguous")
    child = children[0]
    if type(child.get("frame_seq")) is not int \
            or child["frame_seq"] < previous_child["frame_seq"]:
        raise RuntimeError("Return's preedit child observation is stale")
    if child.get("suspended") is not True or child.get("transferring") is not False:
        raise RuntimeError("Return's preedit child is not the retained internal proxy")
    if child.get("keyboard_focused") is not True:
        raise RuntimeError("Return's preedit child no longer owns the keyboard seat")
    return primary, {
        "primary_keyboard_focused": False,
        "child_preedit_continuation": True,
    }


def returned_ime_observation(first, current, same_process, text_input_active, same_input_method):
    """Closed Return diagnostics never include native IDs, editor text or errors."""
    return {
        "first_frame_seq": first["frame_seq"],
        "current_frame_seq": current["frame_seq"] if current is not None else None,
        "same_source_presented": current is not None,
        "initial_keyboard_focused": first.get("keyboard_focused") is True,
        "keyboard_focused": current.get("keyboard_focused") is True if current is not None else False,
        "same_chat_process": same_process is True,
        "chat_text_input_active": text_input_active is True,
        "same_input_method_process": same_input_method is True,
        "private_content_recorded": False,
    }


def detached_preedit_guard_observation(response, primary, child, baseline_matches,
                                      text_input_active, preedit_active):
    """Closed classifications of the actual Float guard and captured exact seat."""
    current_child = current_same_floating_presentation(response, child)
    current_primary = current_same_primary_presentation(response, primary)
    if any(type(value) is not bool for value in (
            baseline_matches, text_input_active, preedit_active)):
        raise RuntimeError("Float preedit guard activity is unobservable")
    child_exact = current_child is not None
    source_exact = current_primary is not None
    child_seat = child_exact and current_child.get("keyboard_focused") is True
    source_seat = source_exact and current_primary.get("keyboard_focused") is True
    source_matches = child_exact \
        and type(current_child.get("input_source_surface_id")) is int \
        and current_child["input_source_surface_id"] == native_surface_id(primary)
    expected_route = child_exact and current_child.get("input_route") == "floating-preedit-source"
    destination_ready = child_exact and current_child.get("suspended") is False \
        and current_child.get("transferring") is False
    return {
        "baseline_value_matches": baseline_matches,
        "chat_text_input_active": text_input_active,
        "input_method_preedit_active": preedit_active,
        "exact_child_present": child_exact,
        "exact_source_present": source_exact,
        "child_keyboard_focused": child_seat,
        "source_keyboard_focused": source_seat,
        "route_matches_expected": expected_route,
        "source_matches_exact": source_matches,
        "child_destination_ready": destination_ready,
        "source_preedit_proxy_valid": destination_ready and expected_route
        and source_matches and source_seat,
        "direct_child_seat_valid": destination_ready and child_seat
        and current_child.get("input_route") is None
        and current_child.get("input_source_surface_id") is None,
    }


def capture_return_input_observations(socket, output, primary, child, same_chat, fcitx_identity):
    """Four diagnostic samples; identities stay in memory and never grant input."""
    samples = []
    path = output / "ime-returned-input-observations.json"

    def persist():
        path.write_text(json.dumps(samples, indent=2, allow_nan=False) + "\n")

    persist()
    for index in range(4):
        if index:
            time.sleep(0.05)
        sample = dict.fromkeys((
            "observation_valid", "exact_primary_present", "primary_keyboard_focused",
            "exact_child_exists", "child_keyboard_focused", "child_presented",
            "child_suspended", "child_transferring", "route_matches_expected",
            "source_matches_exact_child", "input_method_preedit_active",
            "chat_text_input_active", "same_chat_process", "same_input_method_process",
        ), False)
        samples.append(sample)
        try:
            response = call(socket, {"op": "foreign_frames"})
            rows = foreign_frames(response)
            primaries = [row for row in rows if row.get("app_id") == primary["app_id"]
                         and row.get("surface_id") == native_surface_id(primary)
                         and row.get("floating_id") is None]
            children = [row for row in rows if row.get("app_id") == child["app_id"]
                        and row.get("surface_id") == native_surface_id(child)
                        and row.get("floating_id") == child["floating_id"]]
            if len(primaries) != 1 or len(children) != 1:
                raise RuntimeError("Return diagnostic subjects are unobservable")
            current_primary, current_child = primaries[0], children[0]
            for row, fields in ((current_primary, ("presented", "keyboard_focused")),
                                (current_child, ("presented", "keyboard_focused",
                                                 "suspended", "transferring"))):
                if any(type(row.get(field)) is not bool for field in fields):
                    raise RuntimeError("Return diagnostic fields are unobservable")
            if type(same_chat) is not bool:
                raise RuntimeError("Return diagnostic process continuity is unobservable")
            sample.update({
                "exact_primary_present": current_primary["presented"],
                "primary_keyboard_focused": current_primary["keyboard_focused"],
                "exact_child_exists": True,
                "child_keyboard_focused": current_child["keyboard_focused"],
                "child_presented": current_child["presented"],
                "child_suspended": current_child["suspended"],
                "child_transferring": current_child["transferring"],
                "route_matches_expected": current_primary.get("input_route") == "floating-preedit-child",
                "source_matches_exact_child": type(current_primary.get("input_source_surface_id")) is int
                and current_primary["input_source_surface_id"] == native_surface_id(child),
                "input_method_preedit_active": chat_input_method_preedit_active(socket),
                "chat_text_input_active": chat_text_input_active(socket),
                "same_chat_process": same_chat,
                "same_input_method_process": same_fcitx_process(fcitx_identity),
            })
            if any(type(value) is not bool for value in sample.values()):
                raise RuntimeError("Return diagnostic activity is unobservable")
            sample["observation_valid"] = True
        except Exception:
            # Persist only the closed snapshot, never arbitrary exception text.
            sample.update({key: value if type(value) is bool else False
                           for key, value in sample.items()})
            sample["observation_valid"] = False
            persist()
            raise RuntimeError("Return input diagnostic observation failed") from None
        persist()
    return response


def wait_for_return_input_route(socket, output, primary, child, same_chat, fcitx_identity,
                                process, initial_response=None):
    """Wait only for the exact active child's pending Return receipt to be consumed."""
    evidence = {"observation_count": 0, "initial": None, "last": None}
    pending_response = initial_response

    def observe():
        nonlocal pending_response
        sample = dict.fromkeys(("observation_valid", "exact_primary_present",
                                "exact_child_exists", "preedit_active", "text_input_active",
                                "same_chat_process", "same_input_method_process",
                                "pending_adoption", "strict_route_accepted"), False)
        evidence["observation_count"] += 1
        try:
            response = pending_response if pending_response is not None \
                else call(socket, {"op": "foreign_frames"})
            pending_response = None
            current_primary = current_same_primary_presentation(response, primary)
            children = [row for row in foreign_frames(response)
                        if row.get("app_id") == child["app_id"]
                        and row.get("surface_id") == native_surface_id(child)
                        and row.get("floating_id") == child["floating_id"]]
            sample["exact_primary_present"] = current_primary is not None
            sample["exact_child_exists"] = len(children) == 1
            if current_primary is None or len(children) != 1:
                raise RuntimeError("Return adoption lost its exact native subjects")
            current_child = children[0]
            if type(current_child.get("frame_seq")) is not int \
                    or current_child["frame_seq"] < child["frame_seq"] \
                    or type(current_primary.get("keyboard_focused")) is not bool \
                    or any(type(current_child.get(name)) is not bool for name in (
                        "keyboard_focused", "presented", "suspended", "transferring")):
                raise RuntimeError("Return adoption state is malformed or stale")
            activity = (chat_input_method_preedit_active(socket), chat_text_input_active(socket),
                        same_chat, same_fcitx_process(fcitx_identity))
            if any(type(value) is not bool for value in activity):
                raise RuntimeError("Return adoption activity is unobservable")
            sample.update(zip(("preedit_active", "text_input_active", "same_chat_process",
                               "same_input_method_process"), activity))
            if not all(activity):
                raise RuntimeError("Return adoption lost active composition or process continuity")
            if current_child["transferring"]:
                if current_primary["keyboard_focused"] \
                        or current_primary.get("input_route") is not None \
                        or current_primary.get("input_source_surface_id") is not None \
                        or current_child["keyboard_focused"] is not True \
                        or current_child["presented"] is not False \
                        or current_child["suspended"] is not True \
                        or current_child.get("input_route") is not None \
                        or current_child.get("input_source_surface_id") is not None:
                    raise RuntimeError("Return adoption is not the exact pending child transfer")
                sample["pending_adoption"] = True
                result = None
            else:
                result = returned_ime_input_route(response, primary, child)
                sample["strict_route_accepted"] = True
            sample["observation_valid"] = True
            return result
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "ime-returned-adoption-observations.json").write_text(
                json.dumps(evidence, indent=2, allow_nan=False) + "\n")

    return wait_for("Return's exact active preedit input adoption", observe, process)


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
    with hold_physical_modifiers(socket, shift=True):
        for _ in range(5):
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    selected = wait_for_private_selection(
        socket, access, draft, [len(draft) - 5, len(draft)], process, output)
    if selected["selection"] != [len(draft) - 5, len(draft)]:
        raise RuntimeError("physical Shift+ArrowLeft did not select the expected editor range")
    require_ok(call(socket, {"op": "key", "key": "ArrowRight"}))

    # The desktop emoji picker is the production input-method route:
    # Super+. → query → Enter commits through zwp_input_method_v2/text_input_v3.
    physical_chord(socket, ".", logo=True)
    require_ok(call(socket, {"op": "type", "text": "thumbs"}))
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    draft += "👍"
    wait_for("real input-method commit in the private editor",
             lambda: private_editor_value_matches(access, draft), process)
    with hold_physical_modifiers(socket, shift=True):
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    before = wait_for_private_selection(
        socket, access, draft, [len(draft) - 1, len(draft)], process, output,
        require_focused=True)
    if before["selection"] != [len(draft) - 1, len(draft)]:
        raise RuntimeError("the IME result could not be selected through physical input")
    if before["accessibility_focused"] is not True:
        raise RuntimeError("the actual local editor lost accessibility focus before Float")

    # Leave one real edit in the TextInput undo stack. It is intentionally not
    # undone until after the native surface handoff.
    require_ok(call(socket, {"op": "key", "key": "ArrowRight"}))
    undo_base = draft
    require_ok(call(socket, {"op": "type", "text": "!"}))
    draft += "!"
    wait_for("physical edit retained for detached undo",
             lambda: private_editor_value_matches(access, draft), process)
    physical_chord(socket, "ArrowLeft", shift=True)
    before = wait_for_private_selection(
        socket, access, draft, [len(draft) - 1, len(draft)], process, output)
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
    float_geometry = presented_content_geometry(frame)
    physically_click_editor(socket, access, float_geometry)
    require_chat_keyboard_focus_after_click(socket, output, detached=True)
    physical_chord(socket, "z", ctrl=True)
    draft = undo_base
    wait_for("pre-detach undo history in the detached editor",
             lambda: private_editor_value_matches(access, draft), process)
    require_ok(call(socket, {"op": "type", "text": "?"}))
    draft += "?"
    wait_for("detached edit retained for attached undo",
             lambda: private_editor_value_matches(access, draft), process)
    physical_chord(socket, "ArrowLeft", shift=True)
    before_return_editor = wait_for_private_selection(
        socket, access, draft, [len(draft) - 1, len(draft)], process, output)
    before_return = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
    if before_return is None:
        raise RuntimeError("Chat's primary presentation disappeared before Return")
    current_frame, access, return_target = wait_for(
        "current detached Tide Return control",
        lambda: current_detached_control_target(
            socket, frame, "Tide", draft, before_return_editor
        ),
        process,
    )
    return_x = return_target["global_center"]["x"]
    return_y = return_target["global_center"]["y"]
    require_ok(call(socket, {"op": "click_at", "x": return_x, "y": return_y}))
    wait_for("the detached Chat window to stop presenting", lambda: not floating_surface_presenting(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat",
        current_frame["floating_id"]), process)

    def repainted_primary():
        primary = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
        return primary if primary is not None and primary["frame_seq"] > before_return["frame_seq"] else None

    primary = wait_for("Chat's primary window to repaint after Return", repainted_primary, process)
    if native_surface_id(primary) != native_surface_id(before_return):
        raise RuntimeError("Return replaced Chat's exact native primary surface")
    primary, access, attached = wait_for_return_editor_continuity(
        socket, output, primary, current_frame,
        {"access": access, "draft": draft, "observation": before_return_editor}, process,
    )
    (output / "return-editor-continuity.json").write_text(
        json.dumps(closed_editor_continuity(before_return_editor, attached), indent=2) + "\n")
    if attached != before_return_editor:
        raise RuntimeError("Return changed private editor content, caret, selection or keyboard focus")
    physically_click_editor(socket, access, presented_content_geometry(primary))
    require_chat_keyboard_focus_after_click(socket, output)
    physical_chord(socket, "z", ctrl=True)
    draft = undo_base
    wait_for("detached undo history in the returned editor",
             lambda: private_editor_value_matches(access, draft), process)
    attached = private_editor_observation(access, draft)
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"after-return-{size}.png")}))

    # Pointer drag/Return above prove same-editor focus continuity. Separately,
    # focus the already-published controls and let physical Enter activate the
    # same production handoff paths; AT never invokes either action here.
    access = wait_for("keyboard Float control", lambda: agent_conversation_accessibility("Tide"), process)
    access = wait_for("current attached keyboard Float control",
                      lambda: current_owned_conversation_accessibility(access), process)
    focus_agent_conversation_control(access, access["drag"], "Float")
    access = wait_for_conversation_control_focus(access, output, process, detached=False, control="drag")
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    keyboard_frame = wait_for_keyboard_float_presentation(socket, output, frame, primary, process)
    keyboard_access = wait_for(
        "keyboard-activated detached Tide editor",
        lambda: agent_conversation_accessibility("Tide", detached=True), process,
    )
    keyboard_frame = wait_for_floating_keyboard_seat(socket, keyboard_frame, output, process)
    require_chat_keyboard_focus_after_click(socket, output, detached=True)
    keyboard_detached = private_editor_observation(keyboard_access, draft)
    focus_agent_conversation_control(keyboard_access, keyboard_access["control"], "Return")
    keyboard_frame = wait_for_floating_keyboard_seat(socket, keyboard_frame, output, process)
    keyboard_before_return = current_same_primary_presentation(
        call(socket, {"op": "foreign_frames"}), primary)
    if keyboard_before_return is None:
        raise RuntimeError("keyboard Return lost Chat's exact native primary surface")
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    wait_for("keyboard-activated detached Chat withdrawal", lambda: not floating_surface_presenting(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat",
        keyboard_frame["floating_id"]), process)

    def keyboard_returned_primary():
        current = current_same_primary_presentation(
            call(socket, {"op": "foreign_frames"}), keyboard_before_return)
        return current if current is not None \
            and current["frame_seq"] > keyboard_before_return["frame_seq"] else None

    keyboard_primary = wait_for(
        "keyboard Return's exact primary repaint", keyboard_returned_primary, process,
    )
    keyboard_primary, keyboard_return, keyboard_attached = wait_for_return_editor_continuity(
        socket, output, keyboard_primary, keyboard_frame,
        {"access": keyboard_access, "draft": draft, "observation": keyboard_detached},
        process, keyboard=True,
    )
    (output / "keyboard-return-editor-continuity.json").write_text(
        json.dumps(closed_editor_continuity(keyboard_detached, keyboard_attached), indent=2) + "\n")
    if keyboard_attached != keyboard_detached:
        raise RuntimeError("keyboard Float/Return changed private editor state")
    semantic = native_semantic_float_return(
        socket, room, record, draft, keyboard_frame, process, output)
    (output / "float-return.json").write_text(json.dumps({
        **semantic,
        "physical_typing": True, "physical_selection": True, "physical_undo": True,
        "undo_history_crossed_float": True, "undo_history_crossed_return": True,
        "input_method_commit": True, "pointer_drag_detach": True,
        "pointer_return": True, "keyboard_float": True, "keyboard_return": True,
        "native_floating_id": frame["floating_id"],
        "native_surface_id": native_surface_id(frame),
        "frame_seq": frame["frame_seq"],
        "returned_primary_frame_seq": primary["frame_seq"],
        "private_editor": attached, "private_content_recorded": False,
        "test_owned_draft_rendered": True,
        "lifecycle_proven": False,
    }, indent=2) + "\n")
    return {
        "proven": True,
        "floating_id": frame["floating_id"],
        "surface_id": native_surface_id(frame),
        "frame_seq": frame["frame_seq"],
    }


def drag_agent_conversation(socket, access, primary, size):
    """Drag the existing shared handle through the compositor's real seat."""
    geometry = presented_content_geometry(primary)
    target = component_global_target(access, access["drag"], geometry, "drag handle")
    drag_x = target["global_center"]["x"]
    drag_y = target["global_center"]["y"]
    desktop_width, desktop_height = (int(value) for value in size.split("x", 1))
    placement = geometry["placement"]
    drag_to_x = (max(8.0, placement["x"] - 96.0) if placement["x"] >= 140
                 else min(desktop_width - 8.0, placement["x"] + placement["w"] + 96.0))
    drag_to_y = min(desktop_height - 8.0, max(8.0, drag_y + 48.0))
    require_ok(call(socket, {
        "op": "drag", "x_from": drag_x, "y_from": drag_y,
        "x_to": drag_to_x, "y_to": drag_to_y, "steps": 16,
    }))
    return {
        "handle_client_rect": target["client_rect"],
        "drag_from": target["global_center"],
        "drag_to": {"x": drag_to_x, "y": drag_to_y},
        "primary_placement": geometry["placement"],
        "primary_content": geometry["content"],
        "primary_content_origin": geometry["origin"],
        "primary_frame_seq": primary["frame_seq"],
    }


def write_ime_drag_refusal(socket, output, size, drag):
    """Retain public geometry and pixels when the real drag never presents."""
    frames = call(socket, {"op": "foreign_frames"})
    primary = presented_primary_frame(frames, "so.tilo.chat")
    floating = presented_floating_frame(frames, "so.tilo.chat")
    observed_floating = [row for row in foreign_frames(frames)
                         if row.get("app_id") == "so.tilo.chat"
                         and isinstance(row.get("floating_id"), str) and row["floating_id"]]
    primary_geometry = presented_content_geometry(primary) if primary is not None else None
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-after-drag-refused-{size}.png"
    )}))
    (output / "ime-drag-refused.json").write_text(json.dumps({
        "phase": "after_physical_drag_without_floating_presentation",
        "drag": drag,
        "primary_presented": primary is not None,
        "primary_frame_advanced": (
            primary is not None and primary["frame_seq"] > drag["primary_frame_seq"]
        ),
        "primary_keyboard_focused": (
            primary.get("keyboard_focused") if primary is not None else None
        ),
        "primary_geometry": primary_geometry,
        "floating_presented": floating is not None,
        "floating_observed_count": len(observed_floating),
        "floating_pixels_ready_count": sum(
            row.get("pixels_ready") is True for row in observed_floating
        ),
        "floating_error_count": sum(
            row.get("last_error") is not None for row in observed_floating
        ),
        "floating_suspended_count": sum(
            row.get("suspended") is True for row in observed_floating
        ),
        "floating_transferring_count": sum(
            row.get("transferring") is True for row in observed_floating
        ),
        "private_content_recorded": False,
    }, indent=2) + "\n")


def preserve_ime_drag_refusal(socket, output, size, drag):
    """Best-effort diagnostics must leave the original timeout authoritative."""
    try:
        write_ime_drag_refusal(socket, output, size, drag)
    except Exception:  # A broken observer is evidence, never a replacement failure.
        try:
            (output / "ime-drag-refused.json").write_text(json.dumps({
                "phase": "after_physical_drag_without_floating_presentation",
                "diagnostic_unavailable": True,
                "private_content_recorded": False,
            }, indent=2) + "\n")
        except OSError:
            pass


def private_editor_numeric_bounds(access):
    """Return bounded geometry of the private field, never its text or hash."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    path = access["composer"]
    reply = client.call(path, "org.a11y.atspi.Text.GetText", "--", "0", "-1")
    value = atspi.text_reply(reply)
    if value is None or len(value) > 32_000:
        raise RuntimeError("the actual private editor length is unobservable")
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
    return {"character_count": len(value), "caret": caret_offset, "selection": selection}


def preserve_attached_preedit_refusal(
        socket, output, size, access, expectations, fcitx_identity, env):
    """Record only fixture-known equality and protocol booleans on refusal."""
    evidence_path = output / "ime-attached-preedit-refused.json"
    try:
        seat = chat_input_method_observation(socket)
        engine = fcitx_rime_observation(env)
        schema = owned_rime_schema_observation(env)
        evidence_path.write_text(json.dumps({
            "phase": expectations["phase"],
            "baseline_retained": private_editor_value_matches(
                access, expectations["baseline"]
            ),
            "direct_latin_committed": private_editor_value_matches(
                access, expectations["direct_latin"]
            ),
            "rime_candidate_committed": private_editor_value_matches(
                access, expectations["rime_commit"]
            ),
            "field_bounds": private_editor_numeric_bounds(access),
            **seat,
            **engine,
            **schema,
            "same_input_method_process": same_fcitx_process(fcitx_identity),
            "private_content_recorded": False,
        }, indent=2) + "\n")
    except Exception:
        try:
            evidence_path.write_text(json.dumps({
                "phase": expectations["phase"],
                "diagnostic_unavailable": True,
                "private_content_recorded": False,
            }, indent=2) + "\n")
        except OSError:
            pass
    try:
        require_ok(call(socket, {"op": "snapshot", "path": str(
            output / f"ime-attached-preedit-refused-{size}.png"
        )}))
    except Exception:
        pass


def baseline_is_exact_rime_ascii_passthrough(socket, access, expected, fcitx_identity, env):
    """Recognize only the closed, independently observed ASCII failure."""
    seat = chat_input_method_observation(socket)
    engine = fcitx_rime_observation(env)
    bounds = private_editor_numeric_bounds(access)
    return bool(
        private_editor_value_matches(access, expected)
        and bounds == {"character_count": len(expected), "caret": len(expected),
                       "selection": None}
        and seat == {
            "keyboard_is_chat": True,
            "text_input_is_chat": True,
            "text_input_active": True,
            "input_method_connected": True,
            "input_method_keyboard_grabbed": True,
            "input_method_preedit_accepted": False,
        }
        and engine == {
            "fcitx_state_observable": True,
            "fcitx_state_active": True,
            "fcitx_engine_observable": True,
            "fcitx_engine_rime": True,
        }
        and same_fcitx_process(fcitx_identity)
    )


def recover_rime_chinese_mode(
        socket, output, size, access, process, fcitx_identity, env, expected):
    """Undo proven ASCII pass-through and use Rime's physical mode chord."""
    if not baseline_is_exact_rime_ascii_passthrough(
            socket, access, expected, fcitx_identity, env):
        raise RuntimeError("the failed Rime baseline was not exact ASCII pass-through")
    physical_chord(socket, "a", ctrl=True)
    selected = wait_for_private_selection(
        socket, access, expected, [0, len(expected)], process, output,
    )
    if selected["selection"] != [0, len(expected)]:
        raise RuntimeError("physical cleanup did not select the exact ASCII pass-through")
    require_ok(call(socket, {"op": "key", "key": "Backspace"}))
    wait_for(
        "physical removal of the exact Rime ASCII pass-through",
        lambda: private_editor_value_matches(access, ""),
        process,
    )
    before = presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
    )
    if before is None:
        raise RuntimeError("Chat disappeared before the physical Rime mode switch")

    # The shipped Rime default imports numbered_mode_switch: Ctrl+Shift+2
    # toggles ascii_mode. This is deterministic only because the exact ASCII
    # state was independently observed above; no controller changes mode.
    physical_chord(socket, "2", ctrl=True, shift=True)
    if not same_fcitx_process(fcitx_identity) or not fcitx_rime_active(env):
        raise RuntimeError("the physical Rime mode switch changed input-method custody")
    require_ok(call(socket, {"op": "type", "text": expected}))

    def genuine_preedit():
        return bool(
            chat_input_method_preedit_active(socket)
            and private_editor_value_matches(access, "")
            and same_fcitx_process(fcitx_identity)
            and fcitx_rime_active(env)
        )

    try:
        wait_for("genuine Rime preedit after the physical mode switch", genuine_preedit, process)
    except Exception:
        (output / "ime-mode-switch-schema.json").write_text(json.dumps(
            owned_rime_schema_observation(env), indent=2) + "\n")
        raise

    def fresh_candidate_frame():
        frame = presented_primary_frame(
            call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
        )
        return frame if frame is not None and frame["frame_seq"] > before["frame_seq"] \
            and genuine_preedit() else None

    frame = wait_for(
        "fresh Rime candidate frame after the physical mode switch",
        fresh_candidate_frame,
        process,
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-preedit-recovered-{size}.png"
    )}))
    return frame


def preserve_detached_commit_refusal(
        socket, output, size, access, baseline, expected, frame, fcitx_identity):
    """Record closed route and fixture-known equality after a failed commit."""
    evidence_path = output / "ime-detached-commit-refused.json"
    try:
        current = current_same_floating_presentation(
            call(socket, {"op": "foreign_frames"}), frame
        )
        direct_latin_space = baseline + "nihao "
        candidate_prefix = baseline + "你"
        literal_space = baseline + " "
        evidence_path.write_text(json.dumps({
            "phase": "detached_space_commit",
            "baseline_retained": private_editor_value_matches(access, baseline),
            "expected_chinese_committed": private_editor_value_matches(access, expected),
            "direct_latin_space_committed": private_editor_value_matches(
                access, direct_latin_space
            ),
            "candidate_prefix_committed": private_editor_value_matches(
                access, candidate_prefix
            ),
            "literal_space_committed": private_editor_value_matches(access, literal_space),
            "same_presented_child": current is not None,
            "keyboard_focused": current is not None
                and current.get("keyboard_focused") is True,
            "chat_text_input_active": chat_text_input_active(socket),
            "input_route_present": current is not None
                and current.get("input_route") is not None,
            "input_source_present": current is not None
                and current.get("input_source_surface_id") is not None,
            "same_input_method_process": same_fcitx_process(fcitx_identity),
            "private_content_recorded": False,
        }, indent=2) + "\n")
    except Exception:
        try:
            evidence_path.write_text(json.dumps({
                "phase": "detached_space_commit",
                "diagnostic_unavailable": True,
                "private_content_recorded": False,
            }, indent=2) + "\n")
        except OSError:
            pass
    try:
        require_ok(call(socket, {"op": "snapshot", "path": str(
            output / f"ime-detached-commit-refused-{size}.png"
        )}))
    except Exception:
        pass


def native_ime_float_return(socket, output, size, process, fcitx_identity, env):
    """Hold a genuine Rime preedit across Float and Return, then commit it."""
    committed_before = "nihao"
    committed_after = "nihao你好"
    access = wait_for(
        "attached Tide editor for Rime handoff",
        lambda: agent_conversation_accessibility(
            "Tide", diagnostic_path=output / "ime-attached-accessibility.json"),
        process,
    )
    chat_process = access["process"]
    primary = wait_for(
        "Chat's presented primary frame for Rime handoff",
        lambda: presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat"),
        process,
    )
    physically_click_editor(socket, access, presented_content_geometry(primary))
    require_chat_keyboard_focus_after_click(socket, output)
    wait_for(
        "Chat's active input method and keyboard grab",
        lambda: chat_input_method_ready(socket),
        process,
    )
    wait_for(
        "the compositor-owned active Rime engine",
        lambda: same_fcitx_process(fcitx_identity) and fcitx_rime_active(env),
        process,
    )

    (output / "ime-baseline-schema.json").write_text(json.dumps(
        owned_rime_schema_observation(env), indent=2) + "\n")
    # Rime's express editor commits its raw composition on Return. Establish
    # one real edit without toggling Fcitx's global active state.  Use a
    # complete pinyin spelling: a lone `u` is not a valid luna-pinyin syllable
    # and may pass through without ever creating a composition.
    require_ok(call(socket, {"op": "type", "text": committed_before}))
    try:
        wait_for(
            "a genuine Rime baseline preedit",
            lambda: chat_input_method_preedit_active(socket),
            process,
        )
    except Exception:
        preserve_attached_preedit_refusal(
            socket, output, size, access, {
                "phase": "baseline_preedit_before_float",
                "baseline": "",
                "direct_latin": committed_before,
                "rime_commit": "你好",
            }, fcitx_identity, env,
        )
        if not baseline_is_exact_rime_ascii_passthrough(
                socket, access, committed_before, fcitx_identity, env):
            raise
        recover_rime_chinese_mode(
            socket, output, size, access, process, fcitx_identity, env, committed_before,
        )
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    wait_for(
        "the pre-detach committed edit",
        lambda: private_editor_value_matches(access, committed_before), process,
    )
    before_preedit = presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
    )
    if before_preedit is None:
        raise RuntimeError("Chat disappeared before genuine Rime preedit")
    require_ok(call(socket, {"op": "type", "text": "nihao"}))
    wait_for(
        "Rime's active preedit before Float",
        lambda: chat_input_method_preedit_active(socket),
        process,
    )

    def attached_preedit_frame():
        frame = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
        if frame is None or frame["frame_seq"] <= before_preedit["frame_seq"]:
            return None
        if not private_editor_value_matches(access, committed_before):
            preserve_attached_preedit_refusal(
                socket, output, size, access, {
                    "phase": "attached_preedit_before_float",
                    "baseline": committed_before,
                    "direct_latin": committed_before + "nihao",
                    "rime_commit": committed_after,
                }, fcitx_identity, env,
            )
            raise RuntimeError("Rime committed before the Float handoff")
        return frame if chat_text_input_active(socket) else None

    preedit_primary = wait_for(
        "genuine Rime preedit in the attached Chat frame", attached_preedit_frame, process
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-preedit-attached-{size}.png"
    )}))

    access = wait_for(
        "current attached Tide editor and drag handle during Rime preedit",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    if not private_editor_value_matches(access, committed_before):
        raise RuntimeError("resolving the Float handle committed the Rime preedit")
    drag = drag_agent_conversation(socket, access, preedit_primary, size)
    try:
        frame = wait_for(
            "Rime composition's detached native pixels",
            lambda: presented_floating_frame(
                call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
            ),
            process,
        )
    except RuntimeError:
        preserve_ime_drag_refusal(socket, output, size, drag)
        raise
    # A real buffer does not prove its controls, focus or editor are usable.
    # Keep the first presented scene even if the following AT/focus gate fails.
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-detached-first-frame-{size}.png"
    )}))
    (output / "ime-detached-native.json").write_text(json.dumps({
        "phase": "after_detached_native_presentation",
        "keyboard_focused": frame["keyboard_focused"],
        "chat_text_input_active": chat_text_input_active(socket),
        "same_input_method_process": same_fcitx_process(fcitx_identity),
        "geometry": presented_content_geometry(frame),
        "private_content_recorded": False,
    }, indent=2) + "\n")
    detached_access = wait_for(
        "detached Tide editor during Rime preedit",
        lambda: agent_conversation_accessibility(
            "Tide", detached=True, diagnostic_path=output / "ime-detached-accessibility.json"),
        process,
    )
    # AT discovery is asynchronous. Keep the first frame above as evidence,
    # but verify the seat now, on that same native window, not its cached
    # pre-adoption observation. This still refuses an unfocused destination.
    detached_response = call(socket, {"op": "foreign_frames"})
    frame, detached_input = detached_ime_input_route(detached_response, frame, preedit_primary)
    if detached_access["process"] != chat_process:
        raise RuntimeError("Float replaced the Chat process")
    detached_guard = detached_preedit_guard_observation(
        detached_response, preedit_primary, frame,
        private_editor_value_matches(detached_access, committed_before),
        chat_text_input_active(socket), chat_input_method_preedit_active(socket),
    )
    (output / "ime-detached-preedit-guard.json").write_text(
        json.dumps(detached_guard, indent=2) + "\n"
    )
    if not private_editor_value_matches(detached_access, committed_before) \
            or not chat_text_input_active(socket) \
            or not chat_input_method_preedit_active(socket):
        raise RuntimeError("Float committed or deactivated the live Rime preedit")
    if not same_fcitx_process(fcitx_identity):
        raise RuntimeError("Float replaced the compositor-owned Rime process")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-preedit-detached-{size}.png"
    )}))

    # Commit through the real source-seat continuation while the same editor
    # is painted in the authenticated child. The child must then receive the
    # ordinary seat before any subsequent physical selection or undo.
    before_commit_frame = frame
    (output / "ime-detached-before-commit-geometry.json").write_text(
        json.dumps(floating_numeric_geometry(frame), indent=2, allow_nan=False) + "\n"
    )
    require_ok(call(socket, {"op": "key", "key": "Space"}))

    def floating_committed_frame():
        current = current_same_floating_presentation(
            call(socket, {"op": "foreign_frames"}), frame
        )
        if current is None or current["frame_seq"] <= frame["frame_seq"] \
                or not private_editor_value_matches(detached_access, committed_after):
            return None
        if current.get("input_route") is not None \
                or current.get("input_source_surface_id") is not None:
            return None
        if current["keyboard_focused"] is not True or not chat_text_input_active(socket):
            return None
        # Preserve the actual rejected measurement too; no identity/content leaves
        # this closed receipt even when the original bounded wait must fail.
        (output / "ime-detached-after-commit-geometry.json").write_text(
            json.dumps(floating_numeric_geometry(current), indent=2, allow_nan=False) + "\n"
        )
        return current if stable_floating_commit_geometry(before_commit_frame, current) else None

    try:
        frame = wait_for(
            "Rime's exact detached commit and child seat", floating_committed_frame, process
        )
    except RuntimeError:
        preserve_detached_commit_refusal(
            socket, output, size, detached_access, committed_before,
            committed_after, frame, fcitx_identity,
        )
        raise
    if detached_access["process"] != chat_process or not same_fcitx_process(fcitx_identity):
        raise RuntimeError("detached Rime commit replaced Chat or the compositor input method")
    (output / "ime-detached-after-commit-geometry.json").write_text(
        json.dumps(floating_numeric_geometry(frame), indent=2, allow_nan=False) + "\n"
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-commit-detached-{size}.png"
    )}))

    with hold_physical_modifiers(socket, shift=True):
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    committed_selection = [len(committed_before), len(committed_after)]
    floating_selection = wait_for_private_selection(
        socket, detached_access, committed_after, committed_selection, process, output
    )
    if floating_selection["selection"] != committed_selection:
        raise RuntimeError("detached physical selection did not cover the Rime commit")
    require_ok(call(socket, {"op": "key", "key": "ArrowRight"}))
    physical_chord(socket, "z", ctrl=True)
    wait_for(
        "detached Rime commit undo",
        lambda: private_editor_value_matches(detached_access, committed_before), process,
    )

    # Start a second genuine composition in the now-focused child. Return
    # while it is still preedit, then commit that same composition in primary.
    before_second_preedit = frame
    require_ok(call(socket, {"op": "type", "text": "nihao"}))

    def detached_second_preedit_frame():
        current = current_same_floating_presentation(
            call(socket, {"op": "foreign_frames"}), before_second_preedit
        )
        if current is None or current["frame_seq"] <= before_second_preedit["frame_seq"]:
            return None
        if not private_editor_value_matches(detached_access, committed_before):
            raise RuntimeError("Rime's second composition committed before Return")
        return current if current["keyboard_focused"] is True \
            and chat_text_input_active(socket) else None

    frame = wait_for(
        "genuine Rime preedit in the detached Chat frame",
        detached_second_preedit_frame,
        process,
    )
    if not same_fcitx_process(fcitx_identity):
        raise RuntimeError("the detached second composition replaced the input method")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-preedit-detached-second-{size}.png"
    )}))

    before_return = presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
    )
    if before_return is None:
        raise RuntimeError("Chat's primary presentation disappeared before Rime Return")
    before_return_editor = private_editor_observation(detached_access, committed_before)
    frame, detached_access, return_target = wait_for(
        "current Rime Return control on the same composed child",
        lambda: current_ime_return_target(
            socket, frame, detached_access, committed_before, before_return_editor, fcitx_identity),
        process,
    )
    (output / "ime-return-target.json").write_text(json.dumps({
        "geometry": presented_content_geometry(frame), "target": return_target,
        "same_chat_process": detached_access["process"] == chat_process,
        "baseline_matches": before_return_editor["draft_matches"],
        "text_input_active": True, "preedit_active": True,
        "same_input_method_process": True, "private_content_recorded": False,
    }, indent=2, allow_nan=False) + "\n")
    return_x, return_y = (return_target["global_center"][axis] for axis in ("x", "y"))
    require_ok(call(socket, {"op": "click_at", "x": return_x, "y": return_y}))
    wait_for_ime_return_withdrawal(socket, output, frame, before_return, fcitx_identity, process)

    def returned_preedit_frame():
        current = current_same_primary_presentation(
            call(socket, {"op": "foreign_frames"}), before_return
        )
        return (current if current is not None
                and current["frame_seq"] > before_return["frame_seq"] else None)

    returned_first = wait_for("Rime preedit's returned primary frame", returned_preedit_frame, process)
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-returned-first-frame-{size}.png"
    )}))
    returned_access = wait_for(
        "returned Tide editor during Rime preedit",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    # AT readiness can follow the first source presentation. Observe the seat
    # now without accepting another primary native source or regressing paint.
    returned_response = capture_return_input_observations(
        socket, output, returned_first, frame,
        returned_access["process"] == chat_process, fcitx_identity,
    )
    returned, returned_input = wait_for_return_input_route(
        socket, output, returned_first, frame,
        returned_access["process"] == chat_process, fcitx_identity, process, returned_response,
    )
    returned_observation = returned_ime_observation(
        returned_first, returned, returned_access["process"] == chat_process,
        chat_text_input_active(socket), same_fcitx_process(fcitx_identity),
    )
    returned_observation.update(returned_input)
    (output / "ime-returned-native.json").write_text(
        json.dumps(returned_observation, indent=2) + "\n"
    )
    if returned_access["process"] != chat_process:
        raise RuntimeError("Return replaced the Chat process")
    returned_editor_observation = {
        "cached": closed_return_editor_observation(
            returned_access, committed_before, committed_after,
            returned_access["process"] == chat_process),
    }
    (output / "ime-returned-editor-observations.json").write_text(
        json.dumps(returned_editor_observation, indent=2) + "\n")
    fresh_returned_access = agent_conversation_accessibility("Tide")
    returned_editor_observation["fresh"] = closed_return_editor_observation(
        fresh_returned_access, committed_before, committed_after,
        fresh_returned_access is not None and fresh_returned_access["process"] == chat_process)
    (output / "ime-returned-editor-observations.json").write_text(
        json.dumps(returned_editor_observation, indent=2) + "\n")
    if not private_editor_value_matches(returned_access, committed_before) \
            or not chat_text_input_active(socket):
        raise RuntimeError("Return committed or deactivated the live Rime preedit")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"ime-preedit-returned-{size}.png"
    )}))

    require_ok(call(socket, {"op": "key", "key": "Space"}))

    def returned_committed_frame():
        current = current_same_primary_presentation(
            call(socket, {"op": "foreign_frames"}), returned
        )
        if current is None or not private_editor_value_matches(returned_access, committed_after):
            return None
        if current.get("input_route") is not None \
                or current.get("input_source_surface_id") is not None:
            return None
        return current if current["keyboard_focused"] is True \
            and chat_text_input_active(socket) else None

    returned = wait_for(
        "Rime's exact post-Return commit and primary seat",
        returned_committed_frame,
        process,
    )
    physical_chord(socket, "z", ctrl=True)
    wait_for(
        "Rime commit undo after Return",
        lambda: private_editor_value_matches(returned_access, committed_before), process,
    )
    physical_chord(socket, "z", ctrl=True)
    wait_for(
        "pre-detach edit undo after both handoffs",
        lambda: private_editor_value_matches(returned_access, ""), process,
    )

    # The same published controls must also support physical keyboard
    # activation. This is a second handoff after composition has settled.
    returned_access = wait_for("current attached Rime keyboard Float control",
        lambda: current_owned_conversation_accessibility(returned_access), process)
    focus_agent_conversation_control(returned_access, returned_access["drag"], "Float")
    returned_access = wait_for_conversation_control_focus(
        returned_access, output, process, detached=False, control="drag")
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    keyboard_frame = wait_for(
        "keyboard-activated Rime proof Float",
        lambda: fresh_floating_presentation(
            presented_floating_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat"),
            frame,
        ),
        process,
    )
    keyboard_access = wait_for(
        "keyboard-activated Rime proof editor",
        lambda: agent_conversation_accessibility("Tide", detached=True), process,
    )
    if not private_editor_value_matches(keyboard_access, ""):
        raise RuntimeError("keyboard Float changed the editor after Rime undo")
    keyboard_frame = wait_for_floating_keyboard_seat(socket, keyboard_frame, output, process)
    focus_agent_conversation_control(keyboard_access, keyboard_access["control"], "Return")
    keyboard_frame = wait_for_floating_keyboard_seat(socket, keyboard_frame, output, process)
    keyboard_access = wait_for_floating_return_control_focus(keyboard_access, output, process)
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    wait_for(
        "keyboard Rime proof Float withdrawal",
        lambda: not floating_surface_presenting(
            call(socket, {"op": "foreign_frames"}), "so.tilo.chat",
            keyboard_frame["floating_id"],
        ),
        process,
    )
    final_access = wait_for(
        "keyboard-returned Rime proof editor",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    if not private_editor_value_matches(final_access, ""):
        raise RuntimeError("keyboard Return changed the editor after Rime undo")
    (output / "native-ime-float-return.json").write_text(json.dumps({
        "owned_fcitx_processes": 1,
        "same_fcitx_process": same_fcitx_process(fcitx_identity),
        "attached_preedit_frame": True,
        "detached_preedit_frame": True,
        "returned_preedit_frame": True,
        "committed_value_unchanged_before_float": True,
        "committed_value_unchanged_after_float": True,
        "committed_value_unchanged_after_return": True,
        "exact_rime_commit": True,
        "floating_rime_commit": True,
        "floating_rime_selection": True,
        "floating_rime_commit_undo": True,
        "returned_rime_commit": True,
        "returned_primary_keyboard_focused_after_commit": True,
        "returned_child_preedit_continuation": returned_input["child_preedit_continuation"],
        "ime_commit_undo": True,
        "pre_detach_undo": True,
        "pointer_float": True,
        "pointer_return": True,
        "keyboard_float": True,
        "keyboard_return": True,
        "detached_child_keyboard_focused": detached_input["child_keyboard_focused"],
        "detached_source_preedit_continuation": detached_input["source_preedit_continuation"],
        "returned_primary_frame_seq": returned["frame_seq"],
        "private_content_recorded": False,
        "test_owned_preedit_rendered": True,
    }, indent=2) + "\n")
    return True


MARKER_OPERATION_REFUSALS = (
    "invalid_configured_address",
    "no_trusted_catalogue",
    "no_exact_resource_action",
)
MARKER_DIAGNOSTIC_COUNT_LIMIT = 64


class MarkerOperationRefusal(RuntimeError):
    """Content-free reason the bounded marker provider refused one request."""

    def __init__(self, reason, *, matching_operations=0, matching_exact_resources=0):
        if reason not in MARKER_OPERATION_REFUSALS:
            raise ValueError("unknown marker operation refusal")
        self.reason = reason
        self.matching_operation_count = min(
            max(matching_operations, 0), MARKER_DIAGNOSTIC_COUNT_LIMIT
        )
        self.matching_exact_resource_count = min(
            max(matching_exact_resources, 0), MARKER_DIAGNOSTIC_COUNT_LIMIT
        )
        super().__init__("marker operation unavailable")


def marker_operation_from_request(request, agent, resource):
    """Select one operation from the Agent's trusted live-catalogue prefix."""
    if not isinstance(agent, str) or not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,127}", agent):
        raise MarkerOperationRefusal("invalid_configured_address")
    if not isinstance(resource, str) or not re.fullmatch(r"[a-zA-Z0-9._:-]{1,4096}", resource):
        raise MarkerOperationRefusal("invalid_configured_address")
    messages = request.get("messages") if isinstance(request, dict) else None
    if not isinstance(messages, list):
        raise MarkerOperationRefusal("no_trusted_catalogue")
    prefix = "Operations published right now:\n\n"
    catalogue = None
    for message in messages:
        if not isinstance(message, dict) or message.get("role") != "user":
            continue
        content = message.get("content")
        if not isinstance(content, str) or not content.startswith(prefix):
            continue
        catalogue = content[len(prefix):]
        boundaries = [position for marker in (
            "\nPeople in this workspace ",
            "\nEarlier in this conversation ",
            "\nThe request:\n",
        ) if (position := catalogue.find(marker)) >= 0]
        if boundaries:
            catalogue = catalogue[:min(boundaries)]
        break
    if catalogue is None:
        raise MarkerOperationRefusal("no_trusted_catalogue")
    operation_pattern = re.compile(
        r"^  (broker:\d+:([a-zA-Z0-9._:-]{1,4096})#rename) — ",
    )
    operations = {
        match.group(1): match.group(2)
        for line in catalogue.splitlines()
        if (match := operation_pattern.match(line)) is not None
    }
    matches = {
        operation for operation, candidate_resource in operations.items()
        if candidate_resource == resource
    }
    if len(matches) != 1:
        raise MarkerOperationRefusal(
            "no_exact_resource_action",
            matching_operations=len(operations),
            matching_exact_resources=len(matches),
        )
    return next(iter(matches))


def marker_preceding_invoke_result(request):
    """Project one exact fixture invoke result; never retain tool/error content."""
    observation = {"result": "missing", "error_class": "unclassified",
                   "private_content_recorded": False}
    messages = request.get("messages") if isinstance(request, dict) else None
    if not isinstance(messages, list) or len(messages) > 256:
        return dict(observation, result="invalid")
    assistants = [index for index, message in enumerate(messages)
                  if isinstance(message, dict) and message.get("role") == "assistant"]
    replies = [(index, message) for index, message in enumerate(messages)
               if isinstance(message, dict) and message.get("role") == "tool"
               and message.get("tool_call_id") == "proof-plan"]
    if not assistants or not replies:
        return observation
    index = assistants[-1]
    calls = messages[index].get("tool_calls")
    if not isinstance(calls, list) or len(calls) != 1 \
            or not isinstance(calls[0], dict) or calls[0].get("id") != "proof-plan" \
            or not isinstance(calls[0].get("function"), dict) \
            or calls[0]["function"].get("name") != "invoke" \
            or len(replies) != 1 or replies[0][0] != index + 1:
        return dict(observation, result="invalid")
    content = replies[0][1].get("content")
    if not isinstance(content, str) or len(content) > 262144:
        return dict(observation, result="invalid")
    try:
        result = json.loads(content)
    except (ValueError, RecursionError):
        return dict(observation, result="invalid")
    if not isinstance(result, dict) or result.get("result") not in (
            "succeeded", "failed", "not run"):
        return dict(observation, result="invalid")
    observation["result"] = {"not run": "not_run"}.get(
        result["result"], result["result"]
    )
    error = result.get("error")
    if result["result"] == "failed" and isinstance(error, str):
        if error == "the action host no longer knows that invocation":
            observation["error_class"] = "invocation_unknown"
        elif error in (
                "the broker did not answer the task action request",
                "Task action did not reach a terminal state before its deadline",
                "Task step did not settle before its deadline; its outcome is still unknown"):
            observation["error_class"] = "uncertain"
        elif error == (
                "private_view: A collaborator cannot move this participant's private focus or "
                "viewport; use Follow or Reveal explicitly"):
            observation["error_class"] = "private_view"
        elif error == "agent is not admitted to this workspace":
            observation["error_class"] = "agent_membership_lost"
        elif error.startswith("capnp error: "):
            # RPC failure is NOT evidence that an effect was definitely refused.
            # NotAvailable wraps capnp::Error Display; RPC decoding adds the
            # remote-exception prefix, while a local Failed error does not.
            prefixes = ("capnp error: Failed: remote exception: ",
                        "capnp error: Failed: ")
            reasons = {
                "the application no longer publishes that action": "target_unpublished",
                "the application has no invocation stream": "invocation_stream_missing",
                "caller no longer authorized for invocation": "permission_revoked",
                "only an agent may invoke a task action": "agent_admission_lost",
                "the task run lease is unknown, expired, or belongs to another agent":
                    "task_run_stale",
                "canonical task run is stale": "task_run_stale",
                "new task direction must be applied before another action":
                    "direction_pending",
                "task cancellation was requested before another action": "task_cancelled",
                "the task was cancelled; no further action may start": "task_cancelled",
                "task action confirmation evidence is not accepted; request human confirmation outside the run":
                    "confirmation_unsupported",
                "durable task steps are not served by this host": "step_unsupported",
                "accepted task action has no broker state": "invocation_state_missing",
                "remote actions are unavailable: the application no longer publishes that action":
                    "target_unpublished",
                "remote actions are unavailable: the application has no invocation stream":
                    "invocation_stream_missing",
                "remote actions are unavailable: caller no longer authorized for invocation":
                    "permission_revoked",
                "the invocation id was already used for a different request":
                    "invocation_conflict",
                "no such invocation": "invocation_unknown",
                "remote actions are unavailable: the invocation is still being admitted; retry":
                    "admission_in_flight",
            }
            known_errors = {prefix + reason: classification
                            for prefix in prefixes
                            for reason, classification in reasons.items()}
            observation["error_class"] = known_errors.get(error, "rpc_error")
    return observation


class HeldProvider:
    """Bounded host-only HTTP fixture behind the unchanged model gateway.

    Request bodies and authorization values are checked but never stored or
    logged. Only a count/event is evidence; no credentials enter state-dump.
    The fixture cannot initiate a request or invoke any desktop action.
    """
    def __init__(self, marker_agent=None, marker_resource=None):
        self.received = threading.Event()
        self.requests = 0
        self.admission_attempts = 0
        self.admitted_attempts = 0
        self.refused_admissions = 0
        self.failed_admission_probes = 0
        self.refused_requests = 0
        self.allowed = 0
        self.exact_releases = set()
        self.waiting_ordinals = set()
        self.owner_admissions = {}
        self.owner_admission_probe = None
        self.marker_agent = marker_agent
        self.marker_resource = marker_resource
        self.marker_operations = {}
        self.marker_invoke_result = None
        self.marker_rejections = {reason: 0 for reason in MARKER_OPERATION_REFUSALS}
        self.marker_matching_operations = 0
        self.marker_matching_exact_resources = 0
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
                marker_operation = None
                if owner.marker_agent is not None:
                    try:
                        marker_operation = marker_operation_from_request(
                            request, owner.marker_agent, owner.marker_resource
                        )
                    except MarkerOperationRefusal as error:
                        with owner.request_lock:
                            owner.refused_requests += 1
                            owner.marker_rejections[error.reason] = min(
                                owner.marker_rejections[error.reason] + 1,
                                MARKER_DIAGNOSTIC_COUNT_LIMIT,
                            )
                            owner.marker_matching_operations = min(
                                owner.marker_matching_operations
                                + error.matching_operation_count,
                                MARKER_DIAGNOSTIC_COUNT_LIMIT,
                            )
                            owner.marker_matching_exact_resources = min(
                                owner.marker_matching_exact_resources
                                + error.matching_exact_resource_count,
                                MARKER_DIAGNOSTIC_COUNT_LIMIT,
                            )
                        self.send_error(409)
                        return
                probe = owner.owner_admission_probe
                with owner.request_lock:
                    owner.admission_attempts = min(owner.admission_attempts + 1, 64)
                try:
                    admission = probe() if probe is not None else None
                except Exception:
                    with owner.request_lock:
                        owner.failed_admission_probes = min(owner.failed_admission_probes + 1, 64)
                    raise
                with owner.request_lock:
                    if admission is None:
                        owner.refused_admissions = min(owner.refused_admissions + 1, 64)
                    else:
                        owner.admitted_attempts = min(owner.admitted_attempts + 1, 64)
                with owner.request_lock:
                    limit = 3 if owner.marker_agent is not None else 4
                    if owner.requests >= limit:
                        owner.refused_requests += 1
                        self.send_error(429)
                        return
                    owner.requests += 1
                    ordinal = owner.requests
                    if owner.marker_agent is not None and ordinal == 2:
                        owner.marker_invoke_result = marker_preceding_invoke_result(request)
                    owner.owner_admissions[ordinal] = admission
                    if marker_operation is not None:
                        owner.marker_operations[ordinal] = marker_operation
                    owner.request_lock.notify_all()
                owner.received.set()
                if admission is None:
                    self.send_error(409)
                    return
                with owner.request_lock:
                    owner.waiting_ordinals.add(ordinal)
                    released = owner.request_lock.wait_for(
                        lambda: owner.closed or owner.allowed >= ordinal
                        or ordinal in owner.exact_releases,
                        timeout=120 if ordinal >= 2 else 30,
                    )
                    owner.waiting_ordinals.discard(ordinal)
                if not released or owner.closed:
                    self.send_error(504)
                    return
                if owner.marker_agent is None or ordinal > 1:
                    tool_name = "plan"
                    arguments = {"reply": "Fixture response.", "done": True}
                else:
                    tool_name = "invoke"
                    arguments = {
                        "operation": owner.marker_operations[ordinal],
                        "parameters": {"value": "Agent marker proof"},
                        "parts": [{
                            "part": "Rename the Chat category.",
                            "app": "so.tilo.chat",
                        }],
                    }
                reply = {"id": "proof", "model": "bounded-fixture", "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": None,
                    "tool_calls": [{"id": "proof-plan", "type": "function", "function": {
                        "name": tool_name, "arguments": json.dumps(arguments)}}]},
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

    def admission_diagnostic(self):
        """Finite pre-probe and outcome counts, never model bodies or owner IDs."""
        with self.request_lock:
            settled = self.admitted_attempts + self.refused_admissions + self.failed_admission_probes
            return {
                "admission_attempt_count": self.admission_attempts,
                "admitted_count": self.admitted_attempts,
                "refused_count": self.refused_admissions,
                "probe_failure_count": self.failed_admission_probes,
                "pending_probe_count": max(0, self.admission_attempts - settled),
                "counted_request_count": self.requests,
                "waiting_response_count": len(self.waiting_ordinals),
            }

    def overflow_count(self):
        with self.request_lock:
            return self.refused_requests

    def marker_rejection_diagnostic(self):
        """Return fixed counters only; request and exception content never escape."""
        with self.request_lock:
            counts = dict(self.marker_rejections)
            matching_operations = self.marker_matching_operations
            matching_exact_resources = self.marker_matching_exact_resources
        return {
            "rejection_count": min(
                sum(counts.values()), MARKER_DIAGNOSTIC_COUNT_LIMIT
            ),
            "reasons": counts,
            "matching_operation_count": matching_operations,
            "matching_exact_resource_count": matching_exact_resources,
            "private_content_recorded": False,
        }

    def preceding_marker_result(self):
        """Only the immutable closed observation, never the provider request."""
        with self.request_lock:
            return dict(self.marker_invoke_result) if self.marker_invoke_result else {
                "result": "missing", "error_class": "unclassified",
                "private_content_recorded": False,
            }

    def original_third_held(self):
        with self.request_lock:
            return self.allowed == 2 and 3 in self.waiting_ordinals

    def marker_second_held(self):
        """The first invoke continued, but its next model turn remains unreleased."""
        with self.request_lock:
            return self.marker_agent is not None and self.requests == 2 \
                and self.allowed == 1 and 2 in self.waiting_ordinals

    def marker_second_superseded(self):
        """A third turn began while the prior HTTP response remained unreleased."""
        with self.request_lock:
            return self.marker_agent is not None and self.requests == 3 \
                and self.allowed == 1 and {2, 3}.issubset(self.waiting_ordinals)

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

    def release_marker_terminal(self):
        """Finish turn three without answering the superseded held second turn."""
        with self.request_lock:
            if self.marker_agent is None or self.requests != 3 or self.allowed != 1 \
                    or 2 not in self.waiting_ordinals or 3 not in self.waiting_ordinals:
                raise RuntimeError(
                    "marker terminal requires the superseded second and current third turns"
                )
            self.exact_releases.add(3)
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


def retain_marker_rejection_diagnostic(provider, output):
    """Persist only a closed provider refusal when the marker stage aborts."""
    diagnostic = provider.marker_rejection_diagnostic()
    if diagnostic["rejection_count"] == 0:
        return False
    (output / "marker-provider-rejection.json").write_text(
        json.dumps(diagnostic, indent=2) + "\n"
    )
    return True


class ProofTimeout(RuntimeError):
    """A bounded observation timed out, without claiming effect failure."""


def wait_for(label, predicate, process=None, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"{label}: process exited with {process.returncode}")
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise ProofTimeout(f"timed out waiting for {label}")


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


class ActionInvocationRefusal(RuntimeError):
    """Retain only a fixed public transport class for targeted diagnostics."""
    def __init__(self, action, code):
        self.closed_code = code if isinstance(code, str) and code in {
            "not_permitted", "not_found", "unknown_action", "unknown_provider",
            "invalid_arguments", "invalid_request", "action_failed", "stale_revision",
            "unavailable", "timeout", "internal_error",
        } else "other"
        super().__init__(f"ordinary action {action} refused: {self.closed_code}")


def invoke(socket, action, args=None):
    response = call(socket, {"op": "actions_invoke", "provider": action["provider"],
                             "id": action["id"], "args": args or {}})
    if "err" in response:
        raise ActionInvocationRefusal(action["id"], response["err"].get("code"))
    if not isinstance(response.get("answer"), dict):
        raise RuntimeError("ordinary semantic action acknowledgement is absent")
    return response["answer"]


def terminal_status_refusal_detail(error):
    """Preserve the broker's deliberately collapsed visibility refusal, not its text."""
    known = "invocation is unavailable or not visible; this does not establish the original action's outcome"
    return "job_unavailable_or_not_visible" if isinstance(error, dict) \
        and error.get("code") == "action_failed" and error.get("message") == known else "other"


def direction_refusal_reason(error):
    """Classify exact public task-door refusals without retaining their message."""
    message = error.get("message") if isinstance(error, dict) else None
    if not isinstance(message, str):
        return "other"
    return {
        "task run or instruction frontier changed; review before steering": "frontier_changed",
        "workspace task certificate is not current admitted evidence": "certificate_not_current",
        "workspace task agent is not a current room member": "agent_not_current_member",
        "workspace task person lacks write authority": "person_lacks_write_authority",
        "task invocation is no longer admitted": "invocation_not_admitted",
        "task invocation has no durable admission": "durable_admission_missing",
        "task invocation binding changed": "invocation_binding_changed",
        "task original custody unavailable": "original_custody_unavailable",
        "task owner cannot observe its committed result": "committed_result_not_observable",
        "task owner lost its exact committed result": "exact_committed_result_missing",
        "task owner returned an unbound room projection": "owner_projection_unbound",
        "only a person may direct an active task": "person_required",
        "only an active task accepts a direction": "task_not_active",
        "active task has no execution control": "execution_control_missing",
        "task has no execution control": "execution_control_missing",
        "active task has no live run epoch": "live_run_missing",
        "task directions exceed the run instruction bound": "instruction_bound_exceeded",
        "task direction revision exhausted": "direction_revision_exhausted",
        "task direction byte count overflow": "direction_byte_count_overflow",
        "task direction character count overflow": "direction_character_count_overflow",
    }.get(message, "other")


def wait_terminal(socket, acknowledgement, process, *, public_failure_path=None,
                  diagnostic_path=None, closed_failure_path=None):
    """Observe the original invocation; admission is not completed work."""
    receipt = acknowledgement.get("receipt")
    if not isinstance(receipt, dict):
        raise RuntimeError("ordinary action receipt is absent")
    original = receipt.get("update", {}).get("invocation")
    if not original:
        raise RuntimeError("ordinary action invocation identity is absent")
    states = {}
    broker_identity = None
    unchanged = True
    status_refusal = None
    status_refusal_detail = None
    terminal_state = None
    terminal_error_code = None
    refusal_reason = None

    def completed():
        nonlocal receipt, broker_identity, unchanged, status_refusal, status_refusal_detail
        nonlocal terminal_state, terminal_error_code
        nonlocal refusal_reason
        update = receipt.get("update")
        if not isinstance(update, dict) or update.get("invocation") != original:
            unchanged = False
            raise RuntimeError("ordinary action observation changed its invocation")
        state = update.get("state")
        if state in {"accepted", "progress", "item", "succeeded", "failed", "cancelled"}:
            states[state] = min(states.get(state, 0) + 1, 10000)
        if state == "succeeded":
            return receipt
        if state in {"failed", "cancelled"}:
            terminal_state = state
            error = update.get("error")
            code = error.get("code") if isinstance(error, dict) else None
            # Actual DoorError wire codes and the routing exceptions retained
            # by door_error_code; arbitrary application strings stay private.
            terminal_error_code = code if isinstance(code, str) and code in {
                "unknown_action", "invalid_arguments", "action_unavailable",
                "action_confirmation_required", "action_failed",
                "cross_record_reentry", "ambiguous_action_target",
                "bound_broker_route_unavailable", "undeclared_document_mutation",
                "workspace_task_refused",
            } else "other"
            refusal_reason = direction_refusal_reason(error)
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
        broker_identity = broker_id if broker_id <= 2**64 - 1 else None
        response = call(socket, {"op": "actions_status", "invocation_id": broker_id})
        if "err" in response:
            error = response.get("err")
            code = error.get("code") if isinstance(error, dict) else None
            status_refusal = code if code in {
                "unknown_action", "invalid_arguments", "action_failed"
            } else "other"
            status_refusal_detail = terminal_status_refusal_detail(error)
            raise RuntimeError(
                "ordinary action terminal observation was refused "
                f"(status_refusal={status_refusal}, status_refusal_detail={status_refusal_detail})"
            )
        receipt = response.get("answer", {}).get("receipt")
        if not isinstance(receipt, dict):
            raise RuntimeError("ordinary action terminal receipt is absent")
        return None

    try:
        return wait_for("original ordinary action terminal success", completed, process)
    except (ProofTimeout, RuntimeError) as error:
        if closed_failure_path is not None:
            try:
                closed_failure_path.write_text(json.dumps({
                    "terminal_state": terminal_state,
                    "terminal_error_code": terminal_error_code,
                    "refusal_reason": refusal_reason,
                    "original_invocation_matches": unchanged,
                    "states_seen": dict(sorted(states.items())),
                }, indent=2) + "\n")
            except OSError:
                error.add_note("closed action failure diagnostic could not be written")
        if diagnostic_path is not None:
            # Whitelist only inert correlation and closed lifecycle counters.
            # Never serialize a receipt, result, error, prompt or action text.
            safe_original = original if isinstance(original, str) and re.fullmatch(
                r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", original
            ) else None
            try:
                diagnostic = {
                    "original_invocation": safe_original,
                    "broker_invocation_id": broker_identity,
                    "states_seen": dict(sorted(states.items())),
                    "original_unchanged": unchanged,
                }
                if status_refusal is not None:
                    diagnostic["status_refusal"] = status_refusal
                    diagnostic["status_refusal_detail"] = status_refusal_detail
                diagnostic_path.write_text(json.dumps(diagnostic, indent=2) + "\n")
            except OSError:
                error.add_note("closed terminal diagnostic could not be written")
        raise


def wait_for_direction_terminal(socket, action, arguments, process, output):
    return wait_terminal(socket, invoke(socket, action, arguments), process,
                         closed_failure_path=output / "managed-direction-terminal-failure.json")


def chrome_state(socket):
    response = call(socket, {"op": "state_dump"})
    chrome = response.get("state", {}).get("chrome")
    if not isinstance(chrome, dict):
        raise RuntimeError("production chrome state observation is absent")
    return chrome


def production_rime_default():
    """Read the bounded, repo-owned Rime default installed by the OS image."""
    source = REPO / "os/usr/share/rime-data/default.custom.yaml"
    try:
        with source.open("rb") as stream:
            raw = stream.read(4097)
        text = raw.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise RuntimeError("the production Rime default is unreadable") from error
    if len(raw) > 4096 or re.findall(
            r"(?m)^\s*-\s+schema:\s*([^\s#]+)\s*(?:#.*)?$", text
    ) != ["luna_pinyin_simp"]:
        raise RuntimeError("the production Rime default is not the known single schema")
    return raw


def configure_rime_input(profile):
    """Merge the production Rime layout into this disposable session profile."""
    path = profile / "config/tilo/settings.json"
    if path.exists():
        settings = json.loads(path.read_text())
        if not isinstance(settings, dict):
            raise RuntimeError("scratch settings are malformed")
    else:
        settings = {"version": 1}
    current = settings.get("input")
    if current is None:
        current = {}
    if not isinstance(current, dict):
        raise RuntimeError("scratch input settings are malformed")
    settings["input"] = {
        **current,
        "keyboard_layout": "ime/rime",
        "keyboard_layouts": ["us", "ime/rime"],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(settings, indent=2) + "\n")
    # The image installs this exact patch after the RPM transaction. Mirror it
    # into Rime's standard user-data layer before the owned Fcitx process
    # starts, so a manually provisioned proof container has the same sole
    # schema without mutating its host or using a controller mode override.
    rime_default = profile / "data/fcitx5/rime/default.custom.yaml"
    rime_default.parent.mkdir(parents=True, exist_ok=True)
    rime_default.write_bytes(production_rime_default())


def chat_text_input_active(socket):
    """Require the seat's keyboard and active text input on actual Chat."""
    seat = chrome_state(socket).get("seat_focus")
    if not isinstance(seat, dict) or type(seat.get("text_input_active")) is not bool:
        raise RuntimeError("the compositor seat text-input state is unobservable")
    keyboard = seat.get("keyboard")
    text_input = seat.get("text_input")
    if not isinstance(keyboard, dict) or not isinstance(text_input, dict):
        return False
    return bool(
        seat["text_input_active"]
        and keyboard.get("app_id") == "so.tilo.chat"
        and text_input.get("app_id") == "so.tilo.chat"
    )


def chat_input_method_ready(socket):
    """Require Chat's field plus the compositor's live input method and grab."""
    seat = chrome_state(socket).get("seat_focus")
    required = ("text_input_active", "input_method_connected", "input_method_keyboard_grabbed")
    if not isinstance(seat, dict) or any(type(seat.get(key)) is not bool for key in required):
        raise RuntimeError("the compositor input-method readiness is unobservable")
    keyboard = seat.get("keyboard")
    text_input = seat.get("text_input")
    return bool(
        isinstance(keyboard, dict)
        and isinstance(text_input, dict)
        and keyboard.get("app_id") == "so.tilo.chat"
        and text_input.get("app_id") == "so.tilo.chat"
        and all(seat[key] for key in required)
    )


def chat_input_method_preedit_active(socket):
    """Require an accepted compositor preedit on Chat's active text input."""
    seat = chrome_state(socket).get("seat_focus")
    if not isinstance(seat, dict) \
            or type(seat.get("input_method_preedit_active")) is not bool:
        raise RuntimeError("the compositor preedit state is unobservable")
    keyboard = seat.get("keyboard")
    text_input = seat.get("text_input")
    return bool(
        seat["input_method_preedit_active"]
        and seat.get("text_input_active") is True
        and isinstance(keyboard, dict)
        and isinstance(text_input, dict)
        and keyboard.get("app_id") == "so.tilo.chat"
        and text_input.get("app_id") == "so.tilo.chat"
    )


def chat_input_method_observation(socket):
    """Return only closed booleans from the compositor's authoritative seat."""
    seat = chrome_state(socket).get("seat_focus")
    required = (
        "text_input_active", "input_method_connected",
        "input_method_keyboard_grabbed", "input_method_preedit_active",
    )
    if not isinstance(seat, dict) or any(type(seat.get(key)) is not bool for key in required):
        raise RuntimeError("the compositor input-method state is unobservable")
    keyboard = seat.get("keyboard")
    text_input = seat.get("text_input")
    return {
        "keyboard_is_chat": isinstance(keyboard, dict)
            and keyboard.get("app_id") == "so.tilo.chat",
        "text_input_is_chat": isinstance(text_input, dict)
            and text_input.get("app_id") == "so.tilo.chat",
        "text_input_active": seat["text_input_active"],
        "input_method_connected": seat["input_method_connected"],
        "input_method_keyboard_grabbed": seat["input_method_keyboard_grabbed"],
        # This is accepted only after the compositor's client and serial checks;
        # it does not expose either identifier or claim why acceptance failed.
        "input_method_preedit_accepted": seat["input_method_preedit_active"],
    }


def owned_rime_schema_observation(env):
    """Read bounded configuration from only this disposable profile, never live options."""
    result = dict.fromkeys((
        "rime_user_config_present", "rime_user_config_empty",
        "rime_selected_schema_observed", "rime_selected_schema_is_luna_pinyin_simp",
        "rime_schema_exists", "rime_schema_deployed", "rime_schema_default_keybinder",
        "rime_default_custom_matches_production",
        "rime_data_directory_present", "rime_config_directory_present",
        "rime_deployed_directory_present",
    ), False)
    data = pathlib.Path(env.get("XDG_DATA_HOME", ""))
    config = pathlib.Path(env.get("XDG_CONFIG_HOME", ""))
    if not data.is_absolute() or data.name != "data" or config != data.parent / "config":
        return result
    root = data / "fcitx5/rime"
    result["rime_data_directory_present"] = (
        root.resolve().is_relative_to(data.resolve()) and root.is_dir())
    config_root = config / "fcitx5/rime"
    result["rime_config_directory_present"] = (
        config_root.resolve().is_relative_to(config.resolve()) and config_root.is_dir())
    deployed = root / "build"
    result["rime_deployed_directory_present"] = (
        deployed.resolve().is_relative_to(data.resolve()) and deployed.is_dir())

    def bounded_config(path):
        try:
            if not path.resolve().is_relative_to(data.resolve()):
                return None
            with path.open("rb") as stream:
                raw = stream.read(65537)
            return raw.decode("utf-8") if len(raw) <= 65536 else None
        except (OSError, UnicodeError):
            return None

    user = bounded_config(root / "user.yaml")
    if user is not None:
        result["rime_user_config_present"] = True
        result["rime_user_config_empty"] = not user.strip()
        variables = re.findall(r"(?m)^var:[ \t]*\n((?:[ \t]+[^\n]*\n?)*)", user)
        selected = re.findall(
            r"(?m)^  previously_selected_schema:[ \t]*([^\r\n#]*?)[ \t]*(?:#.*)?$",
            variables[0]) if len(variables) == 1 else []
        if len(selected) == 1 and selected[0].strip():
            result["rime_selected_schema_observed"] = True
            result["rime_selected_schema_is_luna_pinyin_simp"] = selected[0].strip() in (
                "luna_pinyin_simp", '"luna_pinyin_simp"', "'luna_pinyin_simp'",
            )
    owned_default = bounded_config(root / "default.custom.yaml")
    if owned_default is not None:
        try:
            result["rime_default_custom_matches_production"] = (
                owned_default.encode("utf-8") == production_rime_default()
            )
        except RuntimeError:
            pass
    # Fixed Luna deployment is independent of persisted selection. Its presence
    # never establishes the live schema or options; never interpolate YAML values.
    schema = bounded_config(root / "build/luna_pinyin_simp.schema.yaml")
    if schema is None:
        return result
    result["rime_schema_exists"] = True
    result["rime_schema_deployed"] = bool(re.search(r"(?m)^__build_info:\s*$", schema))
    binders = re.findall(r"(?m)^key_binder:[ \t]*\n((?:[ \t]+[^\n]*\n?)*)", schema)
    result["rime_schema_default_keybinder"] = len(binders) == 1 and any(
        re.search(r"accept:\s*[\"']?Control\+Shift\+(?:2|at)[\"']?[,\s]", rule)
        and re.search(r"toggle:\s*[\"']?ascii_mode[\"']?[,\s}]", rule)
        for rule in re.split(r"(?m)^[ \t]+-[ \t]+", binders[0])
    )
    return result


def fcitx_rime_observation(env):
    """Observe exact active-state and engine equality without exporting output."""
    command = "/usr/bin/fcitx5-remote"
    try:
        active = subprocess.run(
            [command], env=env, text=True, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, timeout=2,
        )
        # fcitx5-remote exits successfully after printing the state: 0 is
        # closed, 1 inactive and 2 active.  A live Rime engine is not enough;
        # inactive contexts pass physical keys through as ordinary Latin.
        active_known = active.returncode == 0
        state_active = active_known and active.stdout.strip() == "2"
        if not active_known:
            return {
                "fcitx_state_observable": False,
                "fcitx_state_active": False,
                "fcitx_engine_observable": False,
                "fcitx_engine_rime": False,
            }
        current = subprocess.run(
            [command, "-n"], env=env, text=True, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, timeout=2,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {
            "fcitx_state_observable": False,
            "fcitx_state_active": False,
            "fcitx_engine_observable": False,
            "fcitx_engine_rime": False,
        }
    return {
        "fcitx_state_observable": active_known,
        "fcitx_state_active": state_active,
        "fcitx_engine_observable": current.returncode == 0,
        "fcitx_engine_rime": current.returncode == 0 and current.stdout.strip() == "rime",
    }


def fcitx_rime_active(env):
    """Read the isolated composer's active state and exact current engine."""
    observed = fcitx_rime_observation(env)
    return observed["fcitx_state_active"] and observed["fcitx_engine_rime"]


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


def offered_membership_action(socket, room, resource, operation):
    """Select one owner-published membership action without constructing its id."""
    matches = [row for row in actions(socket)
               if row.get("application") == "so.tilo.desktop"
               and row.get("room") == room
               and row.get("record") == "membership"
               and row.get("resource") == resource
               and row.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError("ambiguous native workspace membership action")
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


def offered_governance_action(socket, room, operation):
    """Select the owner-published governance door without constructing its id."""
    matches = [row for row in actions(socket)
               if row.get("application") == "tilo.workspace"
               and row.get("room") == room
               and row.get("resource") == f"workspace.permissions:{room}"
               and row.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError(f"ambiguous workspace governance action {operation}")
    return matches[0] if matches else None


def offered_chat_category_action(socket, room, record, label, operation):
    """Select one current category action while preserving its opaque address."""
    rows = [row for row in actions(socket, label)
            if row.get("application") == "so.tilo.chat"
            and row.get("room") == room
            and row.get("record") == record]
    category_resources = {
        row.get("resource") for row in rows
        if row.get("operation") == "open-category"
        and isinstance(row.get("resource"), str) and row["resource"]
    }
    matches = [row for row in rows
               if row.get("resource") in category_resources
               and row.get("operation") == operation]
    if len(matches) > 1:
        raise RuntimeError(f"ambiguous Chat category action {operation}")
    return matches[0] if matches else None


def grant_exact_marker_action(socket, room, agent, target, process, output):
    """Assign one expiring action rule through the real owner governance surface."""
    if not isinstance(target, dict) or target.get("application") != "so.tilo.chat" \
            or target.get("room") != room or target.get("operation") != "rename" \
            or not isinstance(target.get("resource"), str) or not target["resource"]:
        raise RuntimeError("marker grant requires its exact discovered Chat rename resource")
    profile_name = "Agent marker proof"
    save = wait_for("owner profile.save action", lambda: offered_governance_action(
        socket, room, "profile.save"), process)
    save_receipt = wait_terminal(socket, invoke(socket, save, {
        "name": profile_name,
        "description": "May rename one Chat category for the marker proof",
        "rules": [{
            "scope": {"Action": {
                "app": "so.tilo.chat", "resource": target["resource"], "action": "rename",
            }},
            "effect": "Grant",
            "verb": "Write",
        }],
        "confirm": True,
    }), process)
    assign = wait_for("owner profile.assign action", lambda: offered_governance_action(
        socket, room, "profile.assign"), process)
    assign_receipt = wait_terminal(socket, invoke(socket, assign, {
        "profile": profile_name,
        "subject_kind": "agent",
        "subject": agent,
        "expires_unix": int(time.time()) + 600,
        "confirm": True,
    }), process)
    evidence = {
        "save_invocation": terminal_invocation(save_receipt),
        "assign_invocation": terminal_invocation(assign_receipt),
        "scope": "so.tilo.chat#rename",
        "subject_kind": "agent",
        "expiring": True,
        "broad_chat_grant": False,
        "private_fixture_content_recorded": False,
    }
    (output / "marker-governance.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def assign_expiring_app_write_restriction(
        socket, room, person, app, profile_name, process):
    """Apply one short-lived real owner profile to the exact admitted Person."""
    save = wait_for("owner profile.save action", lambda: offered_governance_action(
        socket, room, "profile.save"), process)
    save_receipt = wait_terminal(socket, invoke(socket, save, {
        "name": profile_name,
        "description": "Native independent app permission proof",
        "rules": [{
            "scope": {"App": app},
            "effect": "Restrict",
            "verb": "Write",
        }],
        "confirm": True,
    }), process)
    assign = wait_for("owner profile.assign action", lambda: offered_governance_action(
        socket, room, "profile.assign"), process)
    expires_unix = int(time.time()) + 12
    assign_receipt = wait_terminal(socket, invoke(socket, assign, {
        "profile": profile_name,
        "subject_kind": "person",
        "subject": person,
        "expires_unix": expires_unix,
        "confirm": True,
    }), process)
    return {
        "save_invocation": terminal_invocation(save_receipt),
        "assign_invocation": terminal_invocation(assign_receipt),
        "expires_unix": expires_unix,
    }


def permission_offer_identity(action):
    fields = ("provider", "id", "application", "room", "record", "resource", "operation")
    if not isinstance(action, dict) or any(not isinstance(action.get(field), str)
                                          for field in fields):
        raise RuntimeError("permission evidence has no exact offered action identity")
    if any(not action[field] for field in fields if field != "record"):
        raise RuntimeError("permission evidence has an empty offered action identity")
    return tuple(action[field] for field in fields)


def permission_offer_matches(action, identity):
    fields = ("provider", "id", "application", "room", "record", "resource", "operation")
    return isinstance(action, dict) and tuple(action.get(field) for field in fields) == identity


def chat_process_identity(access):
    """Kernel identity is compared only in memory, never added to evidence files."""
    pid = access.get("process")
    if not isinstance(pid, int) or isinstance(pid, bool) or process_program(pid) != "chat":
        raise RuntimeError("permission evidence has no live Chat process")
    try:
        started = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    except (OSError, IndexError):
        raise RuntimeError("permission evidence cannot verify Chat process continuity") from None
    return pid, started


class HiddenOfferObservation(collections.namedtuple(
        "HiddenOfferObservationFields", "original observed_at")):
    """Immutable in-memory evidence; not a permission or execution capability."""
    __slots__ = ()


def observe_hidden_permission_offer(socket, original, opposite, read_operation,
                                    initial_access, current_access, initial_identity):
    """Qualify hidden-address refusal using a current catalogue and mounted restriction.

    The caller obtains current_access only after the exact mounted restriction
    passes. This observation is evidence, never an action capability.
    """
    identity = permission_offer_identity(original)
    opposite_identity = permission_offer_identity(opposite)
    if opposite_identity[2] == identity[2] or opposite_identity[3] != identity[3]:
        raise RuntimeError("permission evidence does not compare independent apps in the same room")
    if any(current_access.get(field) != initial_access.get(field)
           for field in ("bus", "process", "window")) \
            or chat_process_identity(current_access) != initial_identity:
        raise RuntimeError("permission evidence changed its mounted Chat process or view")
    try:
        catalogue = actions(socket)
    except (RuntimeError, KeyError, TypeError):
        raise RuntimeError("permission evidence catalogue is unavailable") from None
    if not isinstance(catalogue, list) or any(not isinstance(row, dict) for row in catalogue):
        raise RuntimeError("permission evidence catalogue has no valid public action rows")
    if any(permission_offer_matches(row, identity) for row in catalogue):
        raise RuntimeError("the permission-restricted offer is still visible")
    if not any(permission_offer_matches(row, opposite_identity) for row in catalogue):
        raise RuntimeError("the independent opposite-app offer is no longer visible")
    read = [row for row in catalogue if row.get("operation") == read_operation
            and all(row.get(field) == original[field]
                    for field in ("provider", "application", "room", "record", "resource"))]
    if len(read) != 1:
        raise RuntimeError("the same live resource has no unique current Read control")
    return HiddenOfferObservation(identity, time.monotonic())


def require_stale_action_refused(socket, action, args, process, *, observation=None):
    """Require a saved public offer to refuse, without treating uncertainty as refusal."""
    if not isinstance(observation, HiddenOfferObservation) \
            or observation.original != permission_offer_identity(action) \
            or not 0 <= time.monotonic() - observation.observed_at <= 2:
        raise RuntimeError("stale action has no fresh qualified hidden-offer evidence")
    response = call(socket, {
        "op": "actions_invoke", "provider": action["provider"],
        "id": action["id"], "args": args,
    })
    if not isinstance(response, dict) or ("err" in response and "answer" in response):
        raise RuntimeError("stale action refusal has no unambiguous protocol response")
    if "err" in response:
        error = response["err"]
        if isinstance(error, dict) and error.get("code") == "unknown_action" \
                and isinstance(observation, HiddenOfferObservation) \
                and observation.original == permission_offer_identity(action) \
                and 0 <= time.monotonic() - observation.observed_at <= 2:
            # Production intentionally makes hidden and nonexistent addresses
            # indistinguishable. Only qualified current evidence names this a
            # hidden-offer refusal; it is not a permission_denied wire code.
            return "hidden_action_refused"
        raise RuntimeError("stale action invocation has no qualified definite refusal")
    answer = response.get("answer")
    receipt = answer.get("receipt") if isinstance(answer, dict) else None
    if not isinstance(receipt, dict):
        raise RuntimeError("stale action refusal has no observable receipt")
    # A current hidden address must refuse before admission. An admitted job's
    # Failed/Cancelled result says nothing about permission denial, and status
    # transport uncertainty cannot turn it into qualified refusal evidence.
    raise RuntimeError("stale hidden action was admitted rather than refused")


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


def reopen_marker_composer_while_live(socket, agent, marker, windows, process, *, output=None):
    """Prove one private draft round trip while the real marker is actionable."""
    draft = "Keep this marker Message draft private."
    observations = {"probe_count": 0, "initial": None, "last": None}

    def composer():
        observed = {}
        try:
            return agent_quick_composer_accessibility(agent, windows=windows, observation=observed)
        finally:
            observations["probe_count"] += 1
            if observations["initial"] is None:
                observations["initial"] = json.loads(json.dumps(observed))
            observations["last"] = observed
            if output is not None:
                (output / "agent-quick-composer-observation.json").write_text(
                    json.dumps(observations, indent=2) + "\n")
    access = wait_for("initial marker Message composer", composer, process)
    if not private_editor_value_matches(access, ""):
        raise RuntimeError("the initial marker Message draft was not empty")
    require_ok(call(socket, {"op": "click_named", "name": "agent-quick-editor"}))
    require_ok(call(socket, {"op": "type", "text": draft}))
    wait_for("initial marker's physical Message draft", lambda:
             private_editor_value_matches(access, draft), process)
    require_ok(call(socket, {"op": "click_named", "name": "agent-quick-close"}))
    wait_for("initial quick composer close", lambda: composer() is None, process)
    # This is the freshly observed real node. Its public activation rejects
    # an expired pulse; caching the address cannot extend its authority.
    activate_shell_accessibility_control(marker, marker["label"])
    reopened = wait_for("initial marker composer reopened", composer, process)
    if not private_editor_value_matches(reopened, draft):
        raise RuntimeError("Message draft was lost across the initial marker close and reopen")
    return draft


def quick_composer_targets(response):
    """Decode the real unwrapped Targets response without exposing its payload."""
    if not isinstance(response, dict) or set(response) != {"targets"} \
            or not isinstance(response["targets"], list) \
            or len(response["targets"]) > atspi.APP_TREE_NODE_LIMIT:
        raise RuntimeError("the compositor quick composer target response is malformed")
    result = {}
    for name in ("agent-quick-editor", "agent-quick-send", "agent-quick-close"):
        rows = [row for row in response["targets"]
                if isinstance(row, dict) and row.get("name") == name]
        if len(rows) != 1:
            raise RuntimeError("the compositor did not publish unique quick composer controls")
        rect = rows[0].get("rect")
        if not isinstance(rect, list) or len(rect) != 4 \
                or any(type(value) not in (int, float) or not math.isfinite(value) for value in rect) \
                or rect[2] <= 0 or rect[3] <= 0:
            raise RuntimeError("the compositor quick composer control geometry is unusable")
        result[name] = rect
    return result


def automatic_single_task_status(access, current, before, agent):
    """Require the owner's one exact current task before trusting automatic Steer."""
    if not isinstance(current, dict) or current.get("complete") is not True \
            or not isinstance(current.get("tasks"), list) or len(current["tasks"]) != 1 \
            or not isinstance(before, dict) or not isinstance(current["tasks"][0], dict):
        raise RuntimeError("automatic Steer lacks one complete exact current owner task")
    task = current["tasks"][0]
    if access.get("task") is not None:
        raise RuntimeError("automatic single-task Steer unexpectedly requires task reconfirmation")
    if any(task.get(key) != before.get(key) for key in (
            "task_id", "run_epoch", "after_revision", "title")) \
            or task.get("status") != "in_progress" \
            or task.get("active_agent") != agent or task.get("assigned_agent") != agent \
            or not isinstance(task.get("title"), str) or not task["title"]:
        raise RuntimeError("automatic Steer does not name the exact current Agent owner frontier")
    return f"Steer current task: {task['title']}"


def quick_composer_steer_ready(access, expected_status, expected_draft):
    """Require the actual bound popup's draft, owner label and interactive Send."""
    return quick_composer_status_at(access, {expected_status}) == expected_status \
        and private_editor_value_matches(access, expected_draft) \
        and accessibility_control_enabled(access, access["controls"]["send"])


def closed_steer_owner_observation(task, before, receipt_revision=None):
    """Project only the last successful owner read, never its identifiers/text."""
    present = isinstance(task, dict)
    revision = task.get("after_revision") if present else None
    applied = task.get("applied_revision") if present else None
    return {
        "task_observable": present,
        "task_active": task.get("status") == "in_progress" if present else None,
        "same_task": task.get("task_id") == before.get("task_id") if present else None,
        "same_run": task.get("run_epoch") == before.get("run_epoch") if present else None,
        "revision_equal": revision == before["after_revision"] if type(revision) is int else None,
        "revision_advanced": revision > before["after_revision"] if type(revision) is int else None,
        "revision_matches_receipt": revision == receipt_revision
        if type(revision) is int and receipt_revision is not None else None,
        "applied_matches_receipt": applied == receipt_revision
        if type(applied) is int and receipt_revision is not None else None,
    }


def wait_steer_owner_observation(label, predicate, process, failure_capture):
    """Capture observation failures without retrying or changing their identity."""
    try:
        return wait_for(label, predicate, process)
    except (RuntimeError, OSError):
        try:
            failure_capture()
        except (RuntimeError, OSError, atspi.GateError):
            pass
        raise


def capture_steer_receipt_failure(socket, agent, last_owner, before, output, size,
                                  stage="receipt"):
    """Failure-only closed observations; never resubmit or retain private labels."""
    expected_owner_fields = set(closed_steer_owner_observation(None, before))
    if stage not in {"receipt", "applied"} or not isinstance(last_owner, dict) \
            or set(last_owner) != expected_owner_fields \
            or any(value is not None and type(value) is not bool for value in last_owner.values()):
        raise RuntimeError("the closed Steer failure observation is malformed")
    observed = {"composer_observable": False, "send_enabled": None,
                "draft_empty": None, "status": "unobservable",
                **last_owner, "failed_stage": stage,
                "owner_observation_is_cached": True,
                "send_geometry_valid": False, "physical_send_count": 1,
                "snapshot_captured": False, "private_content_recorded": False}
    labels = {
        f"Steer current task: {before['title']}": "exact_current_task",
        "Choose which current task to steer": "choose_task",
        "This agent has no current task to steer": "no_current_task",
        "Saving direction to the current task…": "submitting",
        "Sent. Waiting for Task Center to confirm whether the direction was saved…": "outcome_unknown",
        "Earlier direction was saved in Task Center.": "saved",
        "The agent applied earlier direction.": "applied",
        "This action is unavailable. Your draft is still here.": "refused",
    }
    access = None
    try:
        access = agent_quick_composer_accessibility(agent)
    except (RuntimeError, OSError, atspi.GateError):
        pass  # Diagnostic failure cannot replace the original strict gate failure.
    if access is not None:
        observed["composer_observable"] = True
        for key, sample in (
                ("send_enabled", lambda: accessibility_control_enabled(access, access["controls"]["send"])),
                ("draft_empty", lambda: private_editor_value_matches(access, "")),
                ("status", lambda: labels.get(quick_composer_status_at(access, labels), "other"))):
            try:
                observed[key] = sample()
            except (RuntimeError, OSError, atspi.GateError):
                pass
    try:
        quick_composer_targets(call(socket, {"op": "list_targets"}))
        observed["send_geometry_valid"] = True
    except (RuntimeError, OSError):
        pass
    try:
        require_ok(call(socket, {"op": "snapshot", "path": str(
            output / f"agent-steer-receipt-failed-{size}.png")}))
        observed["snapshot_captured"] = True
    except (RuntimeError, OSError):
        pass
    (output / "agent-steer-receipt-failed.json").write_text(json.dumps(observed, indent=2))


def exercise_agent_quick_composer(socket, agent, exact_task, process, *,
                                 reopened_message_draft=None, current_tasks=None,
                                 output=None, size=None):
    """Use the actual shell controls while one genuine Agent run is current."""
    fixture_drafts = {
        "message": "Keep this marker Message draft private.",
        "queue": "Queue this marker follow-up later.",
        "steer": "Keep this exact marker task focused.",
    }

    def composer():
        return agent_quick_composer_accessibility(agent)

    def draft_matches(expected):
        current = composer()
        return current is not None and private_editor_value_matches(current, expected)

    access = wait_for("exact Agent quick composer controls", composer, process)
    if reopened_message_draft != fixture_drafts["message"]:
        raise RuntimeError("the marker's initial Message close and reopen was not proven")
    quick_composer_targets(call(socket, {"op": "list_targets"}))

    for intent in ("message", "queue", "steer"):
        if intent == "message":
            if not private_editor_value_matches(access, reopened_message_draft):
                raise RuntimeError("the reopened marker Message draft changed")
            continue
        if intent != "message":
            activate_agent_quick_composer_control(access, intent)
            access = wait_for(f"{intent} composer controls", composer, process)
        if not private_editor_value_matches(access, ""):
            raise RuntimeError(f"the new {intent} draft was not initially private and empty")
        require_ok(call(socket, {"op": "click_named", "name": "agent-quick-editor"}))
        require_ok(call(socket, {"op": "type", "text": fixture_drafts[intent]}))
        wait_for(
            f"physical {intent} draft handoff",
            lambda intent=intent: draft_matches(fixture_drafts[intent]),
            process,
        )

    activate_agent_quick_composer_control(access, "message")
    access = wait_for("restored Message composer", composer, process)
    wait_for("exact Message draft restoration", lambda: draft_matches(fixture_drafts["message"]),
             process)

    activate_agent_quick_composer_control(access, "steer")
    access = wait_for("reopened Steer composer", composer, process)
    wait_for("Steer draft retained across close and reopen", lambda:
             draft_matches(fixture_drafts["steer"]), process)
    before = exact_task()
    if not isinstance(before, dict) or before.get("status") != "in_progress":
        raise RuntimeError("the genuine marker task is not current before physical Steer")
    run_epoch = before.get("run_epoch")
    before_revision = before.get("after_revision")
    if type(run_epoch) is not int or run_epoch <= 0 \
            or type(before_revision) is not int or before_revision < 0:
        raise RuntimeError("the genuine marker task has no exact owner frontier")
    if current_tasks is None:
        raise RuntimeError("automatic Steer has no observable current owner task list")
    current_task_status = automatic_single_task_status(access, current_tasks(), before, agent)
    def interactive_steer():
        current = composer()
        return current if current is not None and quick_composer_steer_ready(
            current, current_task_status, fixture_drafts["steer"]) else None

    access = wait_for("automatic Steer's exact interactive current-task binding",
                      interactive_steer, process)

    require_ok(call(socket, {"op": "click_named", "name": "agent-quick-send"}))
    last_owner = closed_steer_owner_observation(before, before)

    def capture_failure(stage):
        if output is not None:
            capture_steer_receipt_failure(socket, agent, last_owner, before, output, size,
                                          stage=stage)

    def owner_direction_receipt():
        nonlocal last_owner
        task = exact_task()
        last_owner = closed_steer_owner_observation(task, before)
        if not isinstance(task, dict):
            return None
        if task.get("run_epoch") != run_epoch:
            raise RuntimeError("physical Steer crossed into a replacement Agent run")
        revision = task.get("after_revision")
        return task if type(revision) is int and revision > before_revision else None

    received = wait_steer_owner_observation(
        "physical Steer's exact owner frontier receipt", owner_direction_receipt, process,
        lambda: capture_failure("receipt"))
    receipt_revision = received["after_revision"]
    last_owner = closed_steer_owner_observation(received, before, receipt_revision)

    def owner_direction_applied():
        nonlocal last_owner
        task = exact_task()
        last_owner = closed_steer_owner_observation(task, before, receipt_revision)
        if not isinstance(task, dict):
            return None
        if task.get("run_epoch") != run_epoch:
            raise RuntimeError("physical Steer application crossed into a replacement Agent run")
        after_revision = task.get("after_revision")
        applied_revision = task.get("applied_revision")
        if type(after_revision) is not int or type(applied_revision) is not int:
            raise RuntimeError("physical Steer application lacks an owner frontier")
        if after_revision != receipt_revision:
            raise RuntimeError("physical Steer application no longer names the exact receipt")
        return task if applied_revision == receipt_revision else None

    applied = wait_steer_owner_observation(
        "physical Steer's exact owner DirectionApplied frontier", owner_direction_applied,
        process, lambda: capture_failure("applied"))
    receipt_label = wait_for(
        "physical Steer's localized visible owner outcome",
        lambda: agent_quick_composer_status(agent, {
            "Earlier direction was saved in Task Center.",
            "The agent applied earlier direction.",
        }),
        process,
    )
    wait_for("confirmed Steer clears only its fixture draft", lambda: draft_matches(""),
             process)
    return {
        "physical_input": True,
        "intent_drafts_restored": True,
        "close_reopen_retained_draft": True,
        "task_id": received.get("task_id"),
        "run_epoch": run_epoch,
        "before_revision": before_revision,
        "receipt_revision": receipt_revision,
        "applied_revision": applied["applied_revision"],
        "owner_direction_applied": True,
        "task_choice": "automatic_single_current",
        "visible_outcome": "applied" if receipt_label.startswith("The agent") else "saved",
        "ui_invocation_uuid_observable": False,
        "message_submission_proven": False,
        "queue_submission_proven": False,
        "independent_permission_gates_proven": False,
        "private_fixture_content_recorded": False,
    }


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


def observe_chat_task_frontier(
        socket, output, task_id, run_epoch, phase, process, *, title="Lifecycle task",
        expected_sequence=None):
    """Require Chat's retained frontier, not only Task Center's owner read."""
    if expected_sequence is not None \
            and (type(expected_sequence) is not int or expected_sequence <= 0):
        raise RuntimeError("the expected Chat task sequence is malformed")
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
            if not task_phase_accessibility(access, f"Planning: {title}"):
                return None
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
            sampled = True
        frontier = latest_chat_task_frontier(output, task_id, run_epoch, phase)
        if expected_sequence is not None and (
                frontier is None or type(frontier.get("sequence")) is not int
                or frontier["sequence"] != expected_sequence):
            # Same-phase history can precede the held model continuation.
            # Sample Chat's actual retained state again; the owner's sequence
            # is only an exact-match requirement, never substitute evidence.
            sampled = False
            return None
        return frontier

    try:
        return wait_for("Chat's authenticated current-task frontier", observed, process)
    except RuntimeError as error:
        if str(error).startswith("timed out waiting for"):
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / "managed-task-frontier-timeout.png"
            )}))
        raise


def start_managed_agent(socket, room, agent, process, *, public_failure_path=None):
    """Start through the public owner action, never a fixture-only fallback."""
    published = wait_for(
        "owner-published managed Agent start",
        lambda: offered_desktop_action(
            socket, room, "tasks", "desktop:tasks", "start-agent"
        ),
        process,
    )
    receipt = wait_terminal(
        socket, invoke(socket, published, {"agent": agent}), process,
        public_failure_path=public_failure_path,
    )
    value = terminal_value(receipt)
    if value.get("startup_requested") is not True:
        raise RuntimeError("managed Agent start did not acknowledge only its launch request")
    return {"semantic": True, "invocation": terminal_invocation(receipt)}


def prime_chat_frontier_access(title, process, *, output=None):
    label = f"Planning: {title}"
    owner = None
    evidence = {"probe_count": 0, "first": None, "last": None}

    def observe():
        nonlocal owner
        observed = dict.fromkeys(("route_observed", "same_owner_route", "complete_tree",
                                  "unique_planning_control", "unknown_object",
                                  "unknown_interface", "other_transport_failure"), False)
        evidence["probe_count"] += 1
        try:
            if owner is not None and process_program(owner[0]) != "chat":
                raise RuntimeError("held marker priming lost its exact Chat process")
            access = agent_conversation_accessibility("Tide", stale_only=True)
            if access is None:
                return None
            observed["route_observed"] = True
            identity = (access["process"], access["agent"], access["detached"])
            if owner is None:
                owner = identity
            elif identity != owner:
                raise RuntimeError("held marker priming discovered a foreign Chat route")
            observed["same_owner_route"] = True
            client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
            tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
            observed["complete_tree"] = True
            phases = client.named(tree, label)
            observed["unique_planning_control"] = len(phases) == 1
            if len(phases) != 1:
                raise RuntimeError("the first held marker Planning control is ambiguous or absent")
            return {"access": access, "phase_control": phases[0], "phase_label": label,
                    "address": client.address}
        except atspi.GateError as error:
            observed["unknown_object"] = "org.freedesktop.DBus.Error.UnknownObject" in str(error)
            observed["unknown_interface"] = "org.freedesktop.DBus.Error.UnknownInterface" in str(error)
            observed["other_transport_failure"] = not (
                observed["unknown_object"] or observed["unknown_interface"])
            if observed["unknown_object"]:
                return None
            raise
        finally:
            if evidence["first"] is None:
                evidence["first"] = observed.copy()
            evidence["last"] = observed
            if output is not None:
                (output / "marker-chat-prime-observation.json").write_text(
                    json.dumps(evidence, indent=2) + "\n")

    return wait_for("held marker's exact mounted Chat Planning tree", observe, process)


def sample_primed_chat_frontier(socket, output, task_id, admission, prepared, process):
    """Sample the real retained Chat state before opening the modal composer."""
    def observed():
        access = prepared["access"]
        frame = presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat")
        if frame is None:
            return None
        client = atspi.AtspiClient(prepared["address"], access["bus"])
        try:
            label = client.property(prepared["phase_control"], "Name")
            if len(client.named({prepared["phase_control"]: label}, prepared["phase_label"])) != 1:
                return None
        except atspi.GateError as error:
            if not re.search(
                    r" failed: Error: GDBus\.Error:org\.freedesktop\.DBus\.Error\.UnknownObject:",
                    str(error)):
                raise
            return None
        require_chat_keyboard_focus_after_click(socket, output)
        require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
        frontier = latest_chat_task_frontier(output, task_id, admission["run_epoch"], "planning")
        return frontier if frontier is not None \
            and type(frontier.get("sequence")) is int \
            and frontier["sequence"] == admission["sequence"] else None

    return wait_for("Chat's exact second held marker frontier", observed, process)


def managed_agent_marker_stage(socket, room, record, provider, output, process, size):
    """Invoke one least-privilege Chat action and activate its real marker."""
    agent = "tide"
    target = wait_for("current semantic category marker target", lambda:
                      offered_chat_category_action(
                          socket, room, record, "Lifecycle proof", "rename"
                      ), process)
    ensure_chat_category_visible(socket, "Lifecycle proof", process)
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"category-marker-visible-before-task-{size}.png"
    )}))
    # Preserve the discovered opaque address verbatim. Never reconstruct or
    # decode a mounted component's transport alias from its semantic Person id.
    provider.marker_resource = target["resource"]
    grant = grant_exact_marker_action(socket, room, agent, target, process, output)
    queue = wait_for("owner-published Queue task action", lambda: offered_desktop_action(
        socket, room, "tasks", "desktop:tasks", "queue"), process)
    queued_receipt = wait_terminal(socket, invoke(socket, queue, {
        "instruction": "Rename the Lifecycle proof Chat category to Agent marker proof.",
        "title": "Agent marker proof",
        "agent": agent,
        "route": "model",
    }), process)
    queued_value = terminal_value(queued_receipt)
    queued = queued_value.get("result", {}).get("queued")
    task_id = queued.get("task_id") if isinstance(queued, dict) else None
    if not isinstance(task_id, str) or not task_id or queued_value.get("delivery") != "queued":
        raise RuntimeError("marker proof did not durably queue one owner task")

    def exact_task():
        current = current_agent_tasks(socket, room, agent, process)
        rows = current.get("tasks")
        if current.get("complete") is not True or not isinstance(rows, list):
            raise RuntimeError("marker task owner projection is incomplete")
        matches = [row for row in rows
                   if isinstance(row, dict) and row.get("task_id") == task_id]
        if len(matches) > 1:
            raise RuntimeError("marker task appears more than once in owner projection")
        return matches[0] if matches else None

    def accepted_marker_task():
        task = exact_task()
        execution = task.get("execution") if task else None
        if not isinstance(execution, dict):
            return None
        epoch = task.get("run_epoch")
        phase = execution.get("phase")
        sequence = execution.get("sequence")
        if type(epoch) is not int or epoch <= 0 or execution.get("run_epoch") != epoch:
            return None
        revision = task.get("after_revision")
        applied_revision = task.get("applied_revision")
        if phase == "planning" and type(sequence) is int and sequence > 0 \
                and type(revision) is int and revision >= 0 \
                and type(applied_revision) is int and 0 <= applied_revision <= revision:
            return {"run_epoch": epoch, "phase": phase, "sequence": sequence,
                    "after_revision": revision, "applied_revision": applied_revision}
        return None

    provider.require_owner_admission(accepted_marker_task)
    startup = start_managed_agent(socket, room, agent, process)
    try:
        wait_for("marker task's first controlled model request",
                 lambda: provider.request_count() == 1, process)
    except RuntimeError:
        retain_marker_rejection_diagnostic(provider, output)
        raise
    admission = provider.owner_admission(1)
    if admission is None:
        raise RuntimeError("marker provider request lacked owner task custody")
    first_frontier = observe_chat_task_frontier(
        socket, output, task_id, admission["run_epoch"], "planning", process,
        title="Agent marker proof",
        expected_sequence=admission["sequence"],
    )
    if first_frontier.get("sequence") != admission["sequence"]:
        raise RuntimeError("Chat retained a different first marker task Planning sequence")
    prepared_chat = prime_chat_frontier_access("Agent marker proof", process, output=output)
    # Finish all full-tree and focus work before the real action starts its
    # finite completed-marker dwell. Direct shell children remain live reads.
    category_access = ensure_chat_category_visible(socket, "Lifecycle proof", process)
    shell_windows = wait_for("owned compositor accessibility windows",
                             owned_shell_accessibility_windows, process)
    category_access = {**category_access, "address": shell_windows[0]["address"]}
    marker_released_at = time.monotonic()
    provider.release_through(1)
    wait_for("marker task's normal held continuation",
             provider.marker_second_held, process)
    second_admission = provider.owner_admission(2)
    if second_admission is None:
        raise RuntimeError("marker continuation lacked owner task custody")
    chat_frontier = sample_primed_chat_frontier(
        socket, output, task_id, second_admission, prepared_chat, process,
    )
    if chat_frontier.get("sequence") != second_admission["sequence"]:
        raise RuntimeError("Chat retained a different marker task Planning sequence")

    latest_marker_binding = None
    marker_observations = {"first": None, "last": None, "probe_count": 0,
                           "private_content_recorded": False}

    def bound_marker():
        nonlocal latest_marker_binding
        observed = {"elapsed_since_release_ms": round(
                        (time.monotonic() - marker_released_at) * 1000),
                    "primary_frame_present": False, "category_label_matches": None,
                    "bounds_valid": None, "category_bound": None}
        try:
            marker = agent_marker_accessibility(
                agent, windows=shell_windows, observation=observed)
            category = category_access
            frame = presented_primary_frame(
                call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
            )
            observed["primary_frame_present"] = frame is not None
            if category is None or frame is None:
                return None
            client = atspi.AtspiClient(shell_windows[0]["address"], category["bus"])
            try:
                label = atspi.property_text(client.property(category["control"], "Name"))
            except atspi.GateError:
                observed["category_label_matches"] = False
                return None
            observed["category_label_matches"] = label in {
                "Collapse Agent marker proof", "Expand Agent marker proof"}
            if marker is None or not observed["category_label_matches"]:
                return None
            try:
                binding = marker_binds_presented_category(
                    marker, category, presented_content_geometry(frame)
                )
            except (RuntimeError, atspi.GateError):
                observed["bounds_valid"] = False
                raise
            observed["bounds_valid"] = True
            observed["category_bound"] = binding["category_bound"]
            latest_marker_binding = binding
            return (marker, binding) if binding["category_bound"] else None
        finally:
            if marker_observations["first"] is None:
                marker_observations["first"] = observed
            marker_observations["last"] = observed
            marker_observations["probe_count"] += 1
            (output / "marker-accessibility-observation.json").write_text(
                json.dumps(marker_observations, indent=2) + "\n")

    try:
        marker, marker_binding = wait_for(
            "actual compositor Agent marker at the renamed category", bound_marker, process
        )
    except RuntimeError:
        if latest_marker_binding is not None:
            (output / "marker-category-binding-refusal.json").write_text(
                json.dumps(latest_marker_binding, indent=2) + "\n"
            )
        raise
    activate_shell_accessibility_control(marker, marker["label"])
    reopened_message_draft = reopen_marker_composer_while_live(
        socket, agent, marker, shell_windows, process, output=output,
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"agent-action-marker-activated-{size}.png"
    )}))

    preceding_result = provider.preceding_marker_result()
    (output / "marker-preceding-invoke-result.json").write_text(
        json.dumps(preceding_result, indent=2) + "\n"
    )
    if preceding_result != {
            "result": "succeeded", "error_class": "unclassified",
            "private_content_recorded": False}:
        raise RuntimeError("marker action did not complete successfully")

    def category_renamed():
        renamed = offered_chat_category_action(
            socket, room, record, "Agent marker proof", "rename"
        )
        return renamed if renamed is not None \
            and renamed.get("resource") == target["resource"] else None

    wait_for("same Chat category's current public label", category_renamed, process)
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"agent-marker-second-held-{size}.png"
    )}))

    def steer_ready_composer():
        access = agent_quick_composer_accessibility(agent)
        if access is None:
            return None
        return access if accessibility_control_enabled(
            access, access["controls"]["steer"]
        ) else None

    composer = wait_for(
        "actual quick composer with authenticated Steer availability",
        steer_ready_composer,
        process,
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"agent-quick-composer-after-{size}.png"
    )}))
    interaction = exercise_agent_quick_composer(
        socket, agent, exact_task, process,
        reopened_message_draft=reopened_message_draft,
        current_tasks=lambda: current_agent_tasks(socket, room, agent, process),
        output=output, size=size,
    )

    wait_for("post-Steer normal third model turn",
             provider.marker_second_superseded, process)
    third_admission = provider.owner_admission(3)
    admissions = [admission, second_admission, third_admission]
    if third_admission is None \
            or any(row.get("run_epoch") != admission["run_epoch"] for row in admissions) \
            or [row.get("phase") for row in admissions] != ["planning"] * 3 \
            or not admission["sequence"] < second_admission["sequence"] \
            < third_admission["sequence"] \
            or third_admission["after_revision"] != interaction["receipt_revision"] \
            or third_admission["applied_revision"] != interaction["receipt_revision"] \
            or interaction["applied_revision"] != interaction["receipt_revision"] \
            or not provider.marker_second_superseded():
        raise RuntimeError("marker continuation lost its exact owner run or report ordering")
    provider.release_marker_terminal()

    def completed():
        task = read_task(socket, room, task_id, process)
        status = task.get("status")
        if status in {"failed", "cancelled"}:
            raise RuntimeError(f"marker task reached unexpected terminal status {status}")
        return task if status == "done" else None

    terminal = wait_for("marker task's durable owner completion", completed, process)
    if provider.request_count() != 3 or provider.overflow_count() != 0:
        raise RuntimeError("marker proof did not use exactly three controlled model requests")
    provider.release_through(2)
    evidence = {
        "queue_invocation": terminal_invocation(queued_receipt),
        "start_invocation": startup["invocation"],
        "run_epoch": admission["run_epoch"],
        "owner_phase_at_request": admission["phase"],
        "provider_request_count": 3,
        "held_continuation_cancelled_by_direction": True,
        "owner_planning_sequences": [row["sequence"] for row in admissions],
        "direction_revision": interaction["receipt_revision"],
        "exact_action_scope": grant["scope"],
        "category_renamed": True,
        "marker_category_bound": marker_binding["category_bound"],
        "marker_rect": marker_binding["marker_rect"],
        "category_rect": marker_binding["category_rect"],
        "marker_accessible": True,
        "marker_activated": True,
        "quick_composer_accessible": composer is not None,
        "quick_composer_interaction": interaction,
        "terminal_status": terminal["status"],
        "full_lifecycle_proven": False,
        "private_fixture_content_recorded": False,
    }
    (output / "native-agent-marker.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def agent_reply_observation(
        socket, room, record, parent, agent, process, *,
        expected_reply="Fixture response.", diagnostic_path=None):
    """Read the canonical reply in memory and return content-free evidence."""
    reads = [row for row in actions(socket)
             if row.get("application") == "so.tilo.chat"
             and row.get("room") == room
             and row.get("record") == record
             and row.get("operation") == "read-messages"]
    for read in reads:
        receipt = wait_terminal(
            socket, invoke(socket, read, {"pageSize": 32}), process,
            diagnostic_path=diagnostic_path,
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
            if reply.get("text") != expected_reply:
                raise RuntimeError("managed Agent reply did not match the controlled response")
            return {"typed_actor": reply["author"], "parent": parent,
                    "reply_id": reply["id"], "reply_matches_fixture": True}
    return None


def wait_for_agent_reply(socket, room, record, parent, agent, process, output):
    """Preserve a fresh scene and closed lifecycle evidence before cleanup."""
    try:
        return wait_for("Agent's canonical direct Message reply", lambda: agent_reply_observation(
            socket, room, record, parent, agent, process,
            diagnostic_path=output / "managed-reply-terminal-timeout.json"), process)
    except ProofTimeout as error:
        try:
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / "managed-reply-timeout.png")}))
        except Exception:
            error.add_note("fresh reply timeout snapshot could not be captured")
        raise


def wait_for_direct_message_terminal(socket, receipt, process, output):
    """Preserve closed waiter state and a fresh scene when Message stalls."""
    try:
        return wait_terminal(
            socket,
            receipt,
            process,
            diagnostic_path=output / "managed-message-terminal-timeout.json",
        )
    except ProofTimeout as error:
        try:
            require_ok(call(socket, {"op": "snapshot", "path": str(
                output / "managed-message-terminal-timeout.png")}))
        except Exception:
            error.add_note("fresh Message terminal timeout snapshot could not be captured")
        raise


def canonical_physical_message(socket, room, record, expected, process, *, diagnostic_path=None):
    """Derive the original from an actual saved row; keep fixture text in memory."""
    saved = {}
    for read in actions(socket):
        if read.get("application") != "so.tilo.chat" or read.get("room") != room \
                or read.get("record") != record or read.get("operation") != "read-messages":
            continue
        try:
            receipt = wait_terminal(socket, invoke(socket, read, {"pageSize": 32}), process)
        except ActionInvocationRefusal as error:
            if diagnostic_path is not None:
                diagnostic_path.write_text(json.dumps({
                    "refusal_code": error.closed_code,
                    "read_messages_operation": read.get("operation") == "read-messages",
                    "exact_route_matches": read.get("application") == "so.tilo.chat"
                        and read.get("room") == room and read.get("record") == record,
                    "private_content_recorded": False,
                }, indent=2) + "\n")
            raise
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
                            ("tilo-physical-frame ", "state"),
                            ("tilo-physical-poll ", "state")]:
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


def physical_send_activation_revision(output, byte_offset, expected_kind):
    """Use the real Send activation snapshot, not a later raced frame."""
    if expected_kind not in {"pointer_released", "key_pressed"}:
        raise RuntimeError("physical Send activation kind is invalid")
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
        if isinstance(row, dict) and row.get("kind") == expected_kind:
            releases.append(row)
    if len(releases) > 1:
        raise RuntimeError("physical Send activation observation is ambiguous")
    if not releases:
        return None
    row = releases[0]
    before, after = row.get("before"), row.get("after")
    if not isinstance(before, dict) or before.get("app") != "so.tilo.chat" \
            or before.get("agent_message_submission") is not None \
            or not isinstance(after, dict) or after.get("app") != "so.tilo.chat":
        raise RuntimeError("physical Send activation does not bind a new Chat submission")
    value = pending_draft_revision(after)
    return {"revision": value} if value is not None else None


def owned_agent_identities(shell_pid, proc_root=pathlib.Path("/proc")):
    """Observe owned actual processes and kernel starttimes only in memory."""
    rows = {}
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            rows[int(entry.name)] = (int(fields[1]), fields[19])
        except (OSError, IndexError, ValueError):
            continue
    owned = []
    for pid, (_, started) in rows.items():
        if process_program(pid, proc_root) != "tilo-agent":
            continue
        ancestor, seen = pid, set()
        while ancestor in rows and ancestor not in seen and ancestor != shell_pid:
            seen.add(ancestor)
            ancestor = rows[ancestor][0]
        if ancestor == shell_pid:
            owned.append((pid, started))
    return sorted(owned)


def owned_agent_identity(shell_pid):
    """Require one owned actual process and kernel starttime."""
    owned = owned_agent_identities(shell_pid)
    if len(owned) != 1:
        raise RuntimeError("the proof cannot observe one owned managed Agent process")
    return owned[0]


def wait_for_owned_physical_send_control(socket, output, owner, draft, primary, process):
    """Retry only known stale AT transport while observing the exact live Send route."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = dict.fromkeys(("exact_primary_present", "primary_keyboard_focused",
                                "ordinary_native_route", "descriptor_observed", "same_owner_route",
                                "editor_contract_valid", "draft_matches", "complete_tree",
                                "unique_send_control", "send_enabled", "unknown_object",
                                "unknown_interface", "known_stale_transport", "other_transport_failure",
                                "accepted"), False)
        evidence["observation_count"] += 1
        try:
            current = current_same_primary_presentation(
                call(socket, {"op": "foreign_frames"}), primary)
            sample["exact_primary_present"] = current is not None
            if current is None or type(current.get("keyboard_focused")) is not bool:
                raise RuntimeError("physical Send lost its exact observable native primary")
            sample["primary_keyboard_focused"] = current["keyboard_focused"]
            sample["ordinary_native_route"] = current.get("input_route") is None \
                and current.get("input_source_surface_id") is None
            if not sample["primary_keyboard_focused"] or not sample["ordinary_native_route"]:
                raise RuntimeError("physical Send lost its exact ordinary primary seat")
            access = agent_conversation_accessibility(owner["agent"], stale_only=True,
                                                      include_send=True)
            if access is None:
                return None
            sample["descriptor_observed"] = True
            sample["same_owner_route"] = all(access.get(key) == owner[key]
                                             for key in ("process", "agent", "detached")) \
                and access.get("detached") is False
            if not sample["same_owner_route"]:
                raise RuntimeError("physical Send discovered a foreign private editor route")
            sample["editor_contract_valid"] = chat_editor_contract(access) is True
            if not sample["editor_contract_valid"]:
                raise RuntimeError("physical Send's fresh descriptor names another editor")
            observed = _private_editor_observation(access, draft)
            sample["draft_matches"] = observed.get("draft_matches") is True
            if not sample["draft_matches"]:
                raise RuntimeError("physical Send changed the expected private draft")
            sample["complete_tree"] = True
            buttons = access.get("send_controls")
            sample["unique_send_control"] = isinstance(buttons, list) and len(buttons) == 1 \
                and isinstance(buttons[0], str) and bool(buttons[0])
            if not sample["unique_send_control"]:
                raise RuntimeError("the exact direct-agent editor has no unique physical Send control")
            sample["send_enabled"] = accessibility_control_enabled(access, buttons[0]) is True
            if sample["send_enabled"] is not True:
                raise RuntimeError("physical Send's exact control is disabled or unobservable")
            sample["accepted"] = True
            return access, buttons[0], current
        except atspi.GateError as error:
            sample["unknown_object"] = "org.freedesktop.DBus.Error.UnknownObject" in str(error)
            sample["unknown_interface"] = "org.freedesktop.DBus.Error.UnknownInterface" in str(error)
            sample["known_stale_transport"] = stale_editor_transport(error)
            sample["other_transport_failure"] = not sample["known_stale_transport"]
            if sample["known_stale_transport"]:
                return None
            raise
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "physical-send-control-observations.json").write_text(
                json.dumps(evidence, indent=2, allow_nan=False) + "\n")

    return wait_for("physical Send's exact owned enabled control", observe, process)


def wait_for_physical_message_admission(socket, room, record, draft, process, identity, *,
                                      diagnostic_path=None):
    """Observe the saved canonical projection without changing Send or admission."""
    def observe():
        if owned_agent_identity(process.pid) != identity:
            raise RuntimeError("physical Message admission replaced the managed Agent")
        if diagnostic_path is None:
            return canonical_physical_message(socket, room, record, draft, process)
        return canonical_physical_message(socket, room, record, draft, process,
                                          diagnostic_path=diagnostic_path)

    return wait_for("physical Message's original saved owner projection", observe, process)


def message_phase_accessibility(access, draft, label):
    """Read the phase description of only this uniquely saved request's AT row."""
    client = atspi.AtspiClient(atspi.atspi_address(), access["bus"])
    try:
        tree = accessible_subtree(client, access["window"], atspi.APP_TREE_NODE_LIMIT)
        lists = client.named(tree, "Messages")
        if len(lists) > 1:
            raise RuntimeError("the owned conversation has duplicate Messages lists")
        if not lists:
            return False
        children = client.call(lists[0], "org.a11y.atspi.Accessible.GetChildren")
        matches = []
        suffix = ": " + draft
        for path in dict.fromkeys(atspi.PATH_RE.findall(children)):
            name = atspi.property_text(client.property(path, "Name"))
            if name is None:
                raise RuntimeError("the saved request accessibility name is unobservable")
            if name.endswith(suffix) and len(name) > len(suffix):
                matches.append(path)
        if len(matches) > 1:
            raise RuntimeError("the owned conversation has duplicate saved request rows")
        if not matches:
            return False
        description = atspi.property_text(client.property(matches[0], "Description"))
        if description is None:
            raise RuntimeError("the saved request accessibility description is unobservable")
        return description == label
    except atspi.GateError as error:
        if not re.search(
                r" failed: Error: GDBus\.Error:org\.freedesktop\.DBus\.Error\.UnknownObject:",
                str(error)):
            raise
        return False


def wait_for_current_message_phase_accessibility(owner, draft, label, process):
    """Observe current public phase on the same owned, attached Chat route."""
    def observe():
        current = agent_conversation_accessibility(owner["agent"])
        if current is None:
            return None
        if any(current.get(field) != owner.get(field)
               for field in ("process", "bus", "agent", "detached")) \
                or current.get("detached") is not False:
            raise RuntimeError("physical Message phase changed the owned Chat route")
        return current if message_phase_accessibility(current, draft, label) else None

    return wait_for("mounted original Message's current public phase", observe, process)


def owned_chat_message_progress(owner, chat_identity, process, agent_identity, output):
    """Read mounted telemetry without requiring input, paint, or an AT rebuild."""
    if owned_agent_identity(process.pid) != agent_identity \
            or chat_process_identity(owner) != chat_identity:
        raise RuntimeError("mounted Message progress replaced its actual owner")
    return latest_chat_input_state(output)


def physical_send_after_stop(socket, room, record, provider, output, size, process, identity):
    """Supplement, never replace, the semantic lifecycle and physical editor journey."""
    if provider.request_count() != 3 or not provider.original_third_held() \
            or owned_agent_identity(process.pid) != identity:
        raise RuntimeError("physical Send requires the same live Agent and original held third turn")
    draft = "Reply after Stop using this physical Send."

    def admission():
        # The authenticated model POST is the execution boundary. UI polling
        # may legitimately lag it; require mounted progress before release,
        # not as new authority to admit model execution.
        return wait_for_physical_message_admission(
            socket, room, record, draft, process, identity,
            diagnostic_path=output / "physical-send-admission-read-refusal.json")

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
    progress_chat_identity = chat_process_identity(access)
    access, send_control, frame = wait_for_owned_physical_send_control(
        socket, output, access, draft, frame, process,
    )
    focus_agent_conversation_control(access, send_control, "Send message")
    require_ok(call(socket, {"op": "snapshot", "path": str(output / f"before-physical-send-{size}.png")}))
    log = output / "shell.log"
    if not log.is_file() or chat_input_observer_enabled(output) is not True:
        raise RuntimeError("physical Send's actual Chat input observer is unavailable")
    send_log_offset = log.stat().st_size
    require_ok(call(socket, {"op": "key", "key": "Enter"}))
    revision = wait_for("actual keyboard Send private draft revision", lambda:
                        physical_send_activation_revision(
                            output, send_log_offset, "key_pressed"), process)["revision"]

    def sample():
        return owned_chat_message_progress(access, progress_chat_identity, process, identity, output)

    original = wait_for("physical Send's actual canonical request",
                        lambda: canonical_physical_message(socket, room, record, draft, process,
                            diagnostic_path=output / "physical-send-canonical-read-refusal.json"), process)
    wait_for("new authenticated model request after Stop", lambda: provider.request_count() == 4, process)
    admitted = provider.owner_admission(4)
    (output / "physical-send-admission-observation.json").write_text(json.dumps({
        "admitted_present": admitted is not None,
        "admission_matches_original": admitted == original,
        "original_third_held": provider.original_third_held(),
        "provider": provider.admission_diagnostic(),
        "private_content_recorded": False,
    }, indent=2) + "\n")
    if admitted != original or not provider.original_third_held():
        raise RuntimeError("fourth model request did not retain this original while the third stayed held")
    wait_for("exact owner receipt clears the captured private draft",
             lambda: private_editor_value_matches(access, ""), process)
    waiting = wait_for("mounted Chat's original WaitingForReply",
                       lambda: chat_public_message_progress(sample(), original, "waiting_for_reply"), process)
    access = wait_for_current_message_phase_accessibility(
        access, draft, "Waiting for reply", process)
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
    evidence = {"physical_send": True, "keyboard_send": True,
                "same_managed_process": True,
                "original_invocation": original["invocation"], "request_row": original["request_row"],
                "captured_draft_revision": revision, "exact_draft_cleared": True,
                "reply_actor": reply["typed_actor"], "reply_parent": reply["parent"],
                "reply_row": reply["reply_id"],
                "reply_matches_fixture": reply["reply_matches_fixture"],
                "waiting": waiting, "committed": committed,
                "old_task_response_still_held_until_reply": True, "provider_request_count": 4,
                "private_content_recorded": False, "test_owned_draft_rendered": True}
    (output / "physical-send-after-stop.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


def wait_for_restored_agent_offers(socket, room, record, provider, process, expected, output):
    """Observe both real offers while preserving exact process and private state."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = {"same_managed_process": False, "provider_counts_unchanged": False,
                  "same_chat_route": False, "editor_state_matches": False,
                  "message_offer_present": False, "queue_offer_present": False,
                  "message_offer_count": 0, "queue_offer_count": 0}
        evidence["observation_count"] += 1
        try:
            sample["same_managed_process"] = owned_agent_identity(process.pid) == expected["identity"]
            sample["provider_counts_unchanged"] = provider.request_count() == 4 \
                and provider.overflow_count() == 0
            if not sample["same_managed_process"] or not sample["provider_counts_unchanged"]:
                raise RuntimeError("restored offers changed process custody or initiated model work")
            current = agent_conversation_accessibility(expected["access"]["agent"])
            if current is None:
                return None
            sample["same_chat_route"] = all(current.get(field) == expected["access"].get(field)
                for field in ("process", "bus", "agent", "detached")) \
                and current.get("detached") is False
            if not sample["same_chat_route"]:
                raise RuntimeError("restored offers changed the exact attached Chat owner")
            observed = private_editor_observation(current, expected["draft"])
            sample["editor_state_matches"] = all(observed[field] == expected["observation"][field]
                for field in ("draft_matches", "caret", "selection"))
            if not sample["editor_state_matches"]:
                raise RuntimeError("restored offers changed private editor content, caret or selection")
            rows = actions(socket)
            messages = [row for row in rows if row.get("application") == "so.tilo.chat"
                and row.get("room") == room and row.get("record") == record
                and row.get("semantic_identity") == {"kind": "chat.agent", "key": "chat.person:tide"}
                and row.get("operation") == "message"]
            queues = [row for row in rows if row.get("application") == "so.tilo.desktop"
                and row.get("room") == room and row.get("record") == "tasks"
                and row.get("resource") == "desktop:tasks" and row.get("operation") == "queue"]
            sample["message_offer_count"] = min(len(messages), 64)
            sample["queue_offer_count"] = min(len(queues), 64)
            if len(messages) > 1 or len(queues) > 1:
                raise RuntimeError("restored offers have ambiguous exact Message or Queue actions")
            sample["message_offer_present"] = len(messages) == 1
            sample["queue_offer_present"] = len(queues) == 1
            return current if messages and queues else None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "restored-agent-offer-observations.json").write_text(
                json.dumps(evidence, indent=2) + "\n")

    return wait_for("re-admission's actual current Message and Queue offers", observe, process)


def wait_for_restored_detached_editor(socket, expected, child, provider, process, output):
    """Wait for mounted controls, never repair input or replace retained state."""
    evidence = {"observation_count": 0, "initial": None, "last": None}

    def observe():
        sample = {"same_managed_process": False, "provider_counts_unchanged": False,
                  "exact_child_presented": False, "ordinary_child_route": False,
                  "accessibility_observable": False, "same_chat_route": False,
                  "private_state_matches": False, "send_control_count": 0,
                  "queue_control_count": 0, "known_stale_object": False}
        evidence["observation_count"] += 1
        try:
            sample["same_managed_process"] = owned_agent_identity(process.pid) == expected["identity"]
            sample["provider_counts_unchanged"] = provider.request_count() == 4 \
                and provider.overflow_count() == 0
            if not sample["same_managed_process"] or not sample["provider_counts_unchanged"]:
                raise RuntimeError("detached restoration changed process custody or initiated model work")
            rows = [row for row in foreign_frames(call(socket, {"op": "foreign_frames"}))
                    if row.get("app_id") == child["app_id"] and row.get("floating_id") == child["floating_id"]]
            if len(rows) != 1 or rows[0].get("surface_id") != native_surface_id(child):
                raise RuntimeError("detached restoration lost its exact captured native child")
            current_child = rows[0]
            if any(type(current_child.get(field)) is not bool
                   for field in ("presented", "suspended", "transferring")):
                raise RuntimeError("detached restoration native presentation is unobservable")
            sample["exact_child_presented"] = current_child["presented"] \
                and not current_child["suspended"] and not current_child["transferring"]
            sample["ordinary_child_route"] = current_child.get("input_route") is None \
                and current_child.get("input_source_surface_id") is None
            if not sample["exact_child_presented"] or not sample["ordinary_child_route"] \
                    or current_child.get("last_error") is not None:
                raise RuntimeError("detached restoration has no exact presented ordinary child route")
            current = current_owned_conversation_accessibility(expected["access"])
            if current is None:
                return None
            sample["accessibility_observable"] = True
            sample["same_chat_route"] = current.get("detached") is True
            if not sample["same_chat_route"]:
                raise RuntimeError("detached restoration changed its owned Chat route")
            observed = private_editor_observation(current, expected["draft"])
            sample["private_state_matches"] = all(observed[field] == expected["observation"][field]
                for field in ("draft_matches", "caret", "selection"))
            if not sample["private_state_matches"]:
                raise RuntimeError("detached restoration changed private editor content, caret or selection")
            client = atspi.AtspiClient(atspi.atspi_address(), current["bus"])
            names = decoded_accessible_names(accessible_subtree(
                client, current["window"], atspi.APP_TREE_NODE_LIMIT))
            sample["send_control_count"] = min(sum(name == "Send message" for name in names.values()), 64)
            sample["queue_control_count"] = min(sum(name == "Queue task" for name in names.values()), 64)
            if sample["send_control_count"] > 1 or sample["queue_control_count"] > 1:
                raise RuntimeError("detached restoration has duplicate Message or Queue controls")
            return (current, observed, set(names.values())) \
                if sample["send_control_count"] == sample["queue_control_count"] == 1 else None
        except atspi.GateError as error:
            if not re.search(
                    r" failed: Error: GDBus\.Error:org\.freedesktop\.DBus\.Error\.UnknownObject:",
                    str(error)):
                raise
            sample["known_stale_object"] = True
            return None
        finally:
            if evidence["initial"] is None:
                evidence["initial"] = sample.copy()
            evidence["last"] = sample
            (output / "restored-detached-control-observations.json").write_text(
                json.dumps(evidence, indent=2) + "\n")

    return wait_for("restored detached Tide editor", observe, process)


def restored_return_observation(response, child, primary):
    """Observe exact native withdrawal; never infer unseen handoff state."""
    rows = foreign_frames(response)
    children = [row for row in rows if row.get("app_id") == child["app_id"]
                and row.get("floating_id") == child["floating_id"]]
    primaries = [row for row in rows if row.get("app_id") == primary["app_id"]
                and row.get("floating_id") is None
                and row.get("surface_id") == native_surface_id(primary)]
    if len(children) > 1 or len(primaries) != 1 \
            or children and children[0].get("surface_id") != native_surface_id(child):
        raise RuntimeError("restored Return lost its exact native owner route")
    current = children[0] if children else None
    if type(primaries[0].get("presented")) is not bool or current is not None \
            and any(type(current.get(field)) is not bool
                    for field in ("presented", "suspended", "transferring")):
        raise RuntimeError("restored Return native presentation is unobservable")
    ordinary = all(row.get("input_route") is None and row.get("input_source_surface_id") is None
                   for row in primaries + children)
    if not ordinary or any(row.get("last_error") is not None for row in primaries + children):
        raise RuntimeError("restored Return native owner route differs or has an error")
    return {"exact_owner_route": True, "exact_child_exists": current is not None,
            "child_presented": current["presented"] if current is not None else None,
            "child_suspended": current["suspended"] if current is not None else None,
            "child_transferring": current["transferring"] if current is not None else None,
            "child_return_transfer_pending": current["suspended"] and current["transferring"]
                if current is not None else None,
            "primary_presented": primaries[0]["presented"],
            "ordinary_owner_route": ordinary,
            "withdrawn": current is None or current["presented"] is False}


def wait_for_restored_return_withdrawal(socket, output, child, primary, process, initial):
    evidence = {"observation_count": 0, "initial": initial, "last": initial}

    def observe():
        sample = restored_return_observation(call(socket, {"op": "foreign_frames"}), child, primary)
        evidence["observation_count"] += 1
        evidence["last"] = sample
        (output / "revocation-return-withdrawal-observations.json").write_text(
            json.dumps(evidence, indent=2) + "\n")
        return sample if sample["withdrawn"] else None

    return wait_for("revocation proof Float withdrawal", observe, process)


def current_restored_return_access(owner, draft, before):
    current = current_owned_conversation_accessibility(owner)
    if current is None or current.get("detached") is not True:
        raise RuntimeError("restored Return's current owned control is unobservable")
    observed = private_editor_observation(current, draft)
    if any(observed[field] != before[field] for field in ("draft_matches", "caret", "selection")):
        raise RuntimeError("restored Return changed private editor content, caret or selection")
    return current


def native_agent_membership_revocation(
        socket, room, record, provider, output, size, process, old_identity,
        previous_float):
    """Revoke and freshly re-admit Tide through the existing owner actions."""
    if provider.request_count() != 4 or provider.overflow_count() != 0 \
            or owned_agent_identity(process.pid) != old_identity:
        raise RuntimeError("membership revocation requires the completed four-turn lifecycle")
    draft = "Keep this private draft across revocation"
    access = wait_for(
        "attached Tide editor before membership revocation",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    primary = wait_for("Chat frame before membership revocation", lambda: presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), process)
    physically_click_editor(socket, access, presented_content_geometry(primary))
    require_chat_keyboard_focus_after_click(socket, output)
    require_ok(call(socket, {"op": "type", "text": draft}))
    wait_for("private revocation draft", lambda: private_editor_value_matches(access, draft), process)
    with hold_physical_modifiers(socket, shift=True):
        for _ in range(10):
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    before = wait_for_private_selection(
        socket, access, draft, [len(draft) - 10, len(draft)], process, output,
        expected_caret=len(draft) - 10)
    if before["selection"] != [len(draft) - 10, len(draft)]:
        raise RuntimeError("physical revocation selection did not reach the private editor")

    # The existing accessible Float action is the same retained control as the
    # pointer gesture already proven earlier in this run.
    activate_agent_conversation_control(access, "Float")
    frame = wait_for("fresh detached Tide pixels before revocation", lambda: (
        fresh_floating_presentation(
            presented_floating_frame(
                call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
            ),
            previous_float,
        )
    ), process)
    frame, detached_access, detached_before = wait_for_semantic_float_editor_continuity(
        socket, output, frame, {"access": access, "draft": draft, "observation": before}, process)
    (output / "revocation-float-editor-continuity.json").write_text(json.dumps(
        closed_editor_continuity(before, detached_before), indent=2) + "\n")
    if detached_before != before:
        raise RuntimeError("detaching before revocation changed private editor state")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"before-agent-membership-revocation-{size}.png"
    )}))

    remove = wait_for("owner-published Tide removal", lambda: offered_membership_action(
        socket, room, "desktop:agent:tide", "remove"), process)
    wait_terminal(
        socket, invoke(socket, remove), process,
        public_failure_path=output / "failed-agent-tide-removal-terminal.json",
    )

    def membership_withdrawn():
        if offered_membership_action(socket, room, "desktop:agent:tide", "remove") is not None:
            return None
        if offered_agent_action(socket, room, record, "tide", "open-agent-conversation") is not None:
            return None
        return True

    wait_for("Tide membership and Chat route withdrawal", membership_withdrawn, process)
    wait_for("old managed Tide process retirement", lambda: (
        True if old_identity not in owned_agent_identities(process.pid) else None
    ), process)
    revoked = wait_for(
        "detached Tide read-only revocation presentation",
        lambda: agent_read_only_accessibility("Tide", detached=True), process,
    )
    if provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("membership revocation initiated an unauthorized model turn")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"agent-membership-revoked-{size}.png"
    )}))

    add = wait_for("owner-published Tide re-admission", lambda: offered_membership_action(
        socket, room, "desktop:workspace", "add-agent"), process)
    wait_terminal(
        socket, invoke(socket, add, {"agent": "tide", "preset": "operator"}), process,
        public_failure_path=output / "failed-agent-tide-readmission-terminal.json",
    )
    wait_for("fresh Tide membership receipt", lambda: offered_membership_action(
        socket, room, "desktop:agent:tide", "remove"), process)
    wait_for("fresh Tide Chat route", lambda: offered_agent_action(
        socket, room, record, "tide", "open-agent-conversation"), process)
    start_managed_agent(socket, room, "tide", process,
                        public_failure_path=output / "failed-agent-tide-restart-terminal.json")

    def replacement_identity():
        identities = owned_agent_identities(process.pid)
        if len(identities) > 1:
            raise RuntimeError("re-admission launched more than one managed Agent")
        if len(identities) != 1:
            return None
        if identities[0] == old_identity:
            raise RuntimeError("re-admission reused the retired managed process")
        return identities[0]

    replacement_identity = wait_for(
        "fresh managed Tide process after re-admission", replacement_identity, process
    )
    if provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("fresh idle Agent launch initiated an unauthorized model turn")
    restored_access, restored, restored_names = wait_for_restored_detached_editor(socket, {
        "access": detached_access, "draft": draft, "observation": before,
        "identity": replacement_identity}, frame, provider, process, output)
    for field in ("draft_matches", "caret", "selection"):
        if restored[field] != before[field]:
            raise RuntimeError("re-admission did not restore the exact private editor state")
    if not {"Send message", "Queue task"}.issubset(restored_names):
        raise RuntimeError("re-admission did not restore mounted Message and Queue controls")
    before_restored_return = presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
    )
    if before_restored_return is None:
        raise RuntimeError("Chat's primary presentation disappeared before restored Return")
    restored_access = current_restored_return_access(restored_access, draft, before)
    initial_return = restored_return_observation(
        call(socket, {"op": "foreign_frames"}), frame, before_restored_return)
    if not initial_return["exact_child_exists"] or initial_return["child_presented"] is not True:
        raise RuntimeError("restored Return's original detached child is not presented")
    (output / "revocation-return-withdrawal-observations.json").write_text(json.dumps({
        "observation_count": 0, "initial": initial_return, "last": initial_return,
    }, indent=2) + "\n")
    activate_agent_conversation_control(restored_access, "Return", expected_name="Return conversation")
    wait_for_restored_return_withdrawal(
        socket, output, frame, before_restored_return, process, initial_return)

    def restored_primary_repainted():
        primary = presented_primary_frame(
            call(socket, {"op": "foreign_frames"}), "so.tilo.chat"
        )
        return primary if primary is not None \
            and primary["frame_seq"] > before_restored_return["frame_seq"] else None

    restored_primary = wait_for(
        "Chat's primary window to repaint after restored Return",
        restored_primary_repainted,
        process,
    )
    attached_access = wait_for(
        "returned Tide editor after re-admission",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    attached = private_editor_observation(attached_access, draft)
    for field in ("draft_matches", "caret", "selection"):
        if attached[field] != before[field]:
            raise RuntimeError("Return after re-admission changed private editor state")
    attached_access = wait_for_restored_agent_offers(socket, room, record, provider, process, {
        "identity": replacement_identity, "access": attached_access,
        "draft": draft, "observation": before}, output)
    if owned_agent_identity(process.pid) != replacement_identity \
            or provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("restored editor changed process custody or initiated model work")
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"agent-membership-restored-{size}.png"
    )}))
    evidence = {
        "owner_membership_revoked": True,
        "old_managed_process_retired": True,
        "read_only_notice_presented": True,
        "mutable_controls_removed": revoked["mutable_controls"] == 0,
        "editable_composer_removed": revoked["editable_composers"] == 0,
        "owner_membership_readmitted": True,
        "replacement_managed_process": True,
        "message_and_queue_controls_restored": True,
        "private_draft_restored": attached["draft_matches"],
        "caret_restored": attached["caret"] == before["caret"],
        "selection_restored": attached["selection"] == before["selection"],
        "returned_to_primary": True,
        "fresh_detached_frame": True,
        "detached_frame_seq": frame["frame_seq"],
        "fresh_primary_frame_after_return": True,
        "returned_primary_frame_seq": restored_primary["frame_seq"],
        "provider_request_count": provider.request_count(),
        "provider_overflow_count": provider.overflow_count(),
        "private_content_recorded": False,
        "test_owned_draft_rendered": True,
    }
    (output / "agent-membership-revocation.json").write_text(
        json.dumps(evidence, indent=2) + "\n"
    )
    return evidence


def prepare_private_permission_draft(socket, access, draft, selection_length, output, process):
    """Physically replace and select one fixture-owned private draft."""
    frame = wait_for("presented Chat frame for permission draft", lambda: presented_primary_frame(
        call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), process)
    physically_click_editor(socket, access, presented_content_geometry(frame))
    require_chat_keyboard_focus_after_click(socket, output)
    physical_chord(socket, "a", ctrl=True)
    require_ok(call(socket, {"op": "type", "text": draft}))
    wait_for("physical private permission draft", lambda: private_editor_value_matches(
        access, draft), process)
    with hold_physical_modifiers(socket, shift=True):
        for _ in range(selection_length):
            require_ok(call(socket, {"op": "key", "key": "ArrowLeft"}))
    return wait_for_private_selection(
        socket, access, draft, [len(draft) - selection_length, len(draft)], process, output
    )


def same_private_editor_state(left, right):
    return all(left.get(field) == right.get(field)
               for field in ("draft_matches", "caret", "selection"))


def native_chat_write_downgrade(socket, room, record, person, output, size, process):
    """Restrict only Chat.Write, then require expiry to restore exact state."""
    message_draft = "Keep this Message draft through permission loss"
    message_access = wait_for("Message editor before Chat restriction", lambda:
                              agent_conversation_accessibility("Tide"), process)
    initial_identity = chat_process_identity(message_access)
    message_before = prepare_private_permission_draft(
        socket, message_access, message_draft, 8, output, process
    )
    stale_message = wait_for("fresh Message offer before Chat restriction", lambda:
                             offered_agent_action(socket, room, record, "tide", "message"), process)
    opposite = offered_desktop_action(socket, room, "tasks", "desktop:tasks", "queue")
    if opposite is None:
        raise RuntimeError("independent Queue offer is absent before Chat restriction")
    chat_profile = assign_expiring_app_write_restriction(
        socket, room, person, "so.tilo.chat", "Native Chat write restriction", process
    )

    def chat_restricted():
        if offered_agent_action(socket, room, record, "tide", "message") is not None:
            return None
        if offered_desktop_action(socket, room, "tasks", "desktop:tasks", "queue") is None:
            raise RuntimeError("Chat restriction also removed the independent Task offer")
        return agent_write_restricted_accessibility("Tide", message_draft)

    restricted_chat = wait_for("mounted Chat.Write restriction", chat_restricted, process)
    observation = observe_hidden_permission_offer(
        socket, stale_message, opposite, "open-agent-conversation",
        message_access, restricted_chat, initial_identity,
    )
    message_refusal = require_stale_action_refused(
        socket, stale_message, {"value": message_draft}, process, observation=observation
    )
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"person-chat-write-restricted-{size}.png"
    )}))

    def chat_restored():
        restored = offered_agent_action(socket, room, record, "tide", "message")
        if restored is None:
            return None
        if not permission_offer_matches(restored, permission_offer_identity(stale_message)):
            raise RuntimeError("Chat.Write restoration changed the original offered capability")
        access = agent_conversation_accessibility("Tide")
        if access is None:
            return None
        if chat_process_identity(access) != initial_identity:
            raise RuntimeError("Chat.Write restoration replaced the original Chat process")
        observed = private_editor_observation(access, message_draft)
        return (access, observed) if same_private_editor_state(
            observed, message_before
        ) else None

    message_access, message_restored = wait_for(
        "expired Chat.Write restriction restoration", chat_restored, process, timeout=30
    )
    if time.time() < chat_profile["expires_unix"]:
        raise RuntimeError("Chat.Write restored before the real profile expired")
    return {
        "chat_write_restricted": True,
        "chat_read_only_notice_presented": restricted_chat["notice_present"],
        "chat_mutable_controls_removed": restricted_chat["mutable_controls"] == 0,
        "chat_stale_offer_refused": message_refusal == "hidden_action_refused",
        "task_offer_remained_during_chat_restriction": True,
        "chat_profile_expired": True,
        "chat_draft_restored": message_restored["draft_matches"],
        "chat_caret_restored": message_restored["caret"] == message_before["caret"],
        "chat_selection_restored": message_restored["selection"] == message_before["selection"],
    }


def native_task_write_downgrade(socket, room, record, person, output, size, process):
    """Restrict only Task.Write, then require expiry to restore exact state."""
    message_access = wait_for(
        "Message editor before Task restriction",
        lambda: agent_conversation_accessibility("Tide"), process,
    )
    activate_named_agent_control(message_access, "Queue")
    queue_access = wait_for("Task instruction editor before Task restriction", lambda:
                            agent_conversation_accessibility(
                                "Tide", composer_name="Task instruction composer"
                            ), process)
    initial_identity = chat_process_identity(queue_access)
    queue_draft = "Keep this Queue draft through permission loss"
    queue_before = prepare_private_permission_draft(
        socket, queue_access, queue_draft, 8, output, process
    )
    stale_queue = wait_for("fresh Queue offer before Task restriction", lambda:
                           offered_desktop_action(
                               socket, room, "tasks", "desktop:tasks", "queue"
                           ), process)
    opposite = offered_agent_action(socket, room, record, "tide", "message")
    if opposite is None:
        raise RuntimeError("independent Message offer is absent before Task restriction")
    task_profile = assign_expiring_app_write_restriction(
        socket, room, person, "so.tilo.desktop", "Native Task write restriction", process
    )

    def task_restricted():
        if offered_desktop_action(socket, room, "tasks", "desktop:tasks", "queue") is not None:
            return None
        if offered_agent_action(socket, room, record, "tide", "message") is None:
            raise RuntimeError("Task restriction also removed the independent Message offer")
        access = agent_conversation_accessibility(
            "Tide", composer_name="Task instruction composer"
        )
        if access is None:
            return None
        observed = private_editor_observation(access, queue_draft)
        names = accessible_names(access)
        if "Queue task" in names:
            return None
        return (access, observed) if same_private_editor_state(
            observed, queue_before
        ) else None

    restricted_access, queue_restricted = wait_for("mounted Task.Write restriction", task_restricted, process)
    observation = observe_hidden_permission_offer(
        socket, stale_queue, opposite, "read-current-tasks",
        queue_access, restricted_access, initial_identity,
    )
    queue_refusal = require_stale_action_refused(socket, stale_queue, {
        "instruction": queue_draft,
        "title": "Permission proof task",
        "agent": "tide",
        "route": "model",
    }, process, observation=observation)
    require_ok(call(socket, {"op": "snapshot", "path": str(
        output / f"person-task-write-restricted-{size}.png"
    )}))

    def task_restored():
        restored = offered_desktop_action(socket, room, "tasks", "desktop:tasks", "queue")
        if restored is None:
            return None
        if not permission_offer_matches(restored, permission_offer_identity(stale_queue)):
            raise RuntimeError("Task.Write restoration changed the original offered capability")
        access = agent_conversation_accessibility(
            "Tide", composer_name="Task instruction composer"
        )
        if access is None:
            return None
        if chat_process_identity(access) != initial_identity:
            raise RuntimeError("Task.Write restoration replaced the original Chat process")
        observed = private_editor_observation(access, queue_draft)
        return observed if same_private_editor_state(observed, queue_before) else None

    queue_restored = wait_for(
        "expired Task.Write restriction restoration", task_restored, process, timeout=30
    )
    if time.time() < task_profile["expires_unix"]:
        raise RuntimeError("Task.Write restored before the real profile expired")
    return {
        "task_write_restricted": True,
        "task_queue_control_removed": True,
        "task_stale_offer_refused": queue_refusal == "hidden_action_refused",
        "message_offer_remained_during_task_restriction": True,
        "task_profile_expired": True,
        "task_draft_retained_while_restricted": queue_restricted["draft_matches"],
        "task_draft_restored": queue_restored["draft_matches"],
        "task_caret_restored": queue_restored["caret"] == queue_before["caret"],
        "task_selection_restored": queue_restored["selection"] == queue_before["selection"],
    }


def native_person_permission_downgrades(
        socket, room, record, provider, output, size, process, binaries, env):
    """Prove independent real Person Chat.Write and Task.Write restrictions."""
    person = inspect_native_person(binaries, env, room)
    identity = owned_agent_identity(process.pid)
    if provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("permission proof requires the completed four-turn lifecycle")
    chat = native_chat_write_downgrade(
        socket, room, record, person, output, size, process
    )
    task = native_task_write_downgrade(
        socket, room, record, person, output, size, process
    )
    if owned_agent_identity(process.pid) != identity \
            or provider.request_count() != 4 or provider.overflow_count() != 0:
        raise RuntimeError("permission proof changed Agent custody or initiated model work")
    evidence = {
        "typed_person_verified_in_memory": True,
        **chat,
        **task,
        "provider_request_count": provider.request_count(),
        "provider_overflow_count": provider.overflow_count(),
        "private_content_recorded": False,
    }
    (output / "person-permission-downgrade.json").write_text(
        json.dumps(evidence, indent=2) + "\n"
    )
    return evidence


CATALOGUE_BOUNDARY_ROW_LIMIT = 16
CATALOGUE_BOUNDARY_COUNT_LIMIT = 65535


def catalogue_boundary_rows(lines, phases):
    """Extract only closed phases and bounded counts from exact trace records."""
    ansi = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
    prefix = r"(?:\d{4}-\d\d-\d\dT\S+\s+)?(?:INFO|DEBUG)\s+tilo_agent_catalogue:\s+"
    patterns = {
        1: re.compile(
            "^" + prefix
            + r"broker watched catalogue boundary phase=1 "
            + r"published_surfaces=(\d{1,6}) matching_live_surfaces=(\d{1,6}) "
            + r"scoped_actions=(\d{1,6}) permission_accepted_actions=(\d{1,6}) "
            + r"final_surfaces=(\d{1,6})$"
        ),
        2: re.compile(
            "^" + prefix
            + r"SDK watched catalogue boundary phase=2 "
            + r"surfaces=(\d{1,6}) actions=(\d{1,6})$"
        ),
        3: re.compile(
            "^" + prefix
            + r"Agent observed catalogue boundary phase=3 "
            + r"surfaces=(\d{1,6}) actions=(\d{1,6})$"
        ),
    }
    rows = []
    for raw in lines:
        line = ansi.sub("", raw)
        for phase in phases:
            matched = patterns[phase].fullmatch(line)
            if matched is None:
                continue
            counts = [int(value) for value in matched.groups()]
            if any(value > CATALOGUE_BOUNDARY_COUNT_LIMIT for value in counts):
                break
            if phase == 1:
                row = {
                    "phase": 1,
                    "published_surfaces": counts[0],
                    "matching_live_surfaces": counts[1],
                    "scoped_actions": counts[2],
                    "permission_accepted_actions": counts[3],
                    "final_surfaces": counts[4],
                }
            else:
                row = {"phase": phase, "surfaces": counts[0], "actions": counts[1]}
            rows.append(row)
            break
    return rows[-CATALOGUE_BOUNDARY_ROW_LIMIT:]


def broker_catalogue_boundary_diagnostic(output):
    """Read bounded shell windows and retain only exact phase-one counters."""
    path = output / "shell.log"
    if not path.is_file():
        return []
    with path.open("rb") as stream:
        stream.seek(0, os.SEEK_END)
        length = stream.tell()
        window = 512 * 1024
        if length <= 2 * window:
            stream.seek(0)
            chunks = [(stream.read(2 * window), False, False)]
        else:
            stream.seek(0)
            head = stream.read(window)
            stream.seek(length - window)
            tail = stream.read(window)
            chunks = [(head, False, True), (tail, True, False)]
    lines = []
    for chunk, drop_first, drop_last in chunks:
        chunk_lines = chunk.decode(errors="replace").splitlines()
        if drop_first and chunk_lines:
            chunk_lines = chunk_lines[1:]
        if drop_last and chunk_lines and not chunk.endswith((b"\n", b"\r")):
            chunk_lines = chunk_lines[:-1]
        lines.extend(chunk_lines[-4096:])
    return catalogue_boundary_rows(lines, (1,))


def managed_child_diagnostic(profile):
    """Classify bounded managed output without retaining task/model content."""
    logs = list(profile.rglob("launcher-debug-output.log"))
    if len(logs) != 1:
        return {"log_count": len(logs), "line_count": 0,
                "catalogue_boundaries": [], "first_error_kind": None}
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
            ("tilo_svc_collab_fd" in line or "managed collaboration descriptor" in line)
            and any(word in line for word in ("failed", "error", "refused", "invalid", "missing"))
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
        ("model_request_refused", lambda line: "model gateway refused request" in line),
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
        "catalogue_boundaries": catalogue_boundary_rows(lines, (2, 3)),
        "first_error_kind": error_kind,
    }


NATIVE_DELIVERY_STAGES = {
    "source_signer", "source_owner", "source_read", "legacy_read", "broker_original",
    "broker_facts", "broker_guards", "broker_stream_missing", "broker_stream_replaced",
    "broker_lease_missing", "broker_lease_retired", "broker_lease_unavailable",
    "broker_worker_registry", "broker_worker_missing", "source_output", "source_decode",
    "source_stage", "prepare", "sign", "upload_open", "upload_write", "owner_send",
    "document_custody", "document_primary", "document_route", "document_transport",
    "document_enqueue_busy", "document_enqueue_not_ready", "document_enqueue_unavailable",
    "document_enqueue_too_large",
}
NATIVE_DELIVERY_OUTCOMES = {
    "enabled", "disabled", "in_flight", "complete", "definitive_refusal", "uncertain",
    "saved",
}


def native_delivery_diagnostic(output):
    """Return only closed native-delivery state relayed from ordinary Chat."""
    log = output / "shell.log"
    if not log.is_file():
        return {"log_count": 0, "marker_count": 0,
                "markers": [], "source": None, "stage": None,
                "outcome": "unobserved", "log_truncated": False}
    with log.open("rb") as stream:
        stream.seek(0, os.SEEK_END)
        length = stream.tell()
        window = 512 * 1024
        if length <= 2 * window:
            stream.seek(0)
            chunks = [(stream.read(2 * window), False, False)]
        else:
            stream.seek(0)
            head = stream.read(window)
            stream.seek(length - window)
            tail = stream.read(window)
            chunks = [(head, False, True), (tail, True, False)]
    lines = []
    lines_truncated = False
    for chunk, drop_first, drop_last in chunks:
        chunk_lines = chunk.decode(errors="replace").splitlines()
        if drop_first and chunk_lines:
            chunk_lines = chunk_lines[1:]
        if drop_last and chunk_lines and not chunk.endswith((b"\n", b"\r")):
            chunk_lines = chunk_lines[:-1]
        lines_truncated = lines_truncated or len(chunk_lines) > 4096
        lines.extend(chunk_lines[-4096:])
    log_truncated = length > 2 * window or lines_truncated
    observed = []
    ansi = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
    marker = re.compile(
        r"^(\[app so\.tilo\.chat (?:stdout|stderr)\]\s+)?"
        r"(?:\d{4}-\d\d-\d\dT\S+\s+)?DEBUG\s+tilo_agent_delivery:\s+"
        r"native-delivery:([a-z_]+):([a-z_]+)\s*$"
    )
    for line in lines:
        matched = marker.fullmatch(ansi.sub("", line))
        if matched is None:
            continue
        app_prefix, stage, outcome = matched.groups()
        if stage in NATIVE_DELIVERY_STAGES and outcome in NATIVE_DELIVERY_OUTCOMES:
            observed.append(("chat_app" if app_prefix else "shell_broker", stage, outcome))
    if not observed:
        return {"log_count": 1, "marker_count": 0,
                "markers": [], "source": None, "stage": None,
                "outcome": "unobserved", "log_truncated": log_truncated}
    source, stage, outcome = observed[-1]
    return {"log_count": 1, "marker_count": len(observed),
            "markers": [{"source": marker[0], "stage": marker[1], "outcome": marker[2]}
                        for marker in observed[-32:]],
            "source": source, "stage": stage, "outcome": outcome,
            "log_truncated": log_truncated}


def finalize_fixture(
        profile, output, processes, logs, provider, runtime_owner, owned_fcitx=None):
    """Save the closed classifier, then always tear down test-owned state."""
    try:
        try:
            diagnostic = native_delivery_diagnostic(output)
            (output / "native-message-delivery-final.json").write_text(
                json.dumps(diagnostic, indent=2) + "\n"
            )
            # Marker and IME runs can stop before the full lifecycle's
            # first-request diagnostic. Classify the managed child before
            # deleting its private profile; never retain its raw output.
            managed = managed_child_diagnostic(profile)
            managed["broker_catalogue_boundaries"] = broker_catalogue_boundary_diagnostic(output)
            (output / "managed-child-final.json").write_text(
                json.dumps(managed, indent=2) + "\n"
            )
        except OSError:
            raise RuntimeError("native delivery diagnostic unavailable") from None
    finally:
        if owned_fcitx is None:
            shells = [process for process in processes
                      if process.poll() is None and process_program(process.pid) == "shell"]
            if len(shells) == 1:
                owned_fcitx = owned_fcitx_identity(shells[0].pid)
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
        cleanup_owned_fcitx(owned_fcitx)
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
    message_receipt = wait_for_direct_message_terminal(socket, invoke(socket, message_action, {
        "value": "Reply while the queued task remains active.",
    }), process, output)
    message_invocation = terminal_invocation(message_receipt)
    if re.fullmatch(r"[0-9a-fA-F-]{36}", message_invocation) is None:
        raise RuntimeError("direct Message lost its canonical invocation UUID")
    parent = "m_" + message_invocation.replace("-", "").lower()
    try:
        wait_for("parallel direct Message model request",
                 lambda: provider.request_count() >= 2, process)
    except RuntimeError:
        (output / "parallel-message-model-admission.json").write_text(
            json.dumps(provider.admission_diagnostic(), indent=2) + "\n")
        raise

    direction = wait_for("exact-run Direction action", lambda: offered_desktop_action(
        socket, room, "tasks", f"desktop:task:{task_id}", "direct"), process)
    direction_receipt = wait_for_direction_terminal(socket, direction, {
        "run_epoch": epoch,
        "after_revision": after,
        "text": "Keep the current task active after acknowledging this direction.",
    }, process, output)
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
    reply = wait_for_agent_reply(socket, room, record, parent, agent, process, output)

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
        "reply_matches_fixture": reply["reply_matches_fixture"],
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
    receipts = []
    for agent in ["tide", "reed"]:
        if offered_agent_action(socket, room, record, agent, "open-agent-conversation"):
            raise RuntimeError("scratch Chat improperly started with an admitted fixture agent")
        add = wait_for("normal native Add agent action", lambda: offered_membership_action(
            socket, room,
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
        remove = wait_for("governed agent membership readback", lambda: offered_membership_action(
            socket, room,
            f"desktop:agent:{agent}", "remove"), process)
        opened = wait_for("Chat's authenticated agent roster readback", lambda: offered_agent_action(
            socket, room, record, agent, "open-agent-conversation"), process)
        receipts.append({"agent": agent, "preset": "operator", "terminal": receipt,
                         "membership_resource": remove["resource"],
                         "chat_semantic_identity": opened["semantic_identity"]})
    # Ensure admitting the second agent did not replace the first.
    for agent in ["tide", "reed"]:
        if not offered_membership_action(socket, room, f"desktop:agent:{agent}", "remove") \
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


def inspect_native_person(binaries, env, room):
    """Return the exact admitted Person in memory; never retain its identifier."""
    result = subprocess.run([
        str(binaries / "examples/packaged_agent_lifecycle"), "--inspect-native-person",
        env["TILO_STATE_PATH"], room, "",
    ], env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    if result.returncode:
        raise RuntimeError("the private native Person inspector refused") from None
    try:
        observed = json.loads(result.stdout)
    except (TypeError, ValueError):
        raise RuntimeError("the private native Person inspector returned malformed output") from None
    person = observed.get("person") if isinstance(observed, dict) else None
    if not isinstance(person, str) or re.fullmatch(r"[0-9a-f]{64}", person) is None:
        raise RuntimeError("the private native Person inspector returned no typed Person")
    return person


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
              launch_path="applications", task_only=False, marker_only=False,
              ime_only=False):
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
    revocation_evidence = None
    permission_evidence = None
    marker_evidence = None
    owned_fcitx = None
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
        provider = HeldProvider(marker_agent="tide" if marker_only else None)
        run(binaries / "examples/proof_provider", [profile / "config", provider.endpoint], env)
        # Provider provisioning requires a genuinely empty profile. Merge the
        # normal input settings only after it has created that profile, while
        # still preceding the compositor's production settings load.
        if ime_only:
            configure_rime_input(profile)
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
            "tilo_ui::collaboration::broker=info,"
            "tilo_agent_catalogue=debug,"
            "tilo_agent=info,"
            "tilo_agent_delivery=debug,"
            "shell::shell::search::providers::live_app_actions=debug,"
            "shell::shell::search::action_api=debug,tilo_native_action_gate=debug,"
            "tilo_native_governance=debug,tilo_floating_handoff=debug"
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
                                       public_failure_path=output / "failed-agent-conversation-open-terminal.json",
                                       diagnostic_path=output / "agent-conversation-open-observation.json")
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
            if ime_only:
                owned_fcitx = wait_for(
                    "one compositor-owned Fcitx/Rime process",
                    lambda: owned_fcitx_identity(shell.pid), shell,
                )
                proven = native_ime_float_return(
                    socket, output, size, shell, owned_fcitx, env
                )
                (output / "bootstrap.json").write_text(json.dumps({
                    "stage": "native-ime",
                    "native_join_acknowledged": True,
                    "chat_surface_id": frame["surface_id"],
                    "viewport": size,
                    "launch_path": launch_path,
                    "chat_frame_seq": frame["frame_seq"],
                    "agent_invite": invited,
                    "native_ime_handoff_proven": proven,
                    "full_lifecycle_proven": False,
                }, indent=2) + "\n")
                if proven:
                    print("PASS genuine Rime preedit survived Chat Float and Return")
                return proven
            if marker_only:
                marker_evidence = managed_agent_marker_stage(
                    socket, enrollment["room"], category[0]["record"], provider, output, shell, size
                )
                proven = bool(
                    marker_evidence.get("provider_request_count") == 3
                    and marker_evidence.get("marker_activated") is True
                    and marker_evidence.get("quick_composer_accessible") is True
                    and marker_evidence.get("terminal_status") == "done"
                )
                (output / "bootstrap.json").write_text(json.dumps({
                    "stage": "native-marker",
                    "native_join_acknowledged": True,
                    "chat_surface_id": frame["surface_id"],
                    "viewport": size,
                    "launch_path": launch_path,
                    "chat_frame_seq": frame["frame_seq"],
                    "agent_invite": invited,
                    "native_agent_marker_proven": proven,
                    "full_lifecycle_proven": False,
                }, indent=2) + "\n")
                if proven:
                    print("PASS one managed Agent invoked an exact Chat action and opened its marker composer")
                return proven
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
                revocation_evidence = native_agent_membership_revocation(
                    socket, enrollment["room"], category[0]["record"], provider, output,
                    size, shell, lifecycle_evidence["_owned_process_identity"],
                    float_return_proven,
                )
                permission_evidence = native_person_permission_downgrades(
                    socket, enrollment["room"], category[0]["record"], provider, output,
                    size, shell, binaries, env,
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
            and float_return_evidence_proven(float_return_proven)
            and lifecycle_evidence is not None
            and lifecycle_evidence.get("semantic_start") is True
            and lifecycle_evidence.get("reply_actor") == "agent:tide"
            and lifecycle_evidence.get("reply_matches_fixture") is True
            and lifecycle_evidence.get("applied_revision")
                == lifecycle_evidence.get("saved_revision")
            and lifecycle_evidence.get("terminal_status") == "cancelled"
            and physical_send_evidence is not None
            and physical_send_evidence.get("same_managed_process") is True
            and physical_send_evidence.get("reply_actor") == "agent:tide"
            and physical_send_evidence.get("reply_matches_fixture") is True
            and physical_send_evidence.get("provider_request_count") == 4
            and revocation_evidence is not None
            and revocation_evidence.get("owner_membership_revoked") is True
            and revocation_evidence.get("old_managed_process_retired") is True
            and revocation_evidence.get("replacement_managed_process") is True
            and revocation_evidence.get("message_and_queue_controls_restored") is True
            and revocation_evidence.get("private_draft_restored") is True
            and revocation_evidence.get("caret_restored") is True
            and revocation_evidence.get("selection_restored") is True
            and revocation_evidence.get("provider_request_count") == 4
            and revocation_evidence.get("provider_overflow_count") == 0
            and permission_evidence is not None
            and permission_evidence.get("typed_person_verified_in_memory") is True
            and permission_evidence.get("chat_write_restricted") is True
            and permission_evidence.get("chat_stale_offer_refused") is True
            and permission_evidence.get("task_offer_remained_during_chat_restriction") is True
            and permission_evidence.get("chat_draft_restored") is True
            and permission_evidence.get("chat_caret_restored") is True
            and permission_evidence.get("chat_selection_restored") is True
            and permission_evidence.get("task_write_restricted") is True
            and permission_evidence.get("task_stale_offer_refused") is True
            and permission_evidence.get("message_offer_remained_during_task_restriction") is True
            and permission_evidence.get("task_draft_restored") is True
            and permission_evidence.get("task_caret_restored") is True
            and permission_evidence.get("task_selection_restored") is True
            and permission_evidence.get("provider_request_count") == 4
            and permission_evidence.get("provider_overflow_count") == 0
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
        finalize_fixture(
            profile, output, processes, logs, provider, runtime_owner, owned_fcitx
        )


def evidence_directory(stage, size):
    """Keep each run's receipts and renders separate, including failed runs."""
    parent = REPO / "target/packaged-agent-lifecycle" / f"{stage}-{size}"
    parent.mkdir(parents=True, exist_ok=True)
    return pathlib.Path(tempfile.mkdtemp(prefix="run-", dir=parent))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, required=True)
    parser.add_argument(
        "--stage",
        choices=["bootstrap", "native-bootstrap", "native-marker", "managed-task",
                 "native-ime", "lifecycle"],
        default="lifecycle",
    )
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
        marker_only=args.stage == "native-marker",
        ime_only=args.stage == "native-ime",
    )
    if args.stage in {"native-marker", "managed-task", "native-ime", "lifecycle"} and not proven:
        raise RuntimeError(
            "the requested native managed-Agent proof stage did not complete"
        )


if __name__ == "__main__":
    main()
