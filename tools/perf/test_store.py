"""Tests of the performance results store (tools/perf/store.py)."""

from __future__ import annotations

import contextlib
import datetime
import http.server
import io
import json
import sys
import tempfile
import threading
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import record  # noqa: E402
import store  # noqa: E402

ENV = {"commit": "a" * 40, "profile": "dev", "renderer": "none", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu", "started_at": "2026-10-09T01:15:00Z"}
LINE = record.from_samples("fs.a-b", "ms", [1.0, 2.0], **ENV).to_json() + "\n"


class Naming(unittest.TestCase):
    def test_months_back_crosses_the_year(self):
        now = datetime.datetime(2026, 2, 3, tzinfo=datetime.timezone.utc)
        self.assertEqual(store.months_back(now, 3), ["2026-02", "2026-01", "2025-12"])

    def test_file_names_sort_by_start_and_are_unique_per_run(self):
        self.assertEqual(store.file_name("2026-10-09T01:15:00Z", "84600-1"), "20261009T011500Z-84600-1.jsonl")
        with self.assertRaises(ValueError):
            store.file_name("2026-10-09T01:15:00Z", "../x")


class Local(unittest.TestCase):
    def test_reads_the_listed_months(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for month in ("2026-10", "2026-06"):
                (root / month).mkdir()
                (root / month / "20261009T011500Z-1-1.jsonl").write_text(LINE)
            local = store.LocalStore(root)
            self.assertEqual(local.files(["2026-10", "2026-09"]), [("2026-10", "20261009T011500Z-1-1.jsonl")])
            self.assertEqual(len(store.history(local, ["2026-10", "2026-06"])), 2)


class Fetch(unittest.TestCase):
    """`fetch` prints measurements; a gate test's multiplied records only on request."""

    def fetch(self, *extra: str) -> tuple[list[dict], str]:
        real = record.from_samples("footprint.tapp.bytes.x86_64", "bytes", [2_000_000.0], **ENV)
        fake = record.from_samples("footprint.tapp.bytes.x86_64", "bytes", [2_500_000.0],
                                   meta={"synthetic_series": "gate-test-check", "synthetic_factor": "1.25"}, **ENV)
        with tempfile.TemporaryDirectory() as directory:
            month = store.months_back(datetime.datetime.now(datetime.timezone.utc))[0]
            (Path(directory) / month).mkdir()
            (Path(directory) / month / "20261009T011500Z-1-1.jsonl").write_text(real.to_json() + "\n")
            (Path(directory) / month / "20261009T013000Z-2-1.jsonl").write_text(fake.to_json() + "\n")
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                self.assertEqual(store.main(["fetch", directory, *extra]), 0)
        return [json.loads(line) for line in out.getvalue().splitlines()], err.getvalue()

    def test_fetch_omits_synthetic_records_by_default(self):
        printed, err = self.fetch()
        self.assertEqual([item["p50"] for item in printed], [2_000_000.0])
        self.assertIn("omitted 1 synthetic gate-test records", err)

    def test_fetch_synthetic_includes_them(self):
        printed, err = self.fetch("--synthetic")
        self.assertEqual([item["p50"] for item in printed], [2_000_000.0, 2_500_000.0])
        self.assertEqual(err, "")

    def test_history_keeps_them_for_compare(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "2026-10").mkdir()
            item = record.from_samples("fs.a-b", "ms", [1.0], meta={"synthetic_series": "s"}, **ENV)
            (Path(directory) / "2026-10" / "20261009T011500Z-1-1.jsonl").write_text(item.to_json() + "\n")
            [kept] = store.history(store.LocalStore(Path(directory)), ["2026-10"])
            self.assertTrue(store.synthetic(kept))


class Handler(http.server.BaseHTTPRequestHandler):
    files = {"2026-10": {"20261009T011500Z-7-1.jsonl": LINE, "notes.txt": "ignored"}}

    def log_message(self, *args):
        pass

    def do_GET(self):
        listing = "/api/v1/packages/tilo/generic/tilo-perf-results/"
        download = "/api/packages/tilo/generic/tilo-perf-results/"
        body = None
        if self.path.startswith(listing) and self.path.endswith("/files"):
            month = self.path[len(listing):-len("/files")]
            if month in self.files:
                body = json.dumps([{"name": name, "Size": len(text)} for name, text in self.files[month].items()])
        elif self.path.startswith(download):
            month, _, name = self.path[len(download):].partition("/")
            body = self.files.get(month, {}).get(name)
        if body is None:
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.end_headers()
        self.wfile.write(body.encode())


class Package(unittest.TestCase):
    def test_lists_and_reads_without_credentials(self):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            remote = store.open_store(f"http://127.0.0.1:{server.server_port}")
            self.assertIsInstance(remote, store.PackageStore)
            self.assertEqual(remote.files(["2026-10", "2026-09"]), [("2026-10", "20261009T011500Z-7-1.jsonl")])
            [item] = store.history(remote, ["2026-10", "2026-09"])
            self.assertEqual(item.kpi, "fs.a-b")
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
