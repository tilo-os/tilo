"""Loopback-only authenticated smart Git fixture; never uses a real account."""

import http.server
import json
import os
import pathlib
import ssl
import subprocess
import sys
import threading
import time
import urllib.parse

root = pathlib.Path(sys.argv[1]).resolve()
ready = root / "ready.json"
# The test's hang bound (tilo_test_env::hang_bound). Git's own work under a
# loaded runner (index-pack, hooks, fsync) is not what these tests measure, so
# no fixture wait may give up sooner and turn a slow CGI into a dropped
# connection that the client can only report as a network failure.
hang_bound = float(sys.argv[sys.argv.index("--hang-bound") + 1])


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, status, body=b"", headers=()):
        self.send_response(status)
        for key, value in headers:
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def body(self):
        if self.headers.get("Transfer-Encoding", "").lower() != "chunked":
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 <= size <= 8 * 1024 * 1024:
                raise ValueError("oversized fixture request")
            return self.rfile.read(size)
        chunks = bytearray()
        while True:
            size = int(self.rfile.readline().split(b";", 1)[0], 16)
            if not size:
                while self.rfile.readline().strip():
                    pass
                return bytes(chunks)
            if size < 0 or len(chunks) + size > 8 * 1024 * 1024:
                raise ValueError("oversized fixture request")
            chunks.extend(self.rfile.read(size))
            if self.rfile.read(2) != b"\r\n":
                raise ValueError("invalid chunk terminator")

    def serve_git(self):
        self.connection.settimeout(hang_bound)
        if (root / "block").exists():
            (root / "request-started").touch()
            deadline = time.monotonic() + hang_bound
            while not (root / "release").exists():
                if time.monotonic() >= deadline:
                    self.reply(503)
                    return
                time.sleep(0.01)
        if self.headers.get("Authorization") != "Basic dGVzdDp0ZXN0":
            self.server.denials += 1
            # Bound even a broken client that retries the same rejected credentials.
            status = 401 if self.server.denials <= 8 else 403
            self.reply(status, headers=[("WWW-Authenticate", 'Basic realm="Tilo fixture"')])
            return
        url = urllib.parse.urlsplit(self.path)
        path = urllib.parse.unquote(url.path)
        if not path.startswith("/remote.git/") or ".." in path.split("/"):
            self.reply(404)
            return
        payload = self.body()
        environment = os.environ.copy()
        environment.update(
            GIT_PROJECT_ROOT=str(root),
            GIT_HTTP_EXPORT_ALL="1",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=str(root / "empty-config"),
            PATH_INFO=path,
            QUERY_STRING=url.query,
            REQUEST_METHOD=self.command,
            CONTENT_TYPE=self.headers.get("Content-Type", ""),
            CONTENT_LENGTH=str(len(payload)),
            REMOTE_USER="test",
            HTTP_GIT_PROTOCOL=self.headers.get("Git-Protocol", ""),
        )
        output = subprocess.run(
            ["git", "http-backend"], input=payload, capture_output=True,
            env=environment, timeout=hang_bound,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        if output.returncode:
            raise RuntimeError(output.stderr.decode(errors="replace"))
        raw_headers, separator, body = output.stdout.partition(b"\r\n\r\n")
        if not separator:
            raise RuntimeError("Git CGI response has no header boundary")
        status = 200
        headers = []
        for line in raw_headers.decode().splitlines():
            key, value = line.split(":", 1)
            if key.lower() == "status":
                status = int(value.strip().split()[0])
            elif key.lower() not in ("content-length", "connection"):
                headers.append((key, value.strip()))
        self.reply(status, body, headers)

    do_GET = serve_git
    do_POST = serve_git


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
if "--tls" in sys.argv[2:]:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(root / "certificate.pem", root / "key.pem")
    server.socket = context.wrap_socket(server.socket, server_side=True)
server.denials = 0
# Accept in a worker before advertising readiness so a client that races the
# ready file cannot see connection-refused (libgit2 Net/12) before serve_forever.
serving = threading.Thread(target=server.serve_forever, name="git-http-fixture", daemon=True)
serving.start()
temporary = ready.with_suffix(".tmp")
temporary.write_text(json.dumps({"port": server.server_port}), encoding="utf-8")
temporary.replace(ready)
serving.join()
