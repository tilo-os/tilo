#!/usr/bin/env python3
"""A download the person saves is handed to the tab that offered it, never saved by the engine.

The Browser fetches the file as a job of its own, which outlives the tab, so the engine's part
ends at the person's answer: it hands the tab its address, its name and what its request
carried (the site's cookies, HttpOnly ones included, and the page it came from), and lets go.
"""

import http.server
import queue
import tempfile
import threading
import unittest
from pathlib import Path
from urllib.parse import urlsplit

from native_regressions import Host

PAYLOAD = b"download-lifetime\n" * 65536


class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if urlsplit(self.path).path == "/signed-in":
            body = b"<p>signed in</p>"
            self.send_response(200)
            self.send_header("Set-Cookie", "session=abc; Path=/; HttpOnly")
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Disposition", "attachment; filename=lifetime.txt")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        try:
            self.wfile.write(PAYLOAD)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


class DownloadHost(Host):
    def until(self, predicate, timeout=90):
        try:
            return super().until(predicate, timeout)
        except (queue.Empty, TimeoutError) as error:
            self.log.seek(0)
            stderr = self.log.read().decode(errors="replace")[-4000:]
            raise AssertionError(
                f"No matching native event; latest={self.seen[-12:]!r}; stderr={stderr!r}"
            ) from error


class Request(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.downloads = tempfile.TemporaryDirectory(prefix="tilo-downloads-")
        self.addCleanup(self.downloads.cleanup)

    def host(self):
        host = DownloadHost((f"--downloads={self.downloads.name}",))
        self.addCleanup(host.close)
        return host

    @staticmethod
    def ended(host, download_id):
        return host.until(
            lambda event: event["event"] == "download"
            and event["id"] == download_id
            and event["state"] == "cancelled"
        )

    def test_a_saved_download_goes_to_its_tab_with_the_sites_cookies(self):
        host = self.host()
        host.send(op="create_tab", tab=2)
        host.until(lambda event: event["event"] == "tab_ready" and event.get("tab") == 2)
        host.send(op="navigate", tab=2, url=self.origin + "/signed-in")
        host.until(lambda event: event["event"] == "loaded" and event.get("tab") == 2)

        host.send(op="navigate", tab=2, url=self.origin + "/file?case=saved")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_answer", tab=2, id=prompt["id"], allow=True)
        handed = host.until(lambda event: event["event"] == "download_request")

        self.assertEqual(handed["tab"], 2, "only the tab that offered it hears its cookies")
        self.assertEqual(handed["id"], prompt["id"])
        self.assertEqual(handed["url"], self.origin + "/file?case=saved")
        self.assertEqual(handed["name"], "lifetime.txt")
        self.assertIn("session=abc", handed["cookies"], "HttpOnly cookies go with the download")
        self.assertEqual(handed["referrer"], self.origin + "/signed-in")
        self.ended(host, prompt["id"])
        host.settle(1)
        self.assertEqual(list(Path(self.downloads.name).rglob("*")), [], "the engine saved nothing")

    def test_a_download_turned_down_is_handed_to_nobody(self):
        host = self.host()
        host.send(op="download_start", url=self.origin + "/file?case=declined")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_answer", id=prompt["id"], allow=False)
        self.ended(host, prompt["id"])
        host.settle(1)
        self.assertFalse(any(event.get("event") == "download_request" for event in host.seen))

    def test_closing_a_tab_ends_the_download_it_offered(self):
        host = self.host()
        host.send(op="navigate", tab=1, url=self.origin + "/file?case=pending")
        pending = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="create_tab", tab=3)
        host.until(lambda event: event["event"] == "tab_ready" and event.get("tab") == 3)
        host.send(op="try_close_tab", tab=1)
        self.ended(host, pending["id"])
        host.until(lambda event: event["event"] == "closed" and event.get("tab") == 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
