#!/usr/bin/env python3
"""Actual CEF selected-cookie import, TLS transmission and durable profile isolation.

Only synthetic Slack-domain credentials and temporary profiles are used. A local
TLS fixture is trusted by its exact SPKI; no real service or user data is visited.
Run with the built host path, optionally followed by --capability-receipt.
"""
import argparse
import base64
import hashlib
import http.server
import json
from pathlib import Path
import queue
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("host", type=Path)
parser.add_argument("--capability-receipt", action="store_true")
arguments = parser.parse_args()
HOST = arguments.host.resolve()
SESSION = "synthetic_selected_session"


class Host:
    def __init__(self, profile, flags=()):
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen([str(HOST), "--profile=" + str(profile), *flags],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log)
        self.events = queue.Queue()
        threading.Thread(target=self.read, daemon=True).start()
        try:
            self.until(lambda e: e.get("event") == "ready")
        except BaseException:
            self.close()
            raise

    def read(self):
        def exact(n):
            data = bytearray()
            while len(data) < n:
                chunk = self.process.stdout.read(n - len(data))
                if not chunk:
                    raise EOFError("Native CEF host closed")
                data.extend(chunk)
            return data
        try:
            while True:
                length, = struct.unpack("<I", exact(4))
                payload = exact(length)
                if payload[0] == 1:
                    self.events.put(json.loads(payload[1:]))
        except BaseException as error:
            self.events.put(error)

    def send(self, **command):
        payload = b"\x01" + json.dumps(command).encode()
        self.process.stdin.write(struct.pack("<I", len(payload)) + payload)
        self.process.stdin.flush()

    def until(self, predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while True:
            event = self.events.get(timeout=max(.01, deadline - time.monotonic()))
            if isinstance(event, BaseException):
                raise event
            # Native replies must never echo private payloads.
            assert "synthetic-secret" not in json.dumps(event)
            if predicate(event):
                return event
            if time.monotonic() >= deadline:
                raise TimeoutError("Native CEF event not received")

    def import_cookies(self, cookies, request=1, session=SESSION, service="slack", origins=None, verify=False):
        self.send(op="service_session_import", tab=1, request_id=request,
                  session_id=session, service=service, cookies=cookies,
                  origin_storage=origins or [], verify_only=verify)
        return self.until(lambda e: e.get("event") == "service_session_result" and e["request_id"] == request)

    def navigate(self, url, marker):
        self.send(op="navigate", tab=1, url=url + "/" + marker)
        self.until(lambda e: e.get("event") == "title" and e.get("title") == marker)

    def close(self):
        try:
            if self.process.poll() is None:
                try:
                    self.send(op="shutdown")
                    assert self.process.wait(timeout=35) == 0
                finally:
                    if self.process.poll() is None:
                        self.process.kill()
                        self.process.wait()
            self.log.seek(0)
            assert b"synthetic-secret" not in self.log.read()
        finally:
            self.process.stdin.close()
            self.process.stdout.close()
            self.log.close()

    def __enter__(self):
        return self

    def __exit__(self, kind, *_):
        if kind is not None:
            self.log.flush();self.log.seek(0)
            # Only fixed native stage/status diagnostics, never raw DevTools
            # replies, DOM contents, tokens or arbitrary browser log lines.
            for line in self.log.read().splitlines():
                if line.startswith(b"[service-session] "):
                    print(line.decode("ascii"), file=sys.stderr)
        self.close()


def cookie(name, host_only=True, expires=True):
    return dict(host="app.slack.com", domain="app.slack.com" if host_only else "slack.com",
                host_only=host_only, name=name, value="synthetic-secret-" + name,
                path="/", expires_unix_seconds=int(time.time()) + 3600 if expires else None,
                secure=True, http_only=True, same_site="lax")


def main():
    requests = queue.Queue()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            marker = self.path.lstrip("/")
            requests.put((marker, self.headers.get("Cookie", "")))
            page = ("<!doctype html><title>" + marker + "</title>").encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def log_message(self, *_):
            pass

    def observed(marker):
        deadline = time.monotonic() + 10
        while True:
            name, cookies = requests.get(timeout=max(.01, deadline - time.monotonic()))
            if name == marker:
                return cookies
            if time.monotonic() >= deadline:
                raise TimeoutError("TLS fixture was not observed")

    with tempfile.TemporaryDirectory(prefix="tilo-session-cef-") as temporary:
        root = Path(temporary)
        cert, key = root / "cert.pem", root / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                        "-subj", "/CN=app.slack.com", "-keyout", str(key), "-out", str(cert)],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        public = subprocess.check_output(["openssl", "x509", "-in", str(cert), "-pubkey", "-noout"])
        der = subprocess.run(["openssl", "pkey", "-pubin", "-outform", "DER"], input=public,
                             check=True, capture_output=True).stdout
        spki = base64.b64encode(hashlib.sha256(der).digest()).decode()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        flags = ["--host-resolver-rules=MAP app.slack.com 127.0.0.1,MAP other.slack.com 127.0.0.1,MAP discord.com 127.0.0.1",
                 "--ignore-certificate-errors-spki-list=" + spki, "--no-proxy-server"]
        selected = flags + ["--service-session=" + SESSION]
        url = "https://app.slack.com:" + str(server.server_port)
        other = "https://other.slack.com:" + str(server.server_port)
        cookies = [cookie("__Host-owned"), cookie("domain", False), cookie("session", expires=False)]
        try:
            # Ordinary profiles refuse the same well-formed import, without writes.
            with Host(root / "normal", flags) as host:
                result = host.import_cookies(cookies)
                assert result["imported"] is False and result["cookie_count"] == 0
                host.navigate(url, "ordinary")
                assert observed("ordinary") == ""
            with Host(root / "private", flags + ["--private"]) as host:
                result = host.import_cookies(cookies)
                assert result["imported"] is False and result["cookie_count"] == 0
                host.navigate(url, "private")
                assert observed("private") == ""
            with Host(root / "selected", selected) as host:
                refused = host.import_cookies(cookies, session="wrong_session")
                assert refused["imported"] is False
                result = host.import_cookies(cookies, request=2)
                assert result["imported"] is True and result["cookie_count"] == 3
                assert set(result) == {"event", "tab", "request_id", "session_id", "service", "imported", "cookie_count", "origin_storage_count"}
                host.navigate(url, "installed")
                transmitted = observed("installed")
                assert all(c["name"] + "=" + c["value"] in transmitted for c in cookies)
                host.navigate(other, "domain-scope")
                assert observed("domain-scope") == "domain=synthetic-secret-domain"
                # A second import must not overwrite the current service profile.
                replaced = [cookie("domain", False)]
                replaced[0]["value"] = "replacement"
                assert host.import_cookies(replaced, request=3)["imported"] is False
            # Restart the actual CEF host. Persistent and session cookies survive
            # the completed FlushStore receipt, without credential replay.
            with Host(root / "selected", selected) as host:
                verified = host.import_cookies(cookies, request=2, verify=True)
                assert verified["imported"] is True and verified["cookie_count"] == 3 and verified["origin_storage_count"] == 0
                host.navigate(url, "restart")
                transmitted = observed("restart")
                assert all(c["name"] + "=" + c["value"] in transmitted for c in cookies)
            with Host(root / "normal", flags) as host:
                host.navigate(url, "normal-restart")
                assert observed("normal-restart") == ""
            # Validation is atomic: a malformed later cookie cannot install a
            # preceding valid cookie. The strict protocol closes the bad sender.
            invalid = [cookie("valid"), cookie("foreign")]
            invalid[1]["domain"] = invalid[1]["host"] = "evilslack.com"
            bad = Host(root / "invalid", selected)
            try:
                bad.send(op="service_session_import", tab=1, request_id=1,
                         session_id=SESSION, service="slack", cookies=invalid)
                assert bad.process.wait(timeout=35) == 0
            finally:
                bad.close()
            with Host(root / "invalid", selected) as host:
                host.navigate(url, "invalid-restart")
                assert observed("invalid-restart") == ""
            # Origin-only Discord storage uses a local intercepted exact-origin
            # bootstrap, with no provider scripts or external HTTPS request.
            origins = [{"origin": "https://discord.com", "key": "token", "value": '"synthetic-secret-discord"'}]
            with Host(root / "discord", selected) as host:
                result = host.import_cookies([], service="discord", origins=origins)
                assert result["imported"] is True and result["cookie_count"] == 0 and result["origin_storage_count"] == 1, {key: result.get(key) for key in ("event", "request_id", "imported", "cookie_count", "origin_storage_count")}
            with Host(root / "discord", selected) as host:
                verified = host.import_cookies([], request=2, service="discord", origins=origins, verify=True)
                assert verified["imported"] is True and verified["origin_storage_count"] == 1
            # Wrong expected bytes refuse; verification must not overwrite the
            # stored raw DOM string, including its original quotes.
            wrong = [dict(origins[0], value="replacement")]
            with Host(root / "discord", selected) as host:
                assert host.import_cookies([], request=2, service="discord", origins=wrong, verify=True)["imported"] is False
            with Host(root / "discord", selected) as host:
                assert host.import_cookies([], request=2, service="discord", origins=origins, verify=True)["imported"] is True
            with Host(root / "discord-empty", selected) as host:
                assert host.import_cookies([], request=2, service="discord", origins=origins, verify=True)["imported"] is False
            # Refuse the general-origin facility before any preceding valid
            # cookie is written. The selected origin/key is fixed, not an API
            # for arbitrary localStorage or script injection.
            discord_cookie = dict(cookie("atomic"), host="discord.com", domain="discord.com")
            unsupported = [dict(origins[0], origin="https://discord.com.evil.example")]
            bad = Host(root / "discord-invalid", selected)
            try:
                bad.send(op="service_session_import", tab=1, request_id=1, session_id=SESSION,
                         service="discord", cookies=[discord_cookie], origin_storage=unsupported)
                assert bad.process.wait(timeout=35) == 0
            finally:
                bad.close()
            with Host(root / "discord-invalid", selected) as host:
                assert host.import_cookies([discord_cookie], request=2, service="discord", verify=True)["imported"] is False
        finally:
            server.shutdown()
            server.server_close()
    if arguments.capability_receipt:
        print(json.dumps({
            "schema": 1,
            "service_session_protocol": 2,
            "cookie_persistence_verified": True,
            "origin_storage_persistence_verified": True,
        }, separators=(",", ":")))
    else:
        print("selected CEF session import: PASS")


if __name__ == "__main__":
    main()
