#!/usr/bin/env python3
"""Two independent native desktops on a private governed host and audio graph.

Run on the disposable remote builder. App-state-only mode needs no audio server.
No login profile, hardware microphone, screen capture, external account or public
listener is used. Missing evidence
fails the journey; a partial receipt never qualifies the pair.
"""
from __future__ import annotations

import argparse
import array
import concurrent.futures
import importlib.util
import json
import math
import os
import pathlib
import re
import signal
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid

from _control import call, require_ok
from reaper import die_with_parent

REPO = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "packaged_native", pathlib.Path(__file__).with_name("packaged-native-bootstrap.py"))
packaged = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaged)


class Fixture:
    def __init__(self, output, binaries, pipewire, routing_diagnostics=False):
        self.output, self.binaries, self.pipewire = output, binaries, pipewire
        self.routing_diagnostics = routing_diagnostics
        self.native_debugger = None
        self.root_owner = tempfile.TemporaryDirectory(prefix="tilo-native-pair-")
        self.root = pathlib.Path(self.root_owner.name)
        self.root.chmod(0o700)
        self.processes, self.logs, self.stages = [], [], []
        self.private_buses = []
        self.native_tabs = {}
        self.native_instances = {}
        self.native_hosts = {}
        self.native_processes = {}

    def focused_tab(self, control, room, previous):
        tree = call(control, {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
        workspace = next((row for row in tree.get("workspaces", [])
                          if row.get("id") == tree.get("active") and row.get("room") == room), None)
        strip_id = tree.get("active_strip", {}).get(str(tree.get("active")))
        strip = next((row for row in workspace.get("strips", [])
                      if row.get("id") == strip_id), None) if workspace else None
        group_id = tree.get("active_group", {}).get(str(strip_id))
        group = next((row for row in strip.get("window_groups", [])
                      if row.get("id") == group_id), None) if strip else None
        tab = next((row for row in group.get("tabs", [])
                    if row.get("id") == group.get("active_tab")), None) if group else None
        if tab is None:
            return None
        if tab.get("id") in previous:
            return None
        tile = tab.get("tile", {})
        if not any(isinstance(body, dict) and body.get("app_id") == "so.tilo.chat"
                   for body in tile.values()):
            return None
        return tab["id"]

    def presented_tab(self, control, tab_id):
        tree = call(control, {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
        tabs = [tab for workspace in tree.get("workspaces", [])
                for strip in workspace.get("strips", [])
                for group in strip.get("window_groups", [])
                for tab in group.get("tabs", []) if tab.get("id") == tab_id]
        if len(tabs) != 1:
            raise RuntimeError("launched native tab is absent or ambiguous")
        foreign = tabs[0].get("tile", {}).get("Foreign")
        if not isinstance(foreign, dict) or foreign.get("app_id") != "so.tilo.chat":
            return None
        record = tree.get("local_app_records", {}).get(str(tab_id))
        workspace = next(workspace for workspace in tree.get("workspaces", [])
                         if any(tab.get("id") == tab_id for strip in workspace.get("strips", [])
                                for group in strip.get("window_groups", []) for tab in group.get("tabs", [])))
        if (not isinstance(record, dict) or record.get("app_id") != "so.tilo.chat"
                or record.get("room") != workspace.get("room")
                or type(record.get("record_instance")) is not int or record["record_instance"] <= 0):
            return None
        binding = (record["room"], record["record_instance"])
        if control in self.native_instances and self.native_instances[control] != binding:
            raise RuntimeError("launched native tab changed its authenticated instance")
        self.native_instances[control] = binding
        frames = [row for row in packaged.foreign_frames(call(control, {"op": "foreign_frames"}))
                  if row.get("surface_id") == foreign.get("surface_id")]
        if len(frames) > 1:
            raise RuntimeError("exact native tab has ambiguous frames")
        return next((row for row in frames if row.get("app_id") == "so.tilo.chat"
                     and row.get("floating_id") is None and row.get("presented") is True
                     and "last_error" in row and row["last_error"] is None
                     and type(row.get("surface_id")) is int and row["surface_id"] > 0
                     and type(row.get("frame_seq")) is int and row["frame_seq"] > 0), None)

    def spawn(self, label, command, env, **kwargs):
        log = open(self.output / f"{label}.log", "w")
        self.logs.append(log)
        process = subprocess.Popen(list(map(str, command)), env=env, stdout=log,
                                   stderr=log, start_new_session=True,
                                   preexec_fn=die_with_parent(), **kwargs)
        self.processes.append(process)
        return process

    def record(self, stage, **evidence):
        self.stages.append({"stage": stage, **evidence})
        print(stage, flush=True)

    def native_process_evidence(self, process, control):
        """Only this fixture's exact child and bounded verified descendants."""
        if process not in self.processes:
            raise RuntimeError("native process is outside fixture custody")
        evidence = {"pid": process.pid, "returncode": process.poll(), "processes": [],
                    "observation_errors": [], "descendant_limit": 24}
        pending = [(process.pid, os.getpid(), 0)]
        seen = set()
        fields = {"Name", "State", "PPid", "Threads", "CoreDumping", "SigPnd", "ShdPnd"}
        while pending and len(seen) < 24:
            pid, parent, depth = pending.pop(0)
            if pid in seen:
                continue
            seen.add(pid)
            root = pathlib.Path(f"/proc/{pid}")
            try:
                status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
                if int(status["PPid"]) != parent:
                    raise RuntimeError("native child no longer has its observed parent")
                row = {key: value.strip() for key, value in status.items() if key in fields}
                row["pid"] = pid
                evidence["processes"].append(row)
                if depth < 3:
                    children = set()
                    for task in sorted((root / "task").iterdir())[:128]:
                        try:
                            children.update(int(child) for child in (task / "children").read_text().split())
                        except OSError:
                            continue
                    pending.extend((child, pid, depth + 1) for child in sorted(children)[:24])
            except (OSError, ValueError, RuntimeError) as error:
                evidence["observation_errors"].append({"pid": pid, "error": str(error)})
        evidence["descendants_truncated"] = bool(pending)
        path = pathlib.Path(control)
        try:
            metadata = path.lstat()
            evidence["control_socket"] = {"inode": metadata.st_ino, "mode": metadata.st_mode,
                                          "uid": metadata.st_uid}
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(1)
                client.connect(control)
                peer, uid, gid = struct.unpack("3i", client.getsockopt(
                    socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
                evidence["control_socket"].update({"connect": "accepted", "peer_pid": peer,
                                                   "peer_uid": uid, "peer_gid": gid})
        except OSError as error:
            evidence.setdefault("control_socket", {}).update({"connect": "failed", "errno": error.errno,
                                                               "error_type": type(error).__name__})
        return evidence

    def env(self, name):
        profile = self.root / name
        for directory in ("home", "config", "data", "state", "cache", "run", "identity"):
            (profile / directory).mkdir(parents=True, mode=0o700, exist_ok=True)
        env = {**os.environ, "HOME": str(profile / "home"),
               **{f"XDG_{kind.upper()}_HOME": str(profile / kind)
                  for kind in ("config", "data", "state", "cache")},
               "XDG_RUNTIME_DIR": str(profile / "run"),
               "TILO_IDENTITY_ROOT": str(profile / "identity"),
               "TILO_IDENTITY_FILE_SECRETS": "1",
               "TILO_SECRETS_DIR": str(profile / "state/secrets"),
               "CARGO_BUILD_JOBS": "4",
               "TILO_PEER_KEY_PATH": str(profile / "identity/device-private/keypair.bin"),
               "TILO_TEST_CONTROL": str(profile / "run/control.sock"),
               "TILO_STATE_PATH": str(profile / "config/tilo/state.json"),
               "TILO_HEADLESS_SIZE": "1280x800",
               "TILO_LAUNCHER_BIN": str(self.binaries / "tilo-launcher"),
               "RUST_LOG": "warn,tilo_joined_room::voice=debug,tilo_media::voice::gst=debug,shell::network::native_media=debug,shell::shell::native_media=debug,shell::shell::broker_services=debug,tilo_action_broker::collab_broker_capnp::native_media=debug",
               "GST_DEBUG": "pipewiresrc:6,pipewirecore:5,appsink:5,opusenc:5,basesink:5,basesrc:6"}
        if self.routing_diagnostics:
            env["TILO_RELAY_PHASE_DIAGNOSTICS"] = "1"
            env["TILO_RELAY_CORRELATED_DIAGNOSTICS"] = "1"
            env["RUST_LOG"] += (",tilo_relay::peer=info,tilo_relay::rooms=debug,tilo_relay::native_publication=debug"
                                ",shell::network::actor=debug"
                                ",shell::shell::network::native_ingress=debug")
        if self.pipewire is not None:
            env["GST_PLUGIN_PATH"] = str(self.pipewire / "usr/lib64/gstreamer-1.0")
            env["PATH"] = str(self.pipewire / "usr/bin") + os.pathsep + env["PATH"]
        else:
            env.pop("GST_PLUGIN_PATH", None)
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS",
                    "PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR", "TILO_COLLAB_HOST",
                    "TILO_COLLAB_HOSTS", "TILO_APPS_SYSTEM_DIR", "TILO_USER_ID"):
            env.pop(key, None)
        return env

    def start_private_bus(self, name, env):
        path = pathlib.Path(env["XDG_RUNTIME_DIR"]) / "fixture-bus"
        process = self.spawn(f"{name}-bus", [
            "dbus-daemon", "--session", "--nofork", f"--address=unix:path={path}",
        ], env)
        self.private_buses.append(process)
        packaged.wait_for("owned private session bus", path.exists, process)
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={path}"
        return process

    def audio(self, name, env, frequency):
        profile = self.root / name
        config = profile / "pipewire"
        shutil.copytree(self.pipewire / "usr/share/pipewire", config)
        drop = config / "pipewire.conf.d"
        drop.mkdir(exist_ok=True)
        (drop / "99-fixture.conf").write_text('''context.objects = [
 { factory = adapter args = {
   factory.name = support.null-audio-sink node.name = fixture-sink
   node.description = "Fixture output" media.class = Audio/Sink
   audio.position = [ MONO ]
 } }
]
''')
        server = self.spawn(f"{name}-pipewire", [self.pipewire / "usr/bin/pipewire"],
                            {**env, "PIPEWIRE_CONFIG_DIR": str(config)})
        packaged.wait_for("private audio server", lambda: (profile / "run/pipewire-0").exists(), server)
        wpconfig = profile / "config/wireplumber/wireplumber.conf.d"
        wpconfig.mkdir(parents=True)
        (wpconfig / "99-fixture.conf").write_text('''wireplumber.profiles = {
 main = { monitor.alsa = disabled monitor.bluez = disabled
          monitor.v4l2 = disabled monitor.libcamera = disabled }
}
''')
        self.spawn(f"{name}-wireplumber", ["wireplumber"], env)
        # A generated-only native stream follows the private graph's driver.
        # The GStreamer provide-mode driver stalls after its initial buffer ring
        # here, even in a standalone encoder; it cannot qualify native voice.
        tone = profile / "generated-tone.raw"
        second = array.array("h", (round(0.2 * 32767 * math.sin(math.tau * frequency * i / 48000))
                                   for i in range(48000)))
        tone.write_bytes(second.tobytes() * 300)
        source = self.spawn(f"{name}-tone", [
            "pw-cat", "--playback", "--raw", "--rate=48000", "--channels=1",
            "--format=s16", "--target=0", "--latency=20ms", "--properties",
            json.dumps({"media.class": "Audio/Source", "node.name": "fixture-source",
                        "node.description": "Fixture-tone", "node.virtual": True,
                        "node.always-process": True, "node.pause-on-idle": False}), str(tone),
        ], env)

        def sources():
            rows = json.loads(subprocess.check_output(["pw-dump"], env=env, timeout=3))
            nodes = [row.get("info", {}).get("props", {}) for row in rows
                     if row.get("type") == "PipeWire:Interface:Node"]
            audio = [node for node in nodes if node.get("media.class") in {"Audio/Source", "Audio/Sink"}]
            names = {node.get("node.name") for node in audio}
            if names - {"fixture-source", "fixture-sink"}:
                raise RuntimeError("private graph contains an unexpected audio device")
            return audio if names == {"fixture-source", "fixture-sink"} else None

        nodes = packaged.wait_for("only generated source and null output", sources, source)
        self.record(f"{name}-isolated-audio", frequency_hz=frequency,
                    devices=[{"class": n["media.class"], "name": n["node.name"]} for n in nodes])
        self.encoder_preflight(name, env, nodes)

    def encoder_preflight(self, name, env, nodes, phase="generated-only"):
        if any(node.get("node.name") not in {"fixture-source", "fixture-sink"} for node in nodes):
            raise RuntimeError("private encoder preflight found an unapproved audio device")
        # Qualify this generated-only graph before testing the governed FD
        # handoff. This is fixture preflight, never native call acceptance.
        selected = [node for node in nodes if node.get("node.name") == "fixture-source"]
        if len(selected) != 1 or not isinstance(selected[0].get("object.serial"), int):
            raise RuntimeError("private generated source serial is unobservable")
        command = [
            "gst-launch-1.0", "-q", "pipewiresrc",
            f"target-object={selected[0]['object.serial']}", "do-timestamp=true",
            "stream-properties=properties,node.dont-fallback=(boolean)true,node.dont-reconnect=(boolean)true,node.dont-move=(boolean)true,node.linger=(boolean)false",
            "num-buffers=64", "!", "audioconvert", "!", "audioresample", "!",
            "audio/x-raw,rate=48000,channels=1,format=S16LE", "!", "opusenc",
            "frame-size=20", "!", "fakesink", "sync=false",
        ]
        for bound_fd in (False, True):
            # This fresh fixture socket tests the FD-enabled plugin separately
            # from the production observer's one-shot protocol handoff.
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as remote:
                if bound_fd:
                    remote.settimeout(2)
                    remote.connect(str(self.root / name / "run/pipewire-0"))
                arguments = command[:3] + ([f"fd={remote.fileno()}"] if bound_fd else []) + command[3:]
                try:
                    probe = subprocess.run(
                        arguments, env=env, timeout=2, stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE,
                        pass_fds=(remote.fileno(),) if bound_fd else (),
                    )
                except subprocess.TimeoutExpired as failure:
                    diagnostic = re.sub(r"\x1b\[[0-9;]*m", "", (failure.stderr or b"").decode(errors="replace"))
                    lines = [re.sub(r"0x[0-9a-fA-F]+", "<pointer>", line)
                             for line in diagnostic.splitlines()
                             if any(marker in line for marker in ("encoding 960 samples", "sync", "clock returned", "popped buffer", "pts "))]
                    (self.output / f"{name}-{phase}-encoder-timeout.json").write_text(json.dumps({
                        "bound_fd": bound_fd,
                        "encoded_frames": diagnostic.count("encoding 960 samples"),
                        "tail": [line.replace(str(self.root), "<fixture>") for line in lines[-12:]],
                    }, indent=2))
                    raise RuntimeError(
                        f"generated-only encoder preflight exceeded startup deadline (fd={bound_fd})"
                    ) from failure
                if probe.returncode != 0:
                    raise RuntimeError(f"generated-only encoder preflight failed ({probe.returncode}, fd={bound_fd})")
            suffix = "fd-encoder-preflight" if bound_fd else "encoder-preflight"
            self.record(f"{name}-{phase}-{suffix}")

    def stop_private_buses(self):
        # Buses are foreground children with known custody, never discovered by
        # scanning unrelated processes or inspecting another process's environment.
        stopped = 0
        for process in self.private_buses:
            if process.poll() is not None:
                continue
            descriptor = None
            try:
                if process not in self.processes:
                    raise RuntimeError("private bus is outside fixture custody")
                descriptor = os.pidfd_open(process.pid)
                root = pathlib.Path(f"/proc/{process.pid}")
                status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
                if int(status["PPid"]) != os.getpid() or status["Name"].strip() != "dbus-daemon":
                    raise RuntimeError("private bus no longer has its observed identity")
                signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                poller = select.poll()
                poller.register(descriptor, select.POLLIN)
                if not poller.poll(3000):
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                    if not poller.poll(3000):
                        raise RuntimeError("owned private bus did not exit")
                stopped += 1
                process.wait(timeout=3)
            except (ProcessLookupError, FileNotFoundError):
                continue
            except PermissionError as error:
                raise RuntimeError("private bus custody cannot be observed") from error
            finally:
                if descriptor is not None:
                    os.close(descriptor)
        return stopped

    def cleanup_proof(self):
        with tempfile.TemporaryDirectory(prefix="tilo-neighbor-bus-") as neighbor:
            directory = pathlib.Path(neighbor)
            (directory / "home").mkdir(mode=0o700)
            (directory / "run").mkdir(mode=0o700)
            owned = self.env("alice")
            adjacent = {**owned, "HOME": str(directory / "home"), "XDG_RUNTIME_DIR": str(directory / "run")}
            descriptors = []
            try:
                process = self.start_private_bus("cleanup-owned", owned)
                descriptors.append(os.pidfd_open(process.pid))
                path = directory / "run/fixture-bus"
                process = self.spawn("cleanup-adjacent", [
                    "dbus-daemon", "--session", "--nofork", f"--address=unix:path={path}",
                ], adjacent)
                packaged.wait_for("adjacent private session bus", path.exists, process)
                descriptors.append(os.pidfd_open(process.pid))
                if self.stop_private_buses() != 1:
                    raise RuntimeError("cleanup did not select exactly the owned private bus")
                owned_poll, adjacent_poll = select.poll(), select.poll()
                owned_poll.register(descriptors[0], select.POLLIN)
                adjacent_poll.register(descriptors[1], select.POLLIN)
                if not owned_poll.poll(0) or adjacent_poll.poll(0):
                    raise RuntimeError("private cleanup did not preserve the adjacent bus")
                self.record("exact-private-bus-cleanup-proved", owned_exited=True, adjacent_survived=True)
            finally:
                for descriptor in descriptors:
                    try:
                        signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                        poller = select.poll()
                        poller.register(descriptor, select.POLLIN)
                        if not poller.poll(3000):
                            raise RuntimeError("cleanup proof daemon did not exit")
                    except ProcessLookupError:
                        pass
                    finally:
                        os.close(descriptor)

    def close(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    continue
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        self.record("private-process-cleanup", escaped_buses_stopped=self.stop_private_buses())
        for log in self.logs:
            log.close()
        self.root_owner.cleanup()

    def operation(self, socket, room, operation, process, resource=None):
        def offered():
            rows = [row for row in self.catalogue(socket)
                    if row.get("application") == "so.tilo.chat"
                    and row.get("room") == room and row.get("record") == ""
                    and row.get("operation") == operation
                    and (resource is None or row.get("resource") == resource
                         or resource.startswith("chat.") and self.logical_resource(row["resource"]) == resource)]
            if len(rows) > 1:
                raise RuntimeError(f"ambiguous Chat operation {operation}")
            return rows[0] if rows else None
        return packaged.wait_for(f"Chat {operation}", offered, process)

    @staticmethod
    def raw_catalogue(socket, query=""):
        response = call(socket, {"op": "actions_enumerate", "include_programmatic": True,
                                 "detail": "full", "limit": 2000, "query": query})
        if "err" in response or not isinstance(response.get("answer", {}).get("actions"), list):
            raise RuntimeError("complete native action catalogue is unavailable")
        if response["answer"].get("truncated") is not False:
            raise RuntimeError("native action catalogue completeness cannot be observed")
        return response["answer"]["actions"]

    def catalogue(self, socket, query=""):
        rows = self.raw_catalogue(socket, query)
        binding = self.native_instances.get(socket)
        if binding is None:
            return rows
        if socket not in self.native_hosts:
            room, instance = binding
            matches = set()
            observations = []
            for row in self.raw_catalogue(socket):
                if (row.get("application") != "so.tilo.chat" or row.get("room") != room
                        or row.get("record") != "" or row.get("operation") != "live-context-status"):
                    continue
                receipt = packaged.wait_terminal(socket, packaged.invoke(socket, row),
                                                  self.native_processes[socket])
                value = receipt.get("update", {}).get("output", {}).get("value", {})
                view = value.get("nativeView")
                observations.append({
                    "native_view_present": isinstance(view, dict),
                    "observed_instance": view.get("instance") if isinstance(view, dict)
                        and type(view.get("instance")) is int else None,
                    "room_matches": isinstance(view, dict) and view.get("room") == room,
                    "record_matches": isinstance(view, dict) and view.get("record") == "",
                })
                if view == {"instance": instance, "room": room, "record": ""}:
                    if any(value.get(field) is not False for field in ("sharing", "following", "videoCapture")):
                        raise RuntimeError("exact native view did not start with private context")
                    matches.add(self.publication_host(row["resource"]))
            (self.output / f"native-instance-{instance}-observation.json").write_text(json.dumps({
                "expected_instance": instance, "observations": observations}, indent=2))
            if len(matches) > 1:
                raise RuntimeError("exact native instance has ambiguous action publishers")
            if not matches:
                return [row for row in rows if row.get("application") != "so.tilo.chat"]
            self.native_hosts[socket] = matches.pop()
            self.record("native-invocation-metadata-matches-authenticated-launched-tab",
                        room=room, instance=instance, record="", initial_context_private=True)
        host = self.native_hosts[socket]
        return [row for row in rows if row.get("application") != "so.tilo.chat"
                or self.publication_host(row["resource"]) == host]

    @staticmethod
    def publication_host(resource):
        prefix, version, host, lifetime, encoded = resource.split(":", 4)
        if prefix != "tilo-ui-bound" or version != "v1" or len(resource) > 4096:
            raise RuntimeError("unexpected native resource publication")
        uuid.UUID(host)
        uuid.UUID(lifetime)
        bytes.fromhex(encoded).decode("utf-8")
        return host

    @staticmethod
    def logical_resource(resource):
        # BoundPublication aliases include a host and component lifetime;
        # only the UTF-8 domain resource identifies shared CRDT state.
        prefix, version, host, lifetime, encoded = resource.split(":", 4)
        if prefix != "tilo-ui-bound" or version != "v1" or len(resource) > 4096:
            raise RuntimeError("unexpected native resource publication")
        uuid.UUID(host)
        uuid.UUID(lifetime)
        return bytes.fromhex(encoded).decode("utf-8")

    def act(self, session, room, operation, args=None, resource=None):
        socket, process = session
        row = self.operation(socket, room, operation, process, resource)
        if operation in {"start-call", "join-call", "unmute"}:
            # These exact generated-audio call steps are explicitly authorized.
            # Carry confirmation only through a parameter the current door
            # declares; microphone consent remains its separate visible prompt.
            parameters = row.get("params", [])
            observation = {**packaged.public_action_observation(row),
                           "contract": row.get("contract"),
                           "params": [{key: param.get(key) for key in ("name", "ty", "required")}
                                      for param in parameters]}
            (self.output / f"{operation}-declared-contract.json").write_text(json.dumps(observation, indent=2))
            if args is None and any(param.get("name") == "confirm" for param in parameters):
                args = {"confirm": True}
                self.record("explicit-generated-call-action-confirmation", operation=operation)
        acknowledgement = packaged.invoke(socket, row, args)
        failure_path = self.output / f"failed-{operation}.json"
        try:
            receipt = packaged.wait_terminal(socket, acknowledgement, process,
                                            public_failure_path=failure_path)
        except RuntimeError:
            if not failure_path.exists():
                # Public, test-owned operations only. Observe the original job;
                # never re-invoke it or export arguments/results on timeout.
                initial = acknowledgement.get("receipt", {})
                job = initial.get("broker_invocation_id")
                observation = {"observation": "terminal_not_observed",
                               "action": packaged.public_action_observation(row),
                               "original": self.action_status(initial)}
                if type(job) is int and job > 0 and process.poll() is None:
                    try:
                        status = call(socket, {"op": "actions_status", "invocation_id": job})
                        observation["last"] = self.action_status(status.get("answer", {}).get("receipt", {}))
                        error = status.get("err")
                        if isinstance(error, dict):
                            observation["status_error"] = {key: value[:4096] for key, value in error.items()
                                                           if key in {"code", "message"} and isinstance(value, str)}
                    except Exception as diagnostic_error:
                        observation["status_unobservable"] = type(diagnostic_error).__name__
                failure_path.write_text(json.dumps(observation, indent=2) + "\n")
            raise
        return receipt.get("update", {}).get("output", {}).get("value")

    @staticmethod
    def action_status(receipt):
        update = receipt.get("update") or {}
        return {"broker_invocation_id": receipt.get("broker_invocation_id"),
                "invocation": update.get("invocation"), "state": update.get("state")}


    def shared_state(self, sessions, room):
        alice, bob = sessions["alice"], sessions["bob"]
        for session in (alice, bob):
            report = self.act(session, room, "live-context-status")
            if report.get("sharing") is not False or report.get("following") is not False or report.get("videoCapture") is not False:
                raise RuntimeError("native Chat did not start with private context")
        def text_channels(socket):
            channels = {}
            for row in self.catalogue(socket):
                if (row.get("application") != "so.tilo.chat" or row.get("room") != room
                        or row.get("operation") != "post"):
                    continue
                resource = row["resource"]
                logical = self.logical_resource(resource)
                if not logical.startswith("chat.channel:") or logical in channels:
                    raise RuntimeError("ambiguous native Chat channel identity")
                channels[logical] = resource
            return channels

        # Ordinary apps start empty. Continuity modes already created their
        # first channel before Bob opened; the full pair must create its own.
        if not text_channels(alice[0]):
            self.act(alice, room, "new-channel", {"value": "Fixture first conversation"})
        self.act(alice, room, "new-channel", {"value": "Fixture second conversation"})
        channels = packaged.wait_for("two shared text channels", lambda: (
            channels if len(channels := text_channels(alice[0])) >= 2 else None), alice[1])
        first, second = sorted(channels)[:2]
        self.record("alice-new-channel-committed", channels=sorted(channels))
        try:
            packaged.wait_for("native peer sees the same resources", lambda: (
                set(channels).issubset(text_channels(bob[0]))), bob[1])
        except Exception:
            for name, session in sessions.items():
                (self.output / f"{name}-convergence-actions.json").write_text(json.dumps(
                    [packaged.public_action_observation(row) for row in self.catalogue(session[0])], indent=2))
                require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-convergence.png")}))
            raise
        self.record("both-native-resource-catalogues-converged", channels=sorted(channels))
        # Address each conversation through its own current native resource.
        # Generated fixture messages must converge without crossing channels.
        expected = {first: "Fixture Alice in the first conversation",
                    second: "Fixture Bob in the second conversation"}
        for session, logical in ((alice, first), (bob, second)):
            self.act(session, room, "post", {"value": expected[logical]},
                     resource=logical)
        def readable_conversations():
            for session in (alice, bob):
                resources = text_channels(session[0])
                for logical, text in expected.items():
                    page = self.act(session, room, "read-messages", {"pageSize": 32},
                                    resource=logical)
                    content = page.get("content", {}) if isinstance(page, dict) else {}
                    messages = content.get("messages", [])
                    if content.get("channel") != logical.removeprefix("chat.channel:"):
                        raise RuntimeError("native history returned another conversation")
                    if any(message.get("text") == other for other in expected.values()
                           if other != text for message in messages):
                        raise RuntimeError("native message crossed conversations")
                    if sum(message.get("text") == text and message.get("complete") is True
                           for message in messages) != 1:
                        return False
            return True
        packaged.wait_for("each native conversation has its own readable message",
                          readable_conversations, bob[1])
        self.record("both-native-correct-conversation-messages-readable")
        reference = {"kind": "chat.channel", "id": second,
                     "label": "Fixture shared conversation", "span": None}
        self.act(alice, room, "share-resource", {
            "reference": reference, "text": "Fixture live resource address",
            "sourceProvider": "record:so.tilo.chat:"}, resource=first)
        def shared_reference():
            page = self.act(bob, room, "read-messages", {"pageSize": 32},
                            resource=first)
            messages = page.get("content", {}).get("messages", [])
            matches = [message for message in messages
                       if message.get("text") == "Fixture live resource address"]
            if len(matches) > 1:
                raise RuntimeError("native resource share duplicated its message")
            if not matches:
                return None
            logical = "chat.message:" + matches[0]["id"]
            rows = [row for row in self.catalogue(bob[0])
                    if row.get("application") == "so.tilo.chat" and row.get("room") == room
                    and row.get("operation") == "read-message"
                    and self.logical_resource(row["resource"]) == logical]
            if len(rows) != 1:
                return None
            report = self.act(bob, room, "read-message", resource=rows[0]["resource"])
            if report.get("channel") != first.removeprefix("chat.channel:"):
                raise RuntimeError("native resource share crossed conversations")
            return report if report.get("reference") == reference and report.get(
                "sourceProvider") == "record:so.tilo.chat:" else None
        packaged.wait_for("live resource address reaches admitted native reader",
                          shared_reference, bob[1])
        self.record("native-live-resource-reference-preserves-existing-access")
        return text_channels, shared_reference

    def context(self, sessions, room, presence_ids):
        text_channels, _shared_reference = self.shared_state(sessions, room)
        alice, bob = sessions["alice"], sessions["bob"]
        channels = text_channels(alice[0])
        first, second = sorted(channels)[:2]
        self.act(alice, room, "open-channel", resource=first)
        self.act(bob, room, "open-channel", resource=second)
        require_ok(call(alice[0], {"op": "pointer_move", "x": 350, "y": 220}))
        require_ok(call(bob[0], {"op": "pointer_move", "x": 640, "y": 300}))
        self.act(alice, room, "share-live-context")

        def connected():
            response = call(bob[0], {"op": "state_dump"})
            tree = response.get("state", {}).get("workspace_tree", {})
            (self.output / "bob-follow-roster.json").write_text(json.dumps({
                "expected_certified_session": presence_ids[0],
                "workspaces": [{"room": workspace.get("room"),
                                "collaborators": workspace.get("collaborators"),
                                "tabs": [{"id": tab.get("id"), "title": tab.get("title")}
                                         for strip in workspace.get("strips", [])
                                         for group in strip.get("window_groups", [])
                                         for tab in group.get("tabs", [])]}
                               for workspace in tree.get("workspaces", [])],
                "presence": response.get("state", {}).get("chrome", {}).get("presence"),
                "peers": response.get("state", {}).get("peers")}, indent=2))
            return any(row.get("id") == presence_ids[0] and row.get("invited") is False
                       and row.get("active_tab") is not None
                       and any(tab.get("id") == row["active_tab"]
                               for strip in workspace.get("strips", [])
                               for group in strip.get("window_groups", [])
                               for tab in group.get("tabs", []))
                       for workspace in tree.get("workspaces", []) if workspace.get("room") == room
                       for row in workspace.get("collaborators", []))

        packaged.wait_for("Alice's certified session and admitted tab reach Bob", connected, bob[1])
        follow = packaged.require_action(bob[0], "presence.follow")
        receipt = packaged.wait_terminal(bob[0], packaged.invoke(bob[0], follow, {
            "collaborator": presence_ids[0]}), bob[1],
            public_failure_path=self.output / "failed-shell-follow.json")
        (self.output / "shell-follow-terminal.json").write_text(json.dumps(receipt, indent=2))

        def following():
            report = self.act(bob, room, "live-context-status")
            state = call(bob[0], {"op": "state_dump"}).get("state", {})
            tree = state.get("workspace_tree", {})
            (self.output / "bob-follow-observation.json").write_text(json.dumps({
                "report": report,
                "presence": state.get("chrome", {}).get("presence"),
                "workspaces": [{"room": workspace.get("room"),
                    "collaborators": workspace.get("collaborators"),
                    "tabs": [{"id": tab.get("id"), "title": tab.get("title")}
                             for strip in workspace.get("strips", [])
                             for group in strip.get("window_groups", [])
                             for tab in group.get("tabs", [])]}
                    for workspace in tree.get("workspaces", [])]}, indent=2))
            expected = first.removeprefix("chat.channel:")
            if report.get("following") is not True or report.get("channel") != expected:
                return None
            # A different local Chat can report the followed conversation while
            # shell focus is on an unavailable remote-tab placeholder. Require
            # the exact admitted target to paint a real local signed app frame.
            workspace = next((workspace for workspace in tree.get("workspaces", [])
                              if workspace.get("room") == room), None)
            if workspace is None:
                return None
            collaborator = next((row for row in workspace.get("collaborators", [])
                                 if row.get("id") == presence_ids[0]), None)
            target = next((tab for strip in workspace.get("strips", [])
                           for group in strip.get("window_groups", [])
                           for tab in group.get("tabs", [])
                           if collaborator is not None and tab.get("id") == collaborator.get("active_tab")), None)
            foreign = target.get("tile", {}).get("Foreign") if target is not None else None
            surface = foreign.get("surface_id") if isinstance(foreign, dict) else None
            frames = packaged.foreign_frames(call(bob[0], {"op": "foreign_frames"}))
            presented = any(frame.get("surface_id") == surface and surface is not None
                            and frame.get("app_id") == "so.tilo.chat"
                            and frame.get("presented") is True and frame.get("frame_seq", 0) > 0
                            and frame.get("last_error") is None for frame in frames)
            (self.output / "bob-follow-presentation.json").write_text(json.dumps({
                "target_tab": target.get("id") if target is not None else None,
                "target_kind": next(iter(target.get("tile", {})), None) if target is not None else None,
                "peer_card": {key: value[:2048] for key, value in
                              (target.get("tile", {}).get("PeerWindow", {}) if target is not None else {}).items()
                              if key in {"app_id", "note"} and isinstance(value, str)},
                "actual_local_frame_presented": presented}, indent=2))
            return report if presented else None

        report = packaged.wait_for("native Chat follows the other native context", following, bob[1])
        self.record("native-context-follows-opt-in", report=report)
        for name, session in sessions.items():
            require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-following.png")}))
        self.act(alice, room, "stop-sharing-live-context")
        def withdrawn():
            publisher = self.act(alice, room, "live-context-status")
            follower = self.act(bob, room, "live-context-status")
            (self.output / "context-withdrawal-observation.json").write_text(
                json.dumps({"publisher": publisher, "follower": follower}, indent=2))
            return publisher.get("sharing") is False and follower.get("following") is False
        packaged.wait_for("Stop withdraws native context", withdrawn, bob[1])
        self.act(alice, room, "open-channel", resource=second)
        time.sleep(0.5)
        report = self.act(bob, room, "live-context-status")
        if report.get("channel") != first.removeprefix("chat.channel:"):
            raise RuntimeError("stopped context still moved the other native app")
        stop = packaged.require_action(bob[0], "presence.follow.stop")
        packaged.invoke(bob[0], stop)
        packaged.wait_for("shell follow explicitly released", lambda: (
            packaged.chrome_state(bob[0]).get("presence", {}).get("following") is None), bob[1])
        self.record("native-stop-sharing-and-stop-following", report=report)

    def voice(self, sessions, room):
        call_channel = None
        for name, session in sessions.items():
            self.act(session, room, "open-workspace")
            if call_channel is None:
                self.act(session, room, "start-call")
            else:
                def shared_call():
                    rows = [row for row in self.catalogue(session[0])
                            if row.get("application") == "so.tilo.chat"
                            and row.get("room") == room and row.get("operation") == "join-call"
                            and self.logical_resource(row["resource"]) == f"chat.call:{call_channel}"]
                    if len(rows) > 1:
                        raise RuntimeError("ambiguous shared native call")
                    return rows[0] if rows else None
                row = packaged.wait_for("Alice's actual shared voice channel reaches Bob", shared_call, session[1])
                self.act(session, room, "join-call", resource=row["resource"])

            observed_call = False

            def live():
                nonlocal observed_call
                value = self.act(session, room, "call-status")
                if not isinstance(value, dict) or not isinstance(value.get("reported"), dict):
                    raise RuntimeError("native call state is unobservable")
                snapshot = value["reported"]
                if snapshot.get("state") == "ended":
                    # call-status requests an asynchronous refresh and returns
                    # the preceding snapshot. Before the first service reply,
                    # the production default is ended with no channel/issue.
                    # This is pending, never a successful join; the bounded
                    # wait still fails if no current live reply arrives.
                    if not observed_call and value.get("channel") is None and snapshot.get("issue") is None:
                        return None
                    (self.output / f"{name}-voice-failure.json").write_text(json.dumps(value, indent=2))
                    raise RuntimeError(f"native call failed: {snapshot.get('issue')}")
                observed_call = True
                return snapshot if snapshot.get("state") == "live" else None

            snapshot = packaged.wait_for("native governed voice connected", live, session[1])
            if snapshot.get("muted") is not True or snapshot.get("capture_active") is not False:
                raise RuntimeError("joining acquired capture without fresh consent")
            channel = self.act(session, room, "call-status").get("channel")
            if not isinstance(channel, str) or not channel or (call_channel is not None and channel != call_channel):
                raise RuntimeError("native peers joined different voice channels")
            call_channel = channel
            self.record(f"{name}-native-call-listening-muted", channel=channel, report=snapshot)
        for name, session in sessions.items():
            rows = json.loads(subprocess.check_output(["pw-dump"], env=self.env(name), timeout=3))
            nodes = [row.get("info", {}).get("props", {}) for row in rows
                     if row.get("type") == "PipeWire:Interface:Node"
                     and row.get("info", {}).get("props", {}).get("media.class") in {"Audio/Source", "Audio/Sink"}]
            self.encoder_preflight(name, self.env(name), nodes, phase="muted-call-generated-only")
            self.unmute(name, session, room)
        for name, frequency in (("alice", 1100), ("bob", 700)):
            self.measure(name, frequency, sessions[name], room)
        self.mute_and_resume("bob", sessions["bob"], room)
        self.measure("alice", 1100, sessions["alice"], room)
        return call_channel

    def mute_and_resume(self, name, session, room):
        self.require_audio_streams(name, session, room)
        self.act(session, room, "mute")

        def muted():
            report = self.act(session, room, "call-status")["reported"]
            return report if report.get("state") == "live" and report.get("muted") is True and report.get("capture_active") is False else None

        report = packaged.wait_for("explicit Mute stops native capture", muted, session[1])
        packaged.wait_for("Mute retains only native playback", lambda: {
            node["media.class"] for node in self.audio_streams(name, session)
        } == {"Stream/Output/Audio"}, session[1])
        if packaged.chrome_state(session[0]).get("pending_grants_count") != 0:
            raise RuntimeError("Mute unexpectedly requested permission")
        self.record(f"{name}-mute-releases-capture-and-retains-playback", report=report)
        require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-muted.png")}))
        self.unmute(name, session, room)
        self.require_audio_streams(name, session, room)
        self.record(f"{name}-unmute-requires-fresh-visible-consent")

    def unmute(self, name, session, room):
        socket, process = session
        if packaged.chrome_state(socket).get("pending_grants_count") != 0:
            raise RuntimeError("unrelated permission request precedes fixture consent")
        # The preceding peer's unmute changes the governed publisher roster
        # and may reconnect this carrier. The real user can request unmute
        # again when live; no permission refusal is retried or approved here.
        last_request = 0.0
        def consent_pending():
            nonlocal last_request
            value = self.act(session, room, "call-status")
            pending = packaged.chrome_state(socket).get("pending_grants_count")
            (self.output / f"{name}-consent-observation.json").write_text(json.dumps({
                "call": value, "pending_grants_count": pending,
            }, indent=2))
            if value["reported"].get("state") == "ended":
                raise RuntimeError(f"native call ended before consent: {value['reported'].get('issue')}, client: {value.get('clientIssue')}")
            if (pending == 0 and value["reported"].get("state") == "live"
                    and value["reported"].get("issue") is None
                    and value.get("clientIssue") is None
                    and time.monotonic() - last_request >= 1):
                self.act(session, room, "unmute")
                last_request = time.monotonic()
            return pending == 1

        packaged.wait_for("fresh one-shot capture consent", consent_pending, process)
        before = self.act(session, room, "call-status")["reported"]
        if before.get("capture_active") is not False or before.get("muted") is not True:
            raise RuntimeError("unmute started capture before explicit consent")
        require_ok(call(socket, {"op": "snapshot", "path": str(self.output / f"{name}-consent.png")}))
        require_ok(call(socket, {"op": "click_named", "name": "permission_prompt_allow_once"}))

        def capturing():
            report = self.act(session, room, "call-status")["reported"]
            (self.output / f"{name}-capture-observation.json").write_text(json.dumps({
                "reported": report,
                "pending_grants_count": packaged.chrome_state(socket).get("pending_grants_count"),
                "owned_audio_streams": [{key: node.get(key) for key in
                    ("media.class", "node.name", "object.serial", "target.object")}
                    for node in self.audio_streams(name, session)],
            }, indent=2))
            if report.get("state") == "ended":
                raise RuntimeError(f"native capture ended after consent: {report.get('issue')}")
            return report if report.get("capture_active") is True and report.get("muted") is False else None

        try:
            report = packaged.wait_for("current-generation native capture", capturing, process)
        except Exception:
            require_ok(call(socket, {"op": "snapshot", "path": str(self.output / f"{name}-capture-failed.png")}))
            raise
        self.record(f"{name}-explicit-generated-source-consent", report=report)

    def measure(self, name, frequency, session, room):
        path = self.output / f"{name}-received.raw"
        # pw-cat 1.6.9 exits nonzero at its recording sample limit because
        # recording never marks playback drained. Observe this private sink
        # through a bounded EOS pipeline instead; retain strict exit/count checks.
        result = subprocess.run([
            "gst-launch-1.0", "-q", "pipewiresrc", "target-object=fixture-sink",
            "stream-properties=properties,stream.capture.sink=true", "!",
            "audioconvert", "!", "audioresample", "!",
            "audio/x-raw,format=S16LE,rate=48000,channels=1", "!",
            "audiobuffersplit", "output-buffer-duration=1/50", "strict-buffer-size=true", "!",
            "identity", "eos-after=101", "!", "filesink", f"location={path}",
        ], env=self.env(name), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=8)
        diagnostic = result.stderr.decode(errors="replace").replace(
            str(self.root), "<fixture>").replace(str(self.output), "<proof>")
        diagnostic = re.sub(r"\x1b\[[0-9;]*m", "", diagnostic)
        diagnostic = re.sub(r"0x[0-9a-fA-F]+", "<pointer>", diagnostic)
        (self.output / f"{name}-playback-observation.json").write_text(json.dumps({
            "returncode": result.returncode, "diagnostic": diagnostic[:512],
            "call": self.act(session, room, "call-status"),
            "owned_audio_streams": [{"media.class": node.get("media.class")}
                                    for node in self.audio_streams(name, session)],
        }, indent=2))
        if result.returncode:
            raise RuntimeError("isolated playback observation failed")
        samples = array.array("h", path.read_bytes())
        if len(samples) != 96000:
            raise RuntimeError("isolated playback observation is incomplete")
        coefficient = 2 * math.cos(math.tau * frequency / 48000)
        tone_energy = energy = 0.0
        # Measure the exact expected tone in each 20ms output frame. Native
        # drift correction/PLC changes phase; a global two-second coherent
        # bin would mistake that intentional phase change for missing audio.
        for offset in range(0, len(samples), 960):
            s1 = s2 = 0.0
            frame = samples[offset:offset + 960]
            for sample in frame:
                value = sample / 32768
                energy += value * value
                s0 = value + coefficient * s1 - s2
                s2, s1 = s1, s0
            tone_energy += 2 * (s1*s1+s2*s2-coefficient*s1*s2) / len(frame)
        share = 0.0 if energy < 1e-4 else tone_energy / energy
        (self.output / f"{name}-playback-analysis.json").write_text(json.dumps({
            "samples": len(samples), "frequency_hz": frequency, "tone_share": share,
            "rms": math.sqrt(energy / len(samples)),
        }, indent=2))
        if share < 0.5:
            raise RuntimeError(f"{name} did not receive the other native peer's generated {frequency} Hz tone")
        self.record(f"{name}-decoded-remote-tone", frequency_hz=frequency, tone_share=share,
                    samples=len(samples), rms=math.sqrt(energy / len(samples)))

    def audio_streams(self, name, session):
        rows = json.loads(subprocess.check_output(["pw-dump"], env=self.env(name), timeout=3))
        if not isinstance(rows, list) or not rows:
            raise RuntimeError("private native audio graph is unobservable")
        def numeric(value):
            if isinstance(value, bool):
                return None
            if isinstance(value, int):
                return value if value > 0 else None
            if isinstance(value, str) and value.isascii() and value.isdecimal() and len(value) <= 20:
                return int(value)
            return None

        clients = {numeric(row.get("id")): numeric(row.get("info", {}).get("props", {}).get(
            "application.process.id")) for row in rows if row.get("type") == "PipeWire:Interface:Client"}
        owned = []
        for row in rows:
            node = row.get("info", {}).get("props", {})
            if row.get("type") != "PipeWire:Interface:Node" or node.get("media.class") not in {
                    "Stream/Input/Audio", "Stream/Output/Audio"}:
                continue
            process_id = numeric(node.get("application.process.id"))
            if process_id is None:
                process_id = clients.get(numeric(node.get("client.id")))
            if process_id is None:
                raise RuntimeError("private audio stream owner is unobservable")
            if process_id == session[1].pid:
                owned.append(node)
        return owned

    def no_audio_streams(self, name, session):
        return not self.audio_streams(name, session)

    def require_audio_streams(self, name, session, room):
        try:
            packaged.wait_for("owned capture and playback streams are observable", lambda: {
                node["media.class"] for node in self.audio_streams(name, session)
            } == {"Stream/Input/Audio", "Stream/Output/Audio"}, session[1])
        except Exception:
            observation = {
                "call": self.act(session, room, "call-status"),
                "owned_audio_streams": [{"media.class": node.get("media.class")}
                                        for node in self.audio_streams(name, session)],
            }
            (self.output / f"{name}-leave-precondition-failure.json").write_text(json.dumps(observation, indent=2))
            raise

    def leave_and_revoke(self, sessions, room, host, channel):
        for name, session in sessions.items():
            self.require_audio_streams(name, session, room)
            self.act(session, room, "leave-call")
            def left():
                report = self.act(session, room, "call-status")["reported"]
                return report if report.get("state") in {"idle", "ended"} and report.get("capture_active") is False else None
            report = packaged.wait_for("explicit Leave releases the call", left, session[1])
            packaged.wait_for("Leave removes native audio streams", lambda: self.no_audio_streams(name, session), session[1])
            self.record(f"{name}-leave-releases-capture-and-playback", report=report)
            require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-left.png")}))
        # Rejoin the same generated call with a new explicit one-shot consent.
        for name, session in sessions.items():
            rows = [row for row in self.catalogue(session[0])
                    if row.get("application") == "so.tilo.chat" and row.get("room") == room
                    and row.get("operation") == "join-call"
                    and self.logical_resource(row["resource"]) == f"chat.call:{channel}"]
            if len(rows) != 1:
                raise RuntimeError("same shared call is unobservable on rejoin")
            self.act(session, room, "join-call", resource=rows[0]["resource"])
            report = packaged.wait_for("new native call generation", lambda: (
                report if (report := self.act(session, room, "call-status")["reported"]).get("state") == "live"
                else None), session[1])
            if report.get("muted") is not True or report.get("capture_active") is not False:
                raise RuntimeError("rejoining reused microphone consent")
            self.record(f"{name}-native-rejoined-listening-muted", report=report)
            self.unmute(name, session, room)
        self.measure("alice", 1100, sessions["alice"], room)
        self.measure("bob", 700, sessions["bob"], room)
        self.require_audio_streams("bob", sessions["bob"], room)
        host.stdin.write("revoke\n")
        host.stdin.flush()
        packaged.wait_for("host commits Bob removal", lambda: "revoked" in (self.output / "host.log").read_text(), host)
        bob = sessions["bob"]
        packaged.wait_for("revocation closes Bob capture and playback", lambda: self.no_audio_streams("bob", bob), bob[1])
        self.record("governor-revocation-removes-native-audio-streams")
        require_ok(call(bob[0], {"op": "snapshot", "path": str(self.output / "bob-revoked.png")}))
        self.act(sessions["alice"], room, "leave-call")
        packaged.wait_for("remaining participant leaves", lambda: self.no_audio_streams("alice", sessions["alice"]), sessions["alice"][1])

    def app_state(self, sessions, room, early, authors):
        channels, shared_reference = self.shared_state(sessions, room)
        texts = ["Fixture committed before Bob app launch",
                 "Fixture concurrent Alice", "Fixture concurrent Bob"]
        observations = {}
        expected_authors = dict(zip(texts, (authors[0], authors[0], authors[1])))
        def readable():
            for name, session in sessions.items():
                resources = channels(session[0])
                if early not in resources:
                    return False
                page = self.act(session, room, "read-messages", {"pageSize": 32},
                                resource=early)
                content = page.get("content", {})
                if content.get("channel") != early.removeprefix("chat.channel:"):
                    raise RuntimeError("late app read another record")
                messages = content.get("messages", [])
                if any(sum(message.get("text") == text and message.get("complete") is True
                           for message in messages) != 1 for text in texts):
                    return False
                observed = [message for message in messages if message.get("text") in expected_authors]
                if any(message.get("author") != expected_authors[message["text"]] for message in observed):
                    raise RuntimeError("native message attribution differs from its certified invoker")
                observations[name] = observed
            return True
        packaged.wait_for("late app receives early channel", lambda: early in channels(sessions["bob"][0]),
                          sessions["bob"][1])
        # Bob's ordinary signed app was launched only after Alice's first
        # commit. Submit subsequent edits from both native app processes.
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(self.act, session, room, "post", {"value": text},
                                   early)
                       for session, text in zip(sessions.values(), texts[1:])]
            for future in futures:
                future.result()
        packaged.wait_for("late app and concurrent edits converge", readable, sessions["bob"][1])
        self.record("late-native-app-and-concurrent-resource-state-converged", channel=early,
                    messages=observations)
        for name, session in sessions.items():
            before = self.presented_tab(session[0], self.native_tabs[name])
            if before is None:
                raise RuntimeError("native app disappeared before state rendering")
            resource = packaged.wait_for("current exact channel navigation action",
                                         lambda: channels(session[0]).get(early), session[1])
            self.act(session, room, "open-channel", resource=resource)
            def painted():
                frame = self.presented_tab(session[0], self.native_tabs[name])
                return frame if frame and frame["frame_seq"] > before["frame_seq"] else None
            frame = packaged.wait_for("local app paints after shared channel navigation", painted, session[1])
            require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-shared-state.png")}))
            self.record(f"{name}-shared-state-locally-presented", frame_seq=frame["frame_seq"])
        return shared_reference

    def save_automatic_failure(self, socket):
        state = call(socket, {"op": "state_dump"}).get("state", {})
        observations = {"frames": packaged.foreign_frames(call(socket, {"op": "foreign_frames"})),
                        "workspaces": state.get("workspace_tree", {}).get("workspaces"),
                        "peer_sync": state.get("chrome", {}).get("peer_sync")}
        (self.output / "bob-automatic-opening.json").write_text(json.dumps(observations, indent=2))
        require_ok(call(socket, {"op": "snapshot", "path": str(self.output / "bob-automatic-opening.png")}))

    def reconnect(self, sessions, bootstrap, host, early):
        identity_mode = "same-person" if bootstrap["users"][0] == bootstrap["users"][1] else "two-person"
        # Only the disposable host this fixture spawned is stopped. Its keys,
        # room, app workers and grants are unchanged; no revocation is minted.
        if host not in self.processes or host.poll() is not None:
            raise RuntimeError("private governor custody is unavailable")
        os.kill(host.pid, signal.SIGSTOP)
        try:
            self.record("owned-governor-process-paused-without-revocation")
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                time.sleep(0.1)
                if host.poll() is not None:
                    raise RuntimeError("governor exited during transport partition")
            status = pathlib.Path(f"/proc/{host.pid}/status").read_text()
            if not any(line.startswith("State:") and "T (stopped)" in line for line in status.splitlines()):
                raise RuntimeError("private host transport partition cannot be observed")
            expected = {}
            for index, (name, session) in enumerate(sessions.items()):
                rows = [row for row in self.catalogue(session[0])
                        if row.get("application") == "so.tilo.chat" and row.get("room") == bootstrap["room"]
                        and row.get("operation") == "post" and self.logical_resource(row["resource"]) == early]
                if len(rows) != 1:
                    raise RuntimeError("offline app lost its exact resource action")
                marker = f"Fixture offline {name}"
                self.act(session, bootstrap["room"], "post", {"value": marker}, resource=rows[0]["resource"])
                expected[name] = {"text": marker, "author": bootstrap["users"][index]}
            self.record(f"both-{identity_mode}-native-apps-edited-while-host-stopped", messages=expected,
                        peer_status={name: packaged.chrome_state(session[0]).get("peer_sync", {}).get("status")
                                     for name, session in sessions.items()})
        finally:
            if host.poll() is None:
                os.kill(host.pid, signal.SIGCONT)
        observations = {}
        def converged():
            for name, session in sessions.items():
                rows = [row for row in self.catalogue(session[0])
                        if row.get("application") == "so.tilo.chat" and row.get("room") == bootstrap["room"]
                        and row.get("operation") == "read-messages" and self.logical_resource(row["resource"]) == early]
                if len(rows) != 1:
                    return False
                page = self.act(session, bootstrap["room"], "read-messages", {"pageSize": 32}, resource=rows[0]["resource"])
                content = page.get("content", {})
                if content.get("channel") != early.removeprefix("chat.channel:"):
                    raise RuntimeError("reconnected app read another record")
                messages = content.get("messages", [])
                if any(sum(m.get("text") == marker["text"] and m.get("complete") is True for m in messages) != 1
                       for marker in expected.values()):
                    return False
                actual = [m for m in messages if m.get("text") in {marker["text"] for marker in expected.values()}]
                if any(m.get("author") != marker["author"] for marker in expected.values()
                       for m in actual if m.get("text") == marker["text"]):
                    raise RuntimeError("offline message lost its certified author")
                observations[name] = actual
            return True
        packaged.wait_for("offline edits rejoin the same record without duplication", converged,
                          sessions["bob"][1], timeout=90)
        self.record(f"{identity_mode}-offline-edits-converged-after-native-reconnect", messages=observations)

    def create_workspace(self, sessions, bootstrap):
        socket, process = sessions["alice"]
        def tree(session):
            return call(session[0], {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
        before = tree(sessions["alice"])
        originals = {row["id"]: row for row in before.get("workspaces", [])}
        governed = next(row for row in originals.values() if row.get("room") == bootstrap["room"])
        actor = packaged.chrome_state(socket).get("local_user_id")
        (self.output / "workspace-create-authority.json").write_text(json.dumps({
            "actor": actor, "expected_certified_actor": bootstrap["users"][0],
            "active": before.get("active"), "governed_room": governed.get("room"),
            "governed_workspace": governed.get("id"),
            "active_workspace": {key: value for key, value in
                next((row for row in originals.values() if row.get("id") == before.get("active")), {}).items()
                if key in {"id", "room", "owner"}},
            "owner": governed.get("owner"),
            "capabilities": governed.get("capabilities"),
            "write_decision": call(socket, {"op": "check_action", "actor": f"person:{actor}",
                "app": "shell", "resource": "shell", "action": "workspace.create", "verb": "Write"}),
        }, indent=2))
        if actor != bootstrap["users"][0]:
            raise RuntimeError("workspace creation actor differs from the provisioned certified local person")
        # A shared space can publish more than the default first page.
        # Resolve the exact shell action from an observed complete catalogue.
        default_matches = [row for row in self.catalogue(socket) if row["id"] == "workspace.create"]
        matches = [row for row in self.catalogue(socket, "workspace") if row["id"] == "workspace.create"]
        self.record("workspace-creation-discovery", default_matches=len(default_matches),
                    searched_matches=len(matches))
        if len(default_matches) != 1 or len(matches) != 1:
            raise RuntimeError("workspace.create must appear exactly once in both complete and searched catalogues")
        action = default_matches[0]
        packaged.wait_terminal(socket, packaged.invoke(socket, action, {"name": "Fixture independent workspace"}), process)
        def created():
            current = tree(sessions["alice"])
            additions = [row for row in current.get("workspaces", []) if row["id"] not in originals]
            if not additions:
                return None
            if len(additions) != 1 or len(current.get("workspaces", [])) != len(originals) + 1:
                raise RuntimeError("workspace action did not create exactly one independent workspace")
            workspace = additions[0]
            if (workspace.get("room") == bootstrap["room"] or current.get("active") != workspace["id"]
                    or workspace.get("name") != "Fixture independent workspace"
                    or workspace.get("owner") != bootstrap["users"][0]):
                raise RuntimeError("created workspace lacks its own active room and certified local owner")
            if workspace.get("collaborators") or any(strip.get("window_groups") for strip in workspace.get("strips", [])):
                raise RuntimeError("new workspace copied presence or seeded applications")
            capabilities = workspace.get("capabilities", [])
            if len(capabilities) != 1 or any(cap.get("id") in {old.get("id") for old in governed.get("capabilities", [])}
                                             for cap in capabilities):
                raise RuntimeError("new workspace copied permissions from the governed room")
            preserved = next(row for row in current["workspaces"] if row["id"] == governed["id"])
            if any(preserved.get(key) != governed.get(key) for key in ("room", "owner", "capabilities")):
                raise RuntimeError("workspace creation changed governed room authority")
            if any(row.get("room") == workspace["room"] for row in tree(sessions["bob"]).get("workspaces", [])):
                raise RuntimeError("another person was automatically admitted to the new workspace")
            return {key: workspace[key] for key in ("id", "room", "name", "owner")}
        observation = packaged.wait_for("participant creates an independent empty workspace", created, process)
        self.record("native-participant-workspace-created-with-independent-authority", workspace=observation,
                    old_room_authority_preserved=True, peer_automatically_admitted=False)

    def run(self, *, voice_only=False, state_only=False, continuity=None):
        state_only = state_only or continuity is not None
        environments = {name: self.env(name) for name in ("alice", "bob")}
        for name, env in environments.items():
            self.start_private_bus(name, env)
        publisher = self.root / "alice"
        for name, env in environments.items():
            env["TILO_APPS_DATA_DIR"] = str(self.root / name / "apps")
            env["TILO_RUNTIME_BUNDLE_DIR"] = str(publisher / "runtimes")
        _version, native_chat = packaged.install_packages(self.binaries, publisher, environments["alice"])
        self.record("same-source-native-inputs", digests={
            name: packaged.digest_file(path) for name, path in {
                "fixture": pathlib.Path(__file__), "shell": self.binaries / "shell",
                "governor": self.binaries / "examples/native_pair_host",
                "launcher": self.binaries / "tilo-launcher", "native_chat": native_chat,
                "signed_chat": publisher / "chat.tapp",
            }.items()})
        environments["bob"]["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"] = environments["alice"]["TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE"]
        packaged.run(self.binaries / "tilo-install", ["install", publisher / "chat.tapp", "--allow-self-signed"], environments["bob"])
        self.record("ordinary-signed-chat-packages-installed")
        command = [self.binaries / "examples/native_pair_host", self.root]
        if continuity in ("automatic", "reconnect"):
            command.append("--same-person")
        host = self.spawn("host", command, environments["alice"], stdin=subprocess.PIPE, text=True)
        bootstrap_path = self.root / "bootstrap.json"
        packaged.wait_for("private signed host invitations", bootstrap_path.exists, host)
        bootstrap = json.loads(bootstrap_path.read_text())
        same_person = bootstrap["users"][0] == bootstrap["users"][1]
        if same_person != (continuity in ("automatic", "reconnect")):
            raise RuntimeError("certified person identity does not match fixture mode")
        self.record("same-person-independent-certified-devices" if same_person else
                    "two-distinct-certified-native-identities", users=bootstrap["users"])
        if same_person:
            # Keep the production installer's authorization artifact paths intact.
            member_root = self.root / "bob/joined"
            member_identity = member_root / "var/lib/tilo"
            if (member_identity / "user-identity/privkey.bin").exists():
                raise RuntimeError("member received primary personal signing material")
            environments["bob"].update({
                "XDG_CONFIG_HOME": str(member_root / "var/home/bob/.config"),
                "TILO_IDENTITY_ROOT": str(member_identity),
                "TILO_PEER_KEY_PATH": str(member_identity / "device-private/keypair.bin"),
            })
        sessions = {}
        for name, frequency in (("alice", 700), ("bob", 1100)):
            env = environments[name]
            if not state_only:
                self.audio(name, env, frequency)
            command = [self.binaries / "shell", "--mode", "native",
                       "--backend", "headless", "--enable-peer-sync"]
            if self.native_debugger is not None and name == "bob":
                commands = self.output / "bob-debugger.commands"
                commands.write_text("\n".join([
                    "set pagination off", "set confirm off", "set disable-randomization off",
                    "set follow-fork-mode parent", "set detach-on-fork on",
                    "set print frame-arguments none", "set print entry-values no",
                    "handle SIGPIPE nostop noprint pass", "handle SIGCHLD nostop noprint pass",
                    "set logging file " + str(self.output / "bob-native-stacks.txt"),
                    "set logging overwrite on", "set logging redirect on", "set logging enabled on",
                    "run", "thread apply all bt 12", "quit", "",
                ]))
                commands.chmod(0o600)
                command = [self.native_debugger, "--batch", "--nx", "--quiet",
                           "-iex", "set auto-load off", "-iex", "set debuginfod enabled off",
                           "-x", commands, "--args", *command]
            shell = self.spawn(name, command, env)
            socket = env["TILO_TEST_CONTROL"]
            packaged.wait_for(f"{name} native control socket", lambda: pathlib.Path(socket).exists(), shell)
            packaged.invoke(socket, packaged.require_action(socket, "peer.connect", "connect"))
            packaged.wait_for("Connect prompt", lambda: packaged.chrome_state(socket).get(
                "peer_sync", {}).get("connect_input") is not None, shell)
            invitation = bootstrap["invitations"][0 if name == "alice" else 1]
            require_ok(call(socket, {"op": "type", "text": invitation}))
            require_ok(call(socket, {"op": "key", "key": "Enter"}))
            try:
                packaged.wait_for("signed native join", lambda: (
                    packaged.chrome_state(socket).get("peer_sync", {}).get("status") or "").startswith("Joined"), shell)
            except RuntimeError:
                (self.output / f"{name}-join-failure.json").write_text(json.dumps(
                    packaged.chrome_state(socket).get("peer_sync", {}), indent=2))
                raise
            require_ok(call(socket, {"op": "key", "key": "Escape"}))
            self.record(f"{name}-governed-native-join")
            sessions[name] = (socket, shell)
            self.native_processes[socket] = shell
            require_ok(call(socket, {"op": "snapshot", "path": str(self.output / f"{name}-joined.png")}))
        early = None
        if continuity == "automatic" and packaged.presented_primary_frame(
                call(sessions["bob"][0], {"op": "foreign_frames"}), "so.tilo.chat") is not None:
            raise RuntimeError("receiving device opened Chat before the source device")
        for name, (socket, shell) in sessions.items():
            if continuity == "automatic" and name == "bob":
                try:
                    frame = packaged.wait_for("same-person device automatically opens local Chat", lambda:
                        packaged.presented_primary_frame(call(socket, {"op": "foreign_frames"}), "so.tilo.chat"),
                        shell, timeout=90)
                except Exception:
                    self.save_automatic_failure(socket)
                    raise
                tree = call(socket, {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
                tabs = [tab for workspace in tree.get("workspaces", [])
                        for strip in workspace.get("strips", [])
                        for group in strip.get("window_groups", []) for tab in group.get("tabs", [])
                        if tab.get("tile", {}).get("Foreign", {}).get("surface_id") == frame["surface_id"]]
                if len(tabs) != 1:
                    raise RuntimeError("automatic native frame has no exact admitted tab")
                self.native_tabs[name] = tabs[0]["id"]
                self.record("same-person-peer-app-opened-without-second-open-invocation", frame=frame)
                continue
            app = packaged.wait_for("signed Chat offering", lambda: next((
                row for row in packaged.actions(socket, "Chat")
                if row.get("provider") == "applications" and row.get("id") == "so.tilo.chat"), None), shell)
            (self.output / f"{name}-before-open-process.json").write_text(json.dumps(
                self.native_process_evidence(shell, socket), indent=2))
            before_tree = call(socket, {"op": "state_dump"}).get("state", {}).get("workspace_tree", {})
            previous = {tab["id"] for workspace in before_tree.get("workspaces", [])
                        for strip in workspace.get("strips", [])
                        for group in strip.get("window_groups", []) for tab in group.get("tabs", [])}
            receipt = packaged.wait_terminal(socket, packaged.invoke(socket, app), shell,
                                             public_failure_path=self.output / f"{name}-failed-open.json")
            (self.output / f"{name}-open.json").write_text(json.dumps(receipt, indent=2))
            def launched():
                if name not in self.native_tabs:
                    tab = self.focused_tab(socket, bootstrap["room"], previous)
                    if tab is None:
                        return None
                    self.native_tabs[name] = tab
                return self.presented_tab(socket, self.native_tabs[name])
            try:
                frame = packaged.wait_for("exact launched tab presents native Chat", launched, shell)
            except Exception as failure:
                # A dead control socket must not replace the failed acceptance
                # check with a second diagnostic error. Preserve process evidence.
                observations = {"error_type": type(failure).__name__, "error": str(failure),
                                "process_returncode": shell.poll(), "diagnostic_errors": {}}
                self.record(f"{name}-native-chat-presentation-failed",
                            process_returncode=observations["process_returncode"],
                            error_type=observations["error_type"])
                checks = {
                    "frames": lambda: call(socket, {"op": "foreign_frames"}),
                    "notifications": lambda: packaged.chrome_state(socket).get("notifications"),
                    "actions": lambda: [packaged.public_action_observation(row)
                                        for row in self.catalogue(socket)],
                    "snapshot": lambda: require_ok(call(socket, {"op": "snapshot", "path": str(
                        self.output / f"{name}-failed-launch.png")})),
                }
                for kind, observe in checks.items():
                    try:
                        value = observe()
                        if kind != "snapshot":
                            (self.output / f"{name}-{kind}.json").write_text(json.dumps(value, indent=2))
                    except Exception as diagnostic_failure:
                        observations["diagnostic_errors"][kind] = {
                            "type": type(diagnostic_failure).__name__, "error": str(diagnostic_failure)}
                observations["native_process"] = self.native_process_evidence(shell, socket)
                diagnosis = self.output / f"{name}-failure"
                diagnosis.mkdir(mode=0o700)
                try:
                    packaged.diagnose_owned_shell(shell, diagnosis)
                except Exception as diagnostic_failure:
                    observations["diagnostic_errors"]["owned-process"] = {
                        "type": type(diagnostic_failure).__name__, "error": str(diagnostic_failure)}
                observations["process_returncode"] = shell.poll()
                observations["run_seconds_override"] = environments[name].get("TILO_RUN_SECONDS")
                (self.output / f"{name}-presentation-failure.json").write_text(json.dumps(observations, indent=2))
                raise
            self.record(f"{name}-signed-native-chat-presented", tab=self.native_tabs[name],
                        surface_id=frame["surface_id"])
            (self.output / f"{name}-actions.json").write_text(json.dumps(
                [packaged.public_action_observation(row) for row in packaged.actions(socket)], indent=2))
            require_ok(call(socket, {"op": "snapshot", "path": str(self.output / f"{name}-chat.png")}))
            if state_only and name == "alice":
                before = {self.logical_resource(row["resource"]) for row in self.catalogue(socket)
                          if row.get("application") == "so.tilo.chat"
                          and row.get("room") == bootstrap["room"] and row.get("operation") == "post"}
                self.act(sessions[name], bootstrap["room"], "new-channel",
                         {"value": "Fixture before second app launch"})
                row = packaged.wait_for("new early channel", lambda: next((
                    row for row in self.catalogue(socket)
                    if row.get("application") == "so.tilo.chat"
                    and row.get("room") == bootstrap["room"]
                    and row.get("operation") == "post" and self.logical_resource(row["resource"]) not in before), None), shell)
                early = self.logical_resource(row["resource"])
                self.act(sessions[name], bootstrap["room"], "post",
                         {"value": "Fixture committed before Bob app launch"}, resource=row["resource"])
                self.record("alice-state-committed-before-bob-app-launch", channel=early)
        if state_only:
            shared_reference = self.app_state(sessions, bootstrap["room"], early, bootstrap["users"])
            if continuity in ("reconnect", "two-person-reconnect"):
                self.reconnect(sessions, bootstrap, host, early)
                packaged.wait_for("shared resource survives native reconnect", shared_reference, sessions["bob"][1])
                self.record("native-live-resource-reference-readable-after-reconnect")
            self.create_workspace(sessions, bootstrap)
            return
        if not voice_only:
            self.context(sessions, bootstrap["room"], bootstrap["presence_ids"])
        channel = self.voice(sessions, bootstrap["room"])
        self.leave_and_revoke(sessions, bootstrap["room"], host, channel)
        self.create_workspace(sessions, bootstrap)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--binaries", type=pathlib.Path, default=REPO / "target/debug")
    parser.add_argument("--pipewire", type=pathlib.Path,
                        help="private extracted PipeWire server/config/plugin payload")
    parser.add_argument("--native-debugger", type=pathlib.Path,
                        help="diagnose only Bob native startup under an existing debugger; original deadlines and assertions remain")
    parser.add_argument("--cleanup-proof", action="store_true", help="test exact private daemon cleanup without launching Chat")
    parser.add_argument("--voice-proof", action="store_true", help="test governed native voice without qualifying follow or the combined pair")
    parser.add_argument("--routing-diagnostics", action="store_true",
                        help="retain existing relay phase diagnostics in this fixture's private logs")
    parser.add_argument("--state-proof", action="store_true",
                        help="qualify native app state independently of voice and following")
    parser.add_argument("--automatic-opening-proof", action="store_true",
                        help="test automatic native app opening on an enrolled same-person device")
    parser.add_argument("--reconnect-proof", action="store_true",
                        help="test same-person native apps across a private governor transport partition")
    parser.add_argument("--two-person-reconnect-proof", action="store_true",
                        help="test distinct certified people sharing channels and resources across a private governor transport partition")
    parser.add_argument("--font-namespace", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    continuity = "automatic" if args.automatic_opening_proof else "reconnect" if args.reconnect_proof else "two-person-reconnect" if args.two_person_reconnect_proof else None
    state_mode = args.state_proof or continuity is not None
    if sum((args.cleanup_proof, args.voice_proof, args.state_proof,
            args.automatic_opening_proof, args.reconnect_proof, args.two_person_reconnect_proof)) > 1:
        parser.error("choose only one isolated proof mode")
    if not state_mode and not args.cleanup_proof and args.pipewire is None:
        parser.error("voice requires --pipewire")
    if not args.cleanup_proof and not args.font_namespace:
        # Reuse the existing packaged-native proof's OS font-catalog stage.
        # Only this fixture's mount namespace changes; host fonts stay intact.
        with tempfile.TemporaryDirectory(prefix="tilo-state-fonts-") as scratch:
            fonts = pathlib.Path(scratch) / "catalog"
            subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"),
                            "prepare", str(REPO / "crates/sdk/ui-core/assets/fonts"),
                            str(fonts), "--scope", "os"], check=True)
            subprocess.run(["bwrap", "--die-with-parent", "--bind", "/", "/",
                            "--dev-bind", "/dev", "/dev", "--tmpfs", "/usr/share/fonts",
                            "--ro-bind", str(fonts), "/usr/share/fonts/tilo", "--",
                            sys.executable, "-B", str(pathlib.Path(__file__).resolve()),
                            *sys.argv[1:], "--font-namespace"], check=True)
        return
    if not args.cleanup_proof:
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"),
                        "verify", "/usr/share/fonts/tilo", "--scope", "os"], check=True)
        mounts = pathlib.Path("/proc/self/mountinfo").read_text().splitlines()
        if not any(row.split()[4] == "/usr/share/fonts/tilo"
                   and "ro" in row.split()[5].split(",") for row in mounts):
            raise RuntimeError("canonical OS font namespace is not read-only")
    args.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    fixture = Fixture(args.out.resolve(), args.binaries.resolve(), args.pipewire.resolve() if args.pipewire else None,
                      routing_diagnostics=args.routing_diagnostics)
    if args.native_debugger is not None:
        fixture.native_debugger = args.native_debugger.resolve()
        if not fixture.native_debugger.is_file() or not os.access(fixture.native_debugger, os.X_OK):
            raise RuntimeError("native debugger is not an existing executable")
        if any(char in str(fixture.output) for char in ("\n", "\r")):
            raise RuntimeError("native debugger output path is not a single line")
    qualified, state_qualified, error = False, False, None
    try:
        if args.cleanup_proof:
            fixture.cleanup_proof()
        else:
            fixture.run(voice_only=args.voice_proof, state_only=state_mode, continuity=continuity)
            state_qualified = state_mode
            qualified = not args.voice_proof and not state_mode
    except Exception as failure:
        error = str(failure)
        raise
    finally:
        cleanup_error = None
        try:
            fixture.close()
        except Exception as failure:
            qualified = state_qualified = False
            cleanup_error = str(failure)
        (args.out / "journey.json").write_text(json.dumps({
            "qualified": qualified, "app_state_qualified": state_qualified,
            "mode": "two-person-reconnect-proof" if args.two_person_reconnect_proof else f"same-person-{continuity}-proof" if continuity else "native-app-state-proof" if args.state_proof else "cleanup-proof" if args.cleanup_proof else "native-voice-proof" if args.voice_proof else "native-pair",
            "error": error, "cleanup_error": cleanup_error, "stages": fixture.stages,
            "real_microphone": False, "video_screen_sharing": False,
            "routing_diagnostics": args.routing_diagnostics,
            "native_debugger": args.native_debugger is not None,
            "participant_workspace_creation_qualified": (qualified or state_qualified or args.voice_proof and error is None)
                and cleanup_error is None and any(stage["stage"] == "native-participant-workspace-created-with-independent-authority"
                                                  for stage in fixture.stages),
            "same_person_devices_qualified": state_qualified and continuity in ("automatic", "reconnect"),
            "two_person_resource_reconnect_qualified": state_qualified and continuity == "two-person-reconnect",
            "automatic_peer_app_launch_qualified": state_qualified and continuity == "automatic",
            "offline_reconnect_qualified": state_qualified and continuity == "reconnect",
        }, indent=2) + "\n")
        if cleanup_error is not None:
            raise RuntimeError(f"fixture cleanup failed: {cleanup_error}")


if __name__ == "__main__":
    main()
