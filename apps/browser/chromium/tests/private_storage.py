#!/usr/bin/env python3
"""Real CEF normal/private cookie and localStorage isolation."""
import http.server
import json
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import ExitStack

HOST = Path(sys.argv[1]).resolve()
PAGE = b"""<!doctype html><meta charset=utf-8><title>starting</title>
<script>
const params = new URLSearchParams(location.search);
const value = params.get('set');
if (value) {
  localStorage.setItem('qosPrivateFixture', value);
  document.cookie = 'qosPrivateFixture=' + value + ';path=/;SameSite=Lax;Max-Age=3600';
}
document.title = JSON.stringify({
  page: location.search,
  cookie: document.cookie,
  localStorage: localStorage.getItem('qosPrivateFixture')
});
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

    def log_message(self, *_args):
        pass


class Host:
    def __init__(self, profile, private=False, fake_media=False):
        arguments = [str(HOST), "--profile=" + str(profile)]
        if private:
            arguments.append("--private")
        if fake_media:
            arguments.append("--use-fake-device-for-media-stream")
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log)
        self.events = queue.Queue()
        self.seen = []
        self.closed = False
        threading.Thread(target=self._read, daemon=True).start()
        try:
            self.until(lambda event: event["event"] == "ready")
        except BaseException:
            self.close()
            raise

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        self.close()

    def _read_exact(self, length):
        data = bytearray()
        while len(data) < length:
            chunk = self.process.stdout.read(length - len(data))
            if not chunk:
                raise EOFError("CEF helper closed IPC")
            data.extend(chunk)
        return data

    def _read(self):
        try:
            while True:
                length, = struct.unpack("<I", self._read_exact(4))
                payload = self._read_exact(length)
                if payload[0] == 1:
                    self.events.put(json.loads(payload[1:]))
        except Exception as error:
            self.events.put(error)

    def send(self, **command):
        payload = b"\x01" + json.dumps(command).encode()
        self.process.stdin.write(struct.pack("<I", len(payload)) + payload)
        self.process.stdin.flush()

    def until(self, predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            event = self.events.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(event, Exception):
                raise event
            self.seen.append(event)
            if predicate(event):
                return event
        raise TimeoutError("No matching native event: " + repr(self.seen[-8:]))

    def navigate(self, url, tab=1):
        self.send(op="navigate", tab=tab, url=url)
        expected_page = "?" + url.split("?", 1)[1] if "?" in url else ""
        title = self.until(
            lambda event: event["event"] == "title"
            and event.get("tab", 1) == tab
            and event["title"].startswith("{")
            and json.loads(event["title"])["page"] == expected_page
        )
        state = json.loads(title["title"])
        state.pop("page")
        return state

    def request_media(self, marker):
        self.send(
            op="scripts",
            scripts=[
                "navigator.mediaDevices.getUserMedia({video:true,audio:true})"
                ".then(s=>{s.getTracks().forEach(t=>t.stop());document.title='"
                + marker
                + "-granted'}).catch(e=>document.title='"
                + marker
                + "-'+e.name)"
            ],
        )
        return self.until(
            lambda event: event["event"] in ("permission_prompt", "title")
            and (
                event["event"] == "permission_prompt"
                or event.get("title", "").startswith(marker + "-")
            )
        )

    def status(self, origin, request_id):
        self.send(op="site_status", request_id=request_id, origin=origin)
        return self.until(
            lambda event: event["event"] == "site_status"
            and event["request_id"] == request_id
        )

    def revoke(self, origin, request_id, generation):
        self.send(
            op="site_action",
            request_id=request_id,
            origin=origin,
            generation=generation,
            action="revoke",
        )
        result = self.until(
            lambda event: event["event"] == "site_result"
            and event["request_id"] == request_id
        )
        assert result["success"], result

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            if self.process.poll() is None:
                self.send(op="shutdown")
                self.until(lambda event: event["event"] == "closed")
                assert self.process.wait(timeout=35) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.process.stdin.close()
            self.process.stdout.close()
            self.log.close()


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
origin = f"http://127.0.0.1:{server.server_port}/"
site_origin = origin.rstrip("/")

try:
    with tempfile.TemporaryDirectory(prefix="qos-normal-profile-") as normal_root:
        with tempfile.TemporaryDirectory(prefix="qos-private-profile-") as private_root:
            with ExitStack() as stack:
                normal = stack.enter_context(Host(normal_root, fake_media=True))
                private = stack.enter_context(
                    Host(private_root, private=True, fake_media=True)
                )
                assert normal.navigate(origin + "?set=normal-synthetic") == {
                    "cookie": "qosPrivateFixture=normal-synthetic",
                    "localStorage": "normal-synthetic",
                }
                assert private.navigate(origin) == {"cookie": "", "localStorage": None}
                assert private.navigate(origin + "?set=private-synthetic") == {
                    "cookie": "qosPrivateFixture=private-synthetic",
                    "localStorage": "private-synthetic",
                }
                private.send(op="create_tab", tab=2)
                private.until(lambda event: event["event"] == "tab_ready" and event["tab"] == 2)
                assert private.navigate(origin, tab=2) == {
                    "cookie": "qosPrivateFixture=private-synthetic",
                    "localStorage": "private-synthetic",
                }
                assert normal.navigate(origin) == {
                    "cookie": "qosPrivateFixture=normal-synthetic",
                    "localStorage": "normal-synthetic",
                }

                normal_prompt = normal.request_media("normal-media")
                assert normal_prompt["event"] == "permission_prompt", normal_prompt
                normal.send(
                    op="permission_decision", id=normal_prompt["id"], allow=True
                )
                normal.until(
                    lambda event: event["event"] == "title"
                    and event["title"] == "normal-media-granted"
                )
                normal_status = normal.status(site_origin, 11)
                assert normal_status["permissions"]["camera"] == "allowed"
                assert normal_status["permissions"]["microphone"] == "allowed"
                private_status = private.status(site_origin, 12)
                assert private_status["permissions"]["camera"] == "ask"
                assert private_status["permissions"]["microphone"] == "ask"

                private_prompt = private.request_media("private-media")
                assert private_prompt["event"] == "permission_prompt", private_prompt
                private.send(
                    op="permission_decision", id=private_prompt["id"], allow=True
                )
                private.until(
                    lambda event: event["event"] == "title"
                    and event["title"] == "private-media-granted"
                )
                private_status = private.status(site_origin, 13)
                assert private_status["permissions"]["camera"] == "allowed"
                assert private_status["permissions"]["microphone"] == "allowed"
                private.revoke(site_origin, 14, private_status["generation"])
                private_status = private.status(site_origin, 15)
                assert private_status["permissions"]["camera"] == "blocked"
                assert private_status["permissions"]["microphone"] == "blocked"
                normal_status = normal.status(site_origin, 16)
                assert normal_status["permissions"]["camera"] == "allowed"
                assert normal_status["permissions"]["microphone"] == "allowed"

            with Host(private_root, private=True, fake_media=True) as reopened_private:
                assert reopened_private.navigate(origin) == {"cookie": "", "localStorage": None}
                private_status = reopened_private.status(site_origin, 17)
                assert private_status["permissions"]["camera"] == "ask"
                assert private_status["permissions"]["microphone"] == "ask"

            with Host(normal_root, fake_media=True) as reopened_normal:
                assert reopened_normal.navigate(origin) == {
                    "cookie": "qosPrivateFixture=normal-synthetic",
                    "localStorage": "normal-synthetic",
                }
                normal_status = reopened_normal.status(site_origin, 18)
                assert normal_status["permissions"]["camera"] == "allowed"
                assert normal_status["permissions"]["microphone"] == "allowed"
    print(
        "PASS: private storage and permission isolation, multi-tab sharing, and clean reopen"
    )
finally:
    server.shutdown()
    server.server_close()
