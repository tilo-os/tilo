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
    def run_upload(self, root, url, newline=True):
        payload = root / "f"
        payload.write_bytes(b"x")
        manifest = root / "uploads.tsv"
        manifest.write_text(f"{payload}\t{url}" + ("\n" if newline else ""))
        return subprocess.run(
            ["bash", str(SCRIPT), str(manifest)],
            env={**os.environ, "TILO_PACKAGE_TOKEN": "t", "RUNNER_TEMP": str(root)},
            capture_output=True, text=True, check=False,
        )

    def test_unterminated_final_row_is_uploaded_not_silently_skipped(self):
        class Accepted(Moved):
            def do_PUT(self):
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                self.server.uploads += 1
                self.send_response(201)
                self.send_header("Content-Length", "0")
                self.end_headers()

        server = ThreadingHTTPServer(("127.0.0.1", 0), Accepted)
        server.uploads = 0
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_upload(Path(temporary), f"http://127.0.0.1:{server.server_port}/f", False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(server.uploads, 1)

    def test_conflict_readback_requires_http_200_even_if_body_matches(self):
        class Conflict(Moved):
            def do_PUT(self):
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                self.send_response(409)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_GET(self):
                self.send_response(self.server.read_status)
                self.send_header("Location", "/elsewhere")
                self.send_header("Content-Length", "1")
                self.end_headers()
                self.wfile.write(b"x")

        server = ThreadingHTTPServer(("127.0.0.1", 0), Conflict)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            for status in (302, 200):
                server.read_status = status
                result = self.run_upload(Path(temporary), f"http://127.0.0.1:{server.server_port}/f")
                with self.subTest(status=status):
                    self.assertEqual(result.returncode, 0 if status == 200 else 1, result.stderr)
                    if status == 302:
                        self.assertNotIn("already published identically", result.stdout)

    def test_a_redirected_upload_fails_and_names_the_new_location(self) -> None:
        server = ThreadingHTTPServer(("127.0.0.1", 0), Moved)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
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
