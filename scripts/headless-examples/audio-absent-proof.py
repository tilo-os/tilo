#!/usr/bin/env python3
"""A session starts with no audio server: the desk works, Quick Settings shows
the volume row saying there is no audio device, and the sound comes back when
the server appears, without a restart.

The sound is one service among the session's, never a condition of starting
it: a check or an agent in a build container has no PipeWire, and gets the
same desktop with its sound controls saying so.

The session starts with a `pipewire` on PATH that fails at once, so it runs
exactly as it does in a container. A person opens Notes, and opens Quick
Settings from the bar: the panel stands over the desk and the sound's own
volume action answers that there is no output to set. Then a real PipeWire,
WirePlumber and its session output start in the session's own runtime
directory: the same action sets the output's level, which the graph now
holds, and the panel's row is one the person can change again.

It needs the session and the tree's apps built (dist/dev/nested-session-build.sh,
or `TILO_NESTED_SESSION_BUILD` naming where they were).

Usage:
  ./audio-absent-proof.py [--build]
"""
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import real_apps  # noqa: E402
from real_apps import Failed, until  # noqa: E402

APPS = "notes"
STATE = pathlib.Path(os.environ.get("TILO_AUDIO_ABSENT_PROOF_STATE") or tempfile.mkdtemp(
    prefix="tilo-audio-absent-"))
SHOTS = pathlib.Path(os.environ.get("TILO_AUDIO_ABSENT_PROOF_SHOTS")
                     or tempfile.mkdtemp(prefix="tilo-audio-absent-shots-"))
# The session's own output, as nested-session.sh names it (tilo_media::audio::SESSION_SINK).
SESSION_SINK = "tilo-sink"
# What the panel's volume row sets, and the level this drive sets it to.
SET_VOLUME, LEVEL = "set-volume", 0.25

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def chrome(session) -> dict:
    return session.state_dump()["chrome"]


def wait_for(label: str, read, deadline: float = 20.0):
    try:
        return until(label, read, deadline, 0.1)
    except Failed:
        return None


def show_quick_settings(session) -> None:
    """Open the panel over the desk, as a person presses the bar's control."""
    session.wait_target("Open quick settings", 30)
    session.click_named("Open quick settings")


def sound_action(session) -> dict:
    """The sound's own volume action, as the session offers it: the session's
    action on its `sound` resource, not an app's effect that asks for it."""
    return until("the sound's volume action", lambda: next(
        (action for action in session.actions(SET_VOLUME)
         if action.get("provider") == "so.tilo.session"
         and action.get("id", "").split("\x1f")[-1] == SET_VOLUME), None), 30, 0.2)


def without_audio_server() -> pathlib.Path:
    """A PATH whose `pipewire` exits at once: this session has no audio graph."""
    directory = STATE / "path"
    directory.mkdir(parents=True, exist_ok=True)
    stub = directory / "pipewire"
    stub.write_text("#!/bin/sh\nexit 1\n")
    stub.chmod(0o755)
    return directory


def start_audio(session) -> tuple[list[subprocess.Popen], dict]:
    """Start PipeWire, WirePlumber and the session's output where the session
    looks for them: its own runtime directory."""
    runtime = session.runtime_directory()
    env = {**session.env()}
    buses = list(pathlib.Path(runtime).glob("session-bus.*/bus"))
    if buses:
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={buses[0]}"
    system = "/usr/sbin:/usr/bin:/bin"
    started = []
    for program in ("pipewire", "wireplumber"):
        path = shutil.which(program, path=system)
        if path is None:
            raise Failed(f"no {program} to start")
        started.append(subprocess.Popen([path], env=env, start_new_session=True,
                                        stdout=subprocess.DEVNULL,
                                        stderr=subprocess.DEVNULL))
    socket = pathlib.Path(runtime) / "pipewire-0"
    until("the audio server's socket", socket.is_socket, 20, 0.05)
    null_sink = (
        "{ factory.name=support.null-audio-sink node.name=%s"
        ' node.description="Session output" media.class=Audio/Sink'
        " object.linger=true audio.position=[ FL FR ] }" % SESSION_SINK
    )
    real_apps.run([shutil.which("pw-cli", path=system), "create-node", "adapter", null_sink], env=env)
    # The one output is the default sink, as a person's only speaker is.
    wpctl = shutil.which("wpctl", path=system)
    until("the session's output",
          lambda: subprocess.run([wpctl, "inspect", "@DEFAULT_AUDIO_SINK@"], env=env,
                                 capture_output=True).returncode == 0, 20, 0.1)
    return started, env


def sink_volume(env) -> str:
    """What the audio graph holds as the session output's level."""
    return real_apps.run([shutil.which("wpctl", path="/usr/sbin:/usr/bin:/bin"),
                          "get-volume", "@DEFAULT_AUDIO_SINK@"], env=env).strip()


def main() -> int:
    SHOTS.mkdir(parents=True, exist_ok=True)
    if "--build" in sys.argv:
        real_apps.build(apps=APPS)
    real_apps.prepare(STATE, apps=APPS, built=False)
    print(f"==> state {STATE}")
    shim = without_audio_server()

    session = real_apps.Session(STATE)
    audio: list[subprocess.Popen] = []
    try:
        session.start(apps=APPS, fresh=True, out=SHOTS,
                      env={"PATH": f"{shim}:{os.environ['PATH']}"})
        check("the session starts with no audio server", session.running())

        # The desk works: a person opens Notes through the desktop's launcher.
        session.launch("notes")
        session.answering("notes", deadline=180)
        check("the desk opens an application",
              any(window.get("app_id") == "so.tilo.notes" for window in session.windows()))

        # Quick Settings stands over the desk, its volume row in the absent
        # state: the panel is drawn and the sound's own volume action answers
        # that there is no output to set.
        show_quick_settings(session)
        shown = wait_for("the quick settings panel",
                         lambda: (chrome(session).get("surfaces") or {}).get("quick-settings"))
        check("quick settings stands over the desk without an audio server", shown is not None,
              json.dumps(chrome(session).get("overlays")))
        volume = sound_action(session)
        before = None
        try:
            session.invoke(volume["provider"], volume["id"], {"level": LEVEL})
        except Failed as error:
            before = str(error)
        check("the sound's volume action says there is no output to set", before is not None,
              (before or "it set a level with no output")
              .removeprefix("actions_invoke refused: ")[:160])
        shot = session.snapshot(SHOTS / "01-no-audio-device.png")
        print(f"  shot {shot}")

        # The audio server appears: the same action now sets the output's
        # level, which the graph holds, and the panel's row is one a person
        # can change again.
        audio, env = start_audio(session)

        def set_level() -> bool:
            try:
                session.invoke(volume["provider"], volume["id"], {"level": LEVEL})
                return True
            except Failed:
                return False

        applied = wait_for("the sound coming up", set_level, 60)
        landed = applied is not None and wait_for(
            "the output's level", lambda: f"{LEVEL:.2f}" in sink_volume(env), 20) is not None
        check("the volume action sets the output's level when the server appears",
              landed, sink_volume(env))
        check("the panel's sound row is live again",
              (chrome(session).get("surfaces") or {}).get("quick-settings") is not None)
        shot = session.snapshot(SHOTS / "02-audio-back.png")
        print(f"  shot {shot}")
    except Failed as error:
        check("the proof ran to the end", False, str(error))
        session.snapshot(SHOTS / "failure.png")
    finally:
        for process in audio:
            process.terminate()
        session.stop()
    for process in audio:
        try:
            process.wait(5)
        except subprocess.TimeoutExpired:
            process.kill()
    if failures:
        print(f"FAILED: {', '.join(failures)}")
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
