#!/usr/bin/env bash
# Exercise immutable Forgejo attachment publication against a local API double.
set -euo pipefail

# Match the mock server's public repository, independently of the caller's CI
# release configuration. Individual negative cases override this explicitly.
export TILO_RELEASE_REPOSITORY=qos/releases

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
publisher="$repo/apps/imager/packaging/publish-forgejo-release.sh"
work="$(mktemp -d /var/tmp/tilo-forgejo-publication-test.XXXXXX)"
server_pid=''
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  find "$work" -depth -delete
}
trap cleanup EXIT

python3 - "$work/port" <<'PY' &
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

release = None
assets = []
asset_bytes = {}

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def send_json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        global release
        path = urlsplit(self.path).path
        if path == "/api/v1/repos/qos/releases":
            self.send_json(200, {"full_name": "qos/releases", "private": False, "internal": False, "archived": False})
        elif path == "/api/v1/repos/tilo/internal-downloads":
            self.send_json(200, {"full_name": "tilo/internal-downloads", "private": False, "internal": True, "archived": False})
        elif path == "/api/v1/repos/tilo/private-downloads":
            self.send_json(200, {"full_name": "tilo/private-downloads", "private": True, "internal": False, "archived": False})
        elif path.endswith("/releases/tags/imager-v1.2.3"):
            if release is None:
                self.send_json(404, {"message": "not found"})
            else:
                self.send_json(200, release)
        elif path.endswith("/releases/1/assets"):
            self.send_json(200, assets)
        elif path.startswith("/download/"):
            asset_id = int(path.rsplit("/", 1)[1])
            body = asset_bytes[asset_id]
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_json(404, {"message": "not found"})

    def do_POST(self):
        global release
        parsed = urlsplit(self.path)
        if parsed.path.endswith("/releases"):
            length = int(self.headers["Content-Length"])
            request = json.loads(self.rfile.read(length))
            if request.get("draft") is not True:
                self.send_json(400, {"message": "release must begin as a draft"})
                return
            release = {"id": 1, **request}
            self.send_json(201, release)
            return
        if not parsed.path.endswith("/releases/1/assets"):
            self.send_json(404, {"message": "not found"})
            return
        content_type = self.headers.get("Content-Type", "")
        match = re.search(r"boundary=([^;]+)", content_type)
        if not match:
            self.send_json(400, {"message": "missing multipart boundary"})
            return
        boundary = match.group(1).strip('"').encode()
        length = int(self.headers["Content-Length"])
        body = self.rfile.read(length)
        parts = body.split(b"--" + boundary)
        if len(parts) < 3 or b"\r\n\r\n" not in parts[1]:
            self.send_json(400, {"message": "invalid multipart body"})
            return
        payload = parts[1].split(b"\r\n\r\n", 1)[1]
        if payload.endswith(b"\r\n"):
            payload = payload[:-2]
        name = parse_qs(parsed.query).get("name", [""])[0]
        asset_id = len(assets) + 1
        asset_bytes[asset_id] = payload
        asset = {
            "id": asset_id,
            "name": name,
            "size": len(payload),
            "browser_download_url": f"http://127.0.0.1:{self.server.server_port}/download/{asset_id}",
        }
        assets.append(asset)
        self.send_json(201, asset)

    def do_PATCH(self):
        global release
        parsed = urlsplit(self.path)
        if not parsed.path.endswith("/releases/1") or release is None:
            self.send_json(404, {"message": "not found"})
            return
        length = int(self.headers["Content-Length"])
        request = json.loads(self.rfile.read(length))
        if request.get("draft") is not False or not assets:
            self.send_json(400, {"message": "cannot publish an empty release"})
            return
        release = {"id": 1, **request}
        self.send_json(200, release)

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(sys.argv[1], "w", encoding="utf-8") as target:
    target.write(str(server.server_port))
server.serve_forever()
PY
server_pid=$!

for _attempt in $(seq 1 100); do
  [ -s "$work/port" ] && break
  sleep 0.02
done
[ -s "$work/port" ] || { printf 'mock Forgejo server did not start\n' >&2; exit 1; }
server_url="http://127.0.0.1:$(<"$work/port")"
mkdir -p "$work/assets"
printf 'AAAAAAAAAAAAAAAA\n' >"$work/assets/tilo-imager-test.bin"

publish() {
  GITHUB_SERVER_URL="$server_url" \
  GITHUB_REPOSITORY='qos/qos' \
  GITHUB_REF_NAME='imager-v1.2.3' \
  FORGEJO_TOKEN='test-token' \
    "$publisher" "$work/assets"
}

first="$(publish)"
grep -Fq 'published and verified tilo-imager-test.bin' <<<"$first" \
  || { printf 'first publication was not uploaded and verified\n' >&2; exit 1; }
grep -Fq 'published complete release imager-v1.2.3' <<<"$first" \
  || { printf 'verified draft was not promoted after upload\n' >&2; exit 1; }

if TILO_RELEASE_REPOSITORY=qos/qos publish >"$work/source.out" 2>"$work/source.err"; then
  printf 'publisher accepted the source repository as its binary target\n' >&2
  exit 1
fi
grep -Fq 'must be separate from the source repository' "$work/source.err"
if TILO_RELEASE_REPOSITORY=tilo/private-downloads publish \
    >"$work/private.out" 2>"$work/private.err"; then
  printf 'publisher accepted a private binary target\n' >&2
  exit 1
fi
grep -Fq 'release repository must be public' "$work/private.err"
if TILO_RELEASE_REPOSITORY=tilo/internal-downloads publish \
    >"$work/internal.out" 2>"$work/internal.err"; then
  printf 'publisher accepted an internal binary target\n' >&2
  exit 1
fi
grep -Fq 'release repository must be public' "$work/internal.err"

second="$(publish)"
grep -Fq 'verified existing tilo-imager-test.bin' <<<"$second" \
  || { printf 'identical rerun did not accept the existing asset\n' >&2; exit 1; }

printf 'BBBBBBBBBBBBBBBB\n' >"$work/assets/tilo-imager-test.bin"
if publish >"$work/mismatch.out" 2>"$work/mismatch.err"; then
  printf 'mismatched rerun unexpectedly succeeded\n' >&2
  exit 1
fi
grep -Fq 'release asset mismatch' "$work/mismatch.err" \
  || { printf 'mismatched rerun did not explain the immutable conflict\n' >&2; exit 1; }

printf 'AAAAAAAAAAAAAAAA\n' >"$work/assets/tilo-imager-test.bin"
after_conflict="$(publish)"
grep -Fq 'verified existing tilo-imager-test.bin' <<<"$after_conflict" \
  || { printf 'mismatched rerun changed the published asset\n' >&2; exit 1; }

printf 'ok    Forgejo publication is idempotent and immutable\n'
