"""Inside a workspace's VM: open a page in Chromium and say how long it took.

Run by tools/ci/hosted-vm-measure.sh through `tilo-legacy hosted run`, as the
tenant. It starts the image's Chromium engine host the way Browser's session
does (framed JSON commands on its input, framed events and frames on its
output), opens one tab on a page, and prints one JSON line: milliseconds from
starting the engine to its `ready` event and to the tab's first frame. It then
keeps the page open for HOLD seconds (argv[1]) so the host can weigh the VM
with a page in it.
"""

import glob
import json
import os
import struct
import subprocess
import sys
import time

PAGE = (
    "data:text/html,<title>probe</title><h1>Visa application</h1>"
    "<form><label>Name <input value='Ada'></label><button>Next</button></form>"
)


def main() -> int:
    hold = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    engine = glob.glob("/usr/lib/tilo-runtime/*/lib/chromium/tilo-chromium-host")
    if not engine:
        print(json.dumps({"error": "no Chromium engine in this guest"}), flush=True)
        return 1
    profile = os.path.join(os.environ.get("TMPDIR", "/tmp"), f"probe-{os.getpid()}")
    started = time.monotonic()
    host = subprocess.Popen(
        [engine[0], f"--profile={profile}"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    commands, events = host.stdin, host.stdout
    assert commands is not None and events is not None

    def send(command: dict) -> None:
        body = json.dumps(command).encode()
        commands.write(struct.pack("<I", len(body) + 1) + b"\x01" + body)
        commands.flush()

    def packet() -> tuple[int, bytes]:
        head = events.read(5)
        if len(head) < 5:
            raise EOFError("the engine stopped")
        length, kind = struct.unpack("<I", head[:4])[0], head[4]
        return kind, events.read(length - 1)

    elapsed = lambda: round((time.monotonic() - started) * 1000)  # noqa: E731
    report: dict = {}
    try:
        while "ready_ms" not in report:
            kind, body = packet()
            if kind == 1 and json.loads(body).get("event") == "ready":
                report["ready_ms"] = elapsed()
        send({"op": "create_tab", "tab": 1})
        send({"op": "resize", "tab": 1, "width": 1280, "height": 800, "scale_factor": 1.0})
        send({"op": "focus", "tab": 1, "focused": True})
        send({"op": "navigate", "tab": 1, "url": PAGE})
        deadline = time.monotonic() + 60
        while "frame_ms" not in report and time.monotonic() < deadline:
            kind, body = packet()
            if kind in (2, 3, 5):
                report["frame_ms"] = elapsed()
            elif kind == 1 and json.loads(body).get("event") == "loaded":
                report.setdefault("loaded_ms", elapsed())
    except (EOFError, OSError, ValueError) as error:
        report["error"] = str(error)
    print(json.dumps(report), flush=True)
    time.sleep(hold)
    host.kill()
    return 0 if "frame_ms" in report else 1


if __name__ == "__main__":
    sys.exit(main())
