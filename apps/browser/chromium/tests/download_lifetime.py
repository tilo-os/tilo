#!/usr/bin/env python3
"""Visible tabs detach while their native transfer owners finish or cancel."""

import http.server
import queue
import threading
import time
import unittest
from pathlib import Path
from urllib.parse import urlsplit

from native_regressions import Host

PAYLOAD = b"download-lifetime\n" * 65536


class Page(http.server.BaseHTTPRequestHandler):
    requests = []
    requests_lock = threading.Lock()

    def record(self):
        with self.requests_lock:
            self.requests.append((self.command, self.path))

    def attachment(self):
        self.record()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Disposition", "attachment; filename=lifetime.txt")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        try:
            for at in range(0, len(PAYLOAD), 16384):
                self.wfile.write(PAYLOAD[at : at + 16384])
                self.wfile.flush()
                time.sleep(0.02)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        if urlsplit(self.path).path == "/post-start":
            self.record()
            body = b"<form method=post action='/file?case=post'></form><script>document.forms[0].submit()</script>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.attachment()

    def do_POST(self):
        self.attachment()

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


class Lifetime(unittest.TestCase):
    def setUp(self):
        with Page.requests_lock:
            Page.requests.clear()
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = f"http://127.0.0.1:{self.server.server_port}/file"

    def host(self, args=()):
        host = DownloadHost(args)
        self.addCleanup(host.close)
        return host

    @staticmethod
    def terminal(host, download_id, state):
        return host.until(
            lambda event: event["event"] == "download"
            and event["id"] == download_id
            and event["state"] == state
        )

    def test_detached_multi_transfer_owner_closes_after_last_terminal_and_get_retries(self):
        host = self.host()
        host.send(op="create_tab", tab=2)
        host.until(lambda event: event["event"] == "tab_ready" and event.get("tab") == 2)

        host.send(op="navigate", tab=2, url=self.url + "?case=first")
        first = host.until(lambda event: event["event"] == "download_prompt")
        self.assertNotIn("tab", first)
        host.send(op="download_decision", tab=1, id=first["id"], allow=True)
        host.until(lambda event: event["event"] == "download" and event["id"] == first["id"] and event["received"] >= 32768)

        host.send(op="navigate", tab=2, url=self.url + "?case=second")
        second = host.until(lambda event: event["event"] == "download_prompt")
        self.assertNotEqual(second["id"], first["id"])
        host.send(op="download_decision", tab=1, id=second["id"], allow=True)
        host.until(lambda event: event["event"] == "download" and event["id"] == second["id"] and event["received"] >= 32768)

        host.send(op="try_close_tab", tab=2)
        host.until(lambda event: event["event"] == "tab_detached" and event.get("tab") == 2)
        host.send(op="create_tab", tab=32)
        host.until(lambda event: event["event"] == "tab_ready" and event.get("tab") == 32)
        host.send(op="download_cancel", tab=32, id=second["id"])
        cancelled = self.terminal(host, second["id"], "cancelled")
        self.assertTrue(cancelled["retryable"])
        self.assertFalse(
            any(event.get("event") == "closed" and event.get("tab") == 2 for event in host.seen),
            "one terminal transfer must not destroy an owner with another active transfer",
        )

        done = self.terminal(host, first["id"], "complete")
        self.assertEqual(Path(done["path"]).read_bytes(), PAYLOAD)
        host.until(lambda event: event["event"] == "closed" and event.get("tab") == 2)

        before_uri = next(event["url"] for event in reversed(host.seen) if event.get("event") == "uri" and event.get("tab") == 32)
        host.send(op="download_retry", tab=32, id=second["id"])
        retried = host.until(lambda event: event["event"] == "download_prompt")
        self.assertNotIn(retried["id"], (first["id"], second["id"]))
        host.send(op="download_decision", tab=32, id=retried["id"], allow=True)
        retry_done = self.terminal(host, retried["id"], "complete")
        self.assertEqual(Path(retry_done["path"]).read_bytes(), PAYLOAD)
        after_uri = next(event["url"] for event in reversed(host.seen) if event.get("event") == "uri" and event.get("tab") == 32)
        self.assertEqual(after_uri, before_uri, "retry must not navigate the active page")

        host.send(op="navigate", tab=1, url=self.url + "?case=pending")
        pending = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="create_tab", tab=3)
        host.until(lambda event: event["event"] == "tab_ready" and event.get("tab") == 3)
        host.send(op="try_close_tab", tab=1)
        self.terminal(host, pending["id"], "cancelled")
        host.until(lambda event: event["event"] == "closed" and event.get("tab") == 1)

    def test_download_address_keeps_the_page_and_obeys_native_consent(self):
        host = self.host()
        host.send(op="download_start", url=self.url + "?case=address")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        self.assertEqual(prompt["url"], self.url + "?case=address")
        self.assertFalse(any(event.get("event") == "uri" and event.get("url") == prompt["url"] for event in host.seen))
        host.send(op="download_decision", id=prompt["id"], allow=False)
        self.terminal(host, prompt["id"], "cancelled")

        host.send(op="download_start", url=self.url + "?case=approved")
        approved = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_decision", id=approved["id"], allow=True)
        done = self.terminal(host, approved["id"], "complete")
        self.assertEqual(Path(done["path"]).read_bytes(), PAYLOAD)

    def test_consented_filename_survives_later_progress_and_completion(self):
        host = self.host()
        host.send(op="download_start", url=self.url + "?case=filename")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        self.assertEqual(prompt["name"], "lifetime.txt")
        host.send(op="download_decision", id=prompt["id"], allow=True)
        done = self.terminal(host, prompt["id"], "complete")
        self.assertEqual(Path(done["path"]).read_bytes(), PAYLOAD)
        self.assertEqual(Path(done["path"]).name, prompt["name"])
        updates = [event for event in host.seen
                   if event.get("event") == "download" and event.get("id") == prompt["id"]]
        self.assertGreater(len(updates), 0)
        self.assertTrue(all(event["name"] == prompt["name"] for event in updates), updates)

    def test_post_retry_is_refused_while_accepting_then_cancel_all_acknowledges(self):
        host = self.host()
        host.send(op="navigate", tab=1, url=f"http://127.0.0.1:{self.server.server_port}/post-start")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_decision", tab=1, id=prompt["id"], allow=True)
        host.until(lambda event: event["event"] == "download" and event["id"] == prompt["id"] and event["received"] >= 32768)
        host.send(op="download_cancel", tab=1, id=prompt["id"])
        cancelled = self.terminal(host, prompt["id"], "cancelled")
        self.assertFalse(cancelled["retryable"])
        prompt_count = sum(event.get("event") == "download_prompt" for event in host.seen)
        host.send(op="download_retry", tab=1, id=prompt["id"])
        host.settle(0.5)
        self.assertEqual(sum(event.get("event") == "download_prompt" for event in host.seen), prompt_count)
        self.assertTrue(
            next(event for event in reversed(host.seen) if event.get("event") == "download_runtime")["accepting"],
            "POST retry refusal must not depend on the cancel-all barrier",
        )
        with Page.requests_lock:
            self.assertIn(("POST", "/file?case=post"), Page.requests)
            self.assertNotIn(("GET", "/file?case=post"), Page.requests)

        host.send(op="navigate", tab=1, url=self.url + "?case=cancel-all")
        active = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_decision", tab=1, id=active["id"], allow=True)
        host.until(lambda event: event["event"] == "download" and event["id"] == active["id"] and event["received"] >= 32768)
        host.send(op="download_cancel_all", tab=1)
        self.terminal(host, active["id"], "cancelled")
        runtime = host.until(
            lambda event: event["event"] == "download_runtime"
            and event["admitted"] == 0
            and not event["accepting"]
        )
        self.assertEqual(runtime["detached"], 0)

    def test_destination_failure_settles_without_detached_owner(self):
        host = self.host(("--downloads=/dev/null/tilo-downloads",))
        host.send(op="navigate", tab=1, url=self.url + "?case=destination-failure")
        prompt = host.until(lambda event: event["event"] == "download_prompt")
        host.send(op="download_decision", tab=1, id=prompt["id"], allow=True)
        failed = self.terminal(host, prompt["id"], "failed")
        self.assertEqual(failed["error"], "Cannot create download directory")
        self.assertTrue(failed["retryable"])
        runtime = host.until(lambda event: event["event"] == "download_runtime" and event["admitted"] == 0)
        self.assertEqual(runtime["detached"], 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
