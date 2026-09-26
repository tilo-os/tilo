"""tools/upload_immutable_packages.sh names a moved package base instead of failing opaquely."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/upload_immutable_packages.sh"


class Moved(BaseHTTPRequestHandler):
    def do_PUT(self) -> None:  # noqa: N802 - http.server API
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.send_response(307)
        self.send_header("Location", "/api/packages/tilo/generic/p/1/f")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *_args) -> None:
        pass


class UploadRedirectTests(unittest.TestCase):
    def test_a_redirected_upload_fails_and_names_the_new_location(self) -> None:
        server = ThreadingHTTPServer(("127.0.0.1", 0), Moved)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            payload = Path(temporary) / "f"
            payload.write_bytes(b"x")
            manifest = Path(temporary) / "uploads.tsv"
            url = f"http://127.0.0.1:{server.server_port}/api/packages/tilo/generic/p/1/f"
            manifest.write_text(f"{payload}\t{url}\n", encoding="utf-8")
            result = subprocess.run(
                ["bash", str(SCRIPT), str(manifest)],
                env={**os.environ, "TILO_PACKAGE_TOKEN": "t", "RUNNER_TEMP": temporary},
                capture_output=True, text=True, check=False,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("redirected (HTTP 307)", result.stderr)
        self.assertIn("/api/packages/tilo/generic/p/1/f", result.stderr)
        self.assertIn("TILO_PACKAGE_PUBLIC_BASE_URL", result.stderr)


if __name__ == "__main__":
    unittest.main()
