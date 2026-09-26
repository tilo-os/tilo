#!/usr/bin/env bash
# Exercise immutable GitHub attachment publication against a local API double.
set -euo pipefail

# Match the mock server's public repository, independently of the caller's CI
# release configuration. Individual negative cases override this explicitly.
export TILO_RELEASE_REPOSITORY=tilo-os/releases

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
publisher="$repo/apps/imager/packaging/publish-github-release.sh"
work="$(mktemp -d /var/tmp/tilo-github-publication-test.XXXXXX)"
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
import os
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
        if path == "/installation/repositories":
            self.send_json(200, {"total_count": 1, "repositories": [{"full_name": "tilo-os/releases"}]})
        elif path == "/repos/tilo-os/releases":
            self.send_json(200, {"full_name": "tilo-os/releases", "private": False, "visibility": "public", "archived": False})
        elif path.endswith("/releases/tags/imager-v1.2.3"):
            if release is None:
                self.send_json(404, {"message": "not found"})
            else:
                self.send_json(200, release)
        elif path.endswith("/releases/1/assets"):
            self.send_json(200, assets)
        elif path.startswith("/repos/tilo-os/releases/releases/assets/"):
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
        length = int(self.headers["Content-Length"])
        body = self.rfile.read(length)
        name = parse_qs(parsed.query).get("name", [""])[0]
        asset_id = len(assets) + 1
        asset_bytes[asset_id] = body
        asset = {
            "id": asset_id,
            "name": name,
            "size": len(body),
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
# Publish the port by rename so a reader never sees a partial number.
with open(sys.argv[1] + ".partial", "w", encoding="utf-8") as target:
    target.write(str(server.server_port))
os.replace(sys.argv[1] + ".partial", sys.argv[1])
server.serve_forever()
PY
server_pid=$!

# Interpreter start-up is slow on a loaded runner, so wait for as long as the
# server process lives, bounded only against a wedged interpreter.
for _attempt in $(seq 1 1500); do
  [ -s "$work/port" ] && break
  kill -0 "$server_pid" 2>/dev/null || break
  sleep 0.02
done
[ -s "$work/port" ] || { printf 'mock GitHub server did not start\n' >&2; exit 1; }
server_url="http://127.0.0.1:$(<"$work/port")"
mkdir -p "$work/assets"
printf 'AAAAAAAAAAAAAAAA\n' >"$work/assets/tilo-imager-test.bin"

publish() {
  TILO_RELEASE_API_URL="$server_url" \
  TILO_RELEASE_UPLOADS_URL="$server_url" \
  GITHUB_REF_NAME='imager-v1.2.3' \
  GITHUB_RELEASE_TOKEN='test-token' \
    "$publisher" "$work/assets"
}

first="$(publish)"
grep -Fq 'published and verified tilo-imager-test.bin' <<<"$first" \
  || { printf 'first publication was not uploaded and verified\n' >&2; exit 1; }
grep -Fq 'published complete release imager-v1.2.3' <<<"$first" \
  || { printf 'verified draft was not promoted after upload\n' >&2; exit 1; }

if TILO_RELEASE_REPOSITORY=tilo-os/overbroad publish \
    >"$work/scope.out" 2>"$work/scope.err"; then
  printf 'publisher accepted a token scoped to a different repository\n' >&2
  exit 1
fi
grep -Fq 'must be narrowed to exactly tilo-os/overbroad' "$work/scope.err"

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

printf 'ok    GitHub publication is target-scoped, idempotent, and immutable\n'
