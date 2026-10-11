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
    def __init__(self, output, binaries, pipewire):
        self.output, self.binaries, self.pipewire = output, binaries, pipewire
        self.root_owner = tempfile.TemporaryDirectory(prefix="tilo-native-pair-")
        self.root = pathlib.Path(self.root_owner.name)
        self.root.chmod(0o700)
        self.processes, self.logs, self.stages = [], [], []

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
               "RUST_LOG": "warn,tilo_joined_room::voice=debug,tilo_media::voice::gst=debug,shell::network::native_media=debug,shell::shell::native_media=debug,tilo_action_broker::collab_broker_capnp::native_media=debug",
               "GST_DEBUG": "pipewiresrc:6,pipewirecore:5,appsink:5,opusenc:5,basesink:5,basesrc:6"}
        if self.pipewire is not None:
            env["GST_PLUGIN_PATH"] = str(self.pipewire / "usr/lib64/gstreamer-1.0")
        else:
            env.pop("GST_PLUGIN_PATH", None)
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS",
                    "PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR", "TILO_COLLAB_HOST",
                    "TILO_COLLAB_HOSTS", "TILO_APPS_SYSTEM_DIR"):
            env.pop(key, None)
        return env

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
        self.spawn(f"{name}-wireplumber", ["dbus-run-session", "--", "wireplumber"], env)
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
        # Launcher/SDK helpers may fork a bus out of their process group.
        # Exact private profile evidence plus a pidfd bounds cleanup to this run.
        homes = {str(self.root / name / "home").encode() for name in ("alice", "bob")}
        runtimes = {str(self.root / name / "run").encode() for name in ("alice", "bob")}
        stopped = 0
        for process in pathlib.Path("/proc").iterdir():
            if not process.name.isdigit():
                continue
            descriptor = None
            try:
                if process.stat().st_uid != os.getuid() or (process / "comm").read_text().strip() != "dbus-daemon":
                    continue
                descriptor = os.pidfd_open(int(process.name))
                env = dict(item.split(b"=", 1) for item in (process / "environ").read_bytes().split(b"\0") if b"=" in item)
                if env.get(b"HOME") not in homes or env.get(b"XDG_RUNTIME_DIR") not in runtimes:
                    continue
                signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                poller = select.poll()
                poller.register(descriptor, select.POLLIN)
                if not poller.poll(3000):
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                    if not poller.poll(3000):
                        raise RuntimeError("owned private bus did not exit")
                stopped += 1
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
                for env in (owned, adjacent):
                    pid = int(subprocess.check_output([
                        "dbus-daemon", "--session", "--fork", "--print-pid",
                        f"--address=unix:path={env['XDG_RUNTIME_DIR']}/fixture-bus",
                    ], env=env, timeout=3))
                    descriptors.append(os.pidfd_open(pid))
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
                    and (resource is None or row.get("resource") == resource)]
            if len(rows) > 1:
                raise RuntimeError(f"ambiguous Chat operation {operation}")
            return rows[0] if rows else None
        return packaged.wait_for(f"Chat {operation}", offered, process)

    @staticmethod
    def catalogue(socket):
        response = call(socket, {"op": "actions_enumerate", "include_programmatic": True})
        if "err" in response or not isinstance(response.get("answer", {}).get("actions"), list):
            raise RuntimeError("complete native action catalogue is unavailable")
        return response["answer"]["actions"]

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
        receipt = packaged.wait_terminal(socket, packaged.invoke(socket, row, args), process,
                                        public_failure_path=self.output / f"failed-{operation}.json")
        return receipt.get("update", {}).get("output", {}).get("value")

    def shared_state(self, sessions, room):
        alice, bob = sessions["alice"], sessions["bob"]
        for session in (alice, bob):
            report = self.act(session, room, "live-context-status")
            if report.get("sharing") is not False or report.get("following") is not False or report.get("videoCapture") is not False:
                raise RuntimeError("native Chat did not start with private context")
        self.act(alice, room, "new-channel", {"value": "Fixture second conversation"})

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
                     resource=text_channels(session[0])[logical])
        def readable_conversations():
            for session in (alice, bob):
                resources = text_channels(session[0])
                for logical, text in expected.items():
                    page = self.act(session, room, "read-messages", {"pageSize": 32},
                                    resource=resources[logical])
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
            "sourceProvider": "record:so.tilo.chat:"}, resource=channels[first])
        def shared_reference():
            page = self.act(bob, room, "read-messages", {"pageSize": 32},
                            resource=text_channels(bob[0])[first])
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
        return text_channels

    def context(self, sessions, room, presence_ids):
        text_channels = self.shared_state(sessions, room)
        alice, bob = sessions["alice"], sessions["bob"]
        channels = text_channels(alice[0])
        first, second = sorted(channels)[:2]
        self.act(alice, room, "open-channel", resource=channels[first])
        self.act(bob, room, "open-channel", resource=text_channels(bob[0])[second])
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
        self.act(alice, room, "open-channel", resource=channels[second])
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
        channels = self.shared_state(sessions, room)
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
                                resource=resources[early])
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
                                   channels(session[0])[early])
                       for session, text in zip(sessions.values(), texts[1:])]
            for future in futures:
                future.result()
        packaged.wait_for("late app and concurrent edits converge", readable, sessions["bob"][1])
        self.record("late-native-app-and-concurrent-resource-state-converged", channel=early,
                    messages=observations)
        for name, session in sessions.items():
            before = packaged.presented_primary_frame(call(session[0], {"op": "foreign_frames"}), "so.tilo.chat")
            if before is None:
                raise RuntimeError("native app disappeared before state rendering")
            self.act(session, room, "open-channel", resource=channels(session[0])[early])
            def painted():
                frame = packaged.presented_primary_frame(call(session[0], {"op": "foreign_frames"}), "so.tilo.chat")
                return frame if frame and frame["frame_seq"] > before["frame_seq"] else None
            frame = packaged.wait_for("local app paints after shared channel navigation", painted, session[1])
            require_ok(call(session[0], {"op": "snapshot", "path": str(self.output / f"{name}-shared-state.png")}))
            self.record(f"{name}-shared-state-locally-presented", frame_seq=frame["frame_seq"])

    def save_automatic_failure(self, socket):
        state = call(socket, {"op": "state_dump"}).get("state", {})
        observations = {"frames": packaged.foreign_frames(call(socket, {"op": "foreign_frames"})),
                        "workspaces": state.get("workspace_tree", {}).get("workspaces"),
                        "peer_sync": state.get("chrome", {}).get("peer_sync")}
        (self.output / "bob-automatic-opening.json").write_text(json.dumps(observations, indent=2))
        require_ok(call(socket, {"op": "snapshot", "path": str(self.output / "bob-automatic-opening.png")}))

    def reconnect(self, sessions, bootstrap, host, early):
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
            self.record("both-same-person-native-apps-edited-while-host-stopped", messages=expected,
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
        self.record("same-person-offline-edits-converged-after-native-reconnect", messages=observations)

    def run(self, *, voice_only=False, state_only=False, continuity=None):
        state_only = state_only or continuity is not None
        environments = {name: self.env(name) for name in ("alice", "bob")}
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
        if continuity is not None:
            command.append("--same-person")
        host = self.spawn("host", command, environments["alice"], stdin=subprocess.PIPE, text=True)
        bootstrap_path = self.root / "bootstrap.json"
        packaged.wait_for("private signed host invitations", bootstrap_path.exists, host)
        bootstrap = json.loads(bootstrap_path.read_text())
        same_person = bootstrap["users"][0] == bootstrap["users"][1]
        if same_person != (continuity is not None):
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
            shell = self.spawn(name, [self.binaries / "shell", "--mode", "native",
                                      "--backend", "headless", "--enable-peer-sync"], env)
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
                self.record("same-person-peer-app-opened-without-second-open-invocation", frame=frame)
                continue
            app = packaged.wait_for("signed Chat offering", lambda: next((
                row for row in packaged.actions(socket, "Chat")
                if row.get("provider") == "applications" and row.get("id") == "so.tilo.chat"), None), shell)
            receipt = packaged.wait_terminal(socket, packaged.invoke(socket, app), shell,
                                             public_failure_path=self.output / f"{name}-failed-open.json")
            (self.output / f"{name}-open.json").write_text(json.dumps(receipt, indent=2))
            try:
                packaged.wait_for("presented native Chat", lambda: packaged.presented_primary_frame(
                    call(socket, {"op": "foreign_frames"}), "so.tilo.chat"), shell)
            except Exception:
                (self.output / f"{name}-frames.json").write_text(json.dumps(
                    call(socket, {"op": "foreign_frames"}), indent=2))
                (self.output / f"{name}-notifications.json").write_text(json.dumps(
                    packaged.chrome_state(socket).get("notifications"), indent=2))
                (self.output / f"{name}-actions.json").write_text(json.dumps(
                    [packaged.public_action_observation(row) for row in self.catalogue(socket)], indent=2))
                require_ok(call(socket, {"op": "snapshot", "path": str(self.output / f"{name}-failed-launch.png")}))
                raise
            self.record(f"{name}-signed-native-chat-presented")
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
            self.app_state(sessions, bootstrap["room"], early, bootstrap["users"])
            if continuity == "reconnect":
                self.reconnect(sessions, bootstrap, host, early)
            return
        if not voice_only:
            self.context(sessions, bootstrap["room"], bootstrap["presence_ids"])
        channel = self.voice(sessions, bootstrap["room"])
        self.leave_and_revoke(sessions, bootstrap["room"], host, channel)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--binaries", type=pathlib.Path, default=REPO / "target/debug")
    parser.add_argument("--pipewire", type=pathlib.Path,
                        help="private extracted PipeWire server/config/plugin payload")
    parser.add_argument("--cleanup-proof", action="store_true", help="test exact private daemon cleanup without launching Chat")
    parser.add_argument("--voice-proof", action="store_true", help="test governed native voice without qualifying follow or the combined pair")
    parser.add_argument("--state-proof", action="store_true",
                        help="qualify native app state independently of voice and following")
    parser.add_argument("--automatic-opening-proof", action="store_true",
                        help="test automatic native app opening on an enrolled same-person device")
    parser.add_argument("--reconnect-proof", action="store_true",
                        help="test same-person native apps across a private governor transport partition")
    parser.add_argument("--font-namespace", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    continuity = "automatic" if args.automatic_opening_proof else "reconnect" if args.reconnect_proof else None
    state_mode = args.state_proof or continuity is not None
    if sum((args.cleanup_proof, args.voice_proof, args.state_proof,
            args.automatic_opening_proof, args.reconnect_proof)) > 1:
        parser.error("choose only one isolated proof mode")
    if not state_mode and not args.cleanup_proof and args.pipewire is None:
        parser.error("voice requires --pipewire")
    if state_mode and not args.font_namespace:
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
    if state_mode:
        subprocess.run([sys.executable, "-B", str(REPO / "tools/font-catalog.py"),
                        "verify", "/usr/share/fonts/tilo", "--scope", "os"], check=True)
        mounts = pathlib.Path("/proc/self/mountinfo").read_text().splitlines()
        if not any(row.split()[4] == "/usr/share/fonts/tilo"
                   and "ro" in row.split()[5].split(",") for row in mounts):
            raise RuntimeError("canonical OS font namespace is not read-only")
    args.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    fixture = Fixture(args.out.resolve(), args.binaries.resolve(), args.pipewire.resolve() if args.pipewire else None)
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
            "mode": f"same-person-{continuity}-proof" if continuity else "native-app-state-proof" if args.state_proof else "cleanup-proof" if args.cleanup_proof else "native-voice-proof" if args.voice_proof else "native-pair",
            "error": error, "cleanup_error": cleanup_error, "stages": fixture.stages,
            "real_microphone": False, "video_screen_sharing": False,
            "same_person_devices_qualified": state_qualified and continuity is not None,
            "automatic_peer_app_launch_qualified": state_qualified and continuity == "automatic",
            "offline_reconnect_qualified": state_qualified and continuity == "reconnect",
        }, indent=2) + "\n")
        if cleanup_error is not None:
            raise RuntimeError(f"fixture cleanup failed: {cleanup_error}")


if __name__ == "__main__":
    main()
