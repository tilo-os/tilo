#!/usr/bin/env python3
"""A real page hands its whole frame over when its window's share is watched.

Usage: page_share.py CEF_HOST PAGE_SHARE_PROBE

The probe (the toolkit's `page-share-probe` example) runs the engine as Browser's
windows run it, drawn from its pixels and captured as a display list only while a
session shares the window with someone watching, draws it with the web view a
window draws it with, and says the window is watched as the session's ask for a
whole frame does. What the share's outbox then holds is what the app sends its
session. The page is the form the remote end-to-end run's machines answer, at the
size its window has there and from the same kind of origin (a loopback server).
No personal profile, display or network beyond that loopback.
"""
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest

if len(sys.argv) != 3:
    raise SystemExit(__doc__)
HOST = Path(sys.argv[1]).resolve(strict=True)
PROBE = Path(sys.argv[2]).resolve(strict=True)

# How long an app has to hand its page over once asked, here: the whole of it, the
# engine's snapshot and the frames around it, on a machine without a GPU.
WITHIN_MS = 5000
# The window the machines share: its page's size there.
WIDTH, HEIGHT = 604, 708

FORM = b"""<!doctype html><title>Apply</title><h1>Apply for a visitor visa</h1>
<form action="/sent" method="get">
<label>Given names <input name="given" value="Amina"></label>
<label>Family name <input name="family" value="Otieno"></label>
<label>Nationality <select name="nationality" id="nat"><option value="">Select a country</option>
<option selected>Kenya</option><option>Other</option></select></label>
<div id="other" hidden><label>Which country? <input name="other" required></label></div>
<label>Passport number <input name="passport" value="A1234567"></label>
<fieldset><legend>Refused before?</legend>
<label><input type="radio" name="refused" value="yes"> Yes</label>
<label><input type="radio" name="refused" value="no" checked> No</label></fieldset>
<label>Arrival <input type="date" name="arrival" value="2027-03-01"></label>
<button>Send application</button></form>
<script>document.getElementById('nat').addEventListener('change', e =>
document.getElementById('other').hidden = e.target.value !== 'Other');</script>
"""
# What the form shows whoever looks at it: its heading and three of its labels.
SHOWN = ("Apply for a visitor visa", "Given names", "Family name", "Passport number")
# What its fields hold.
HELD = ("Amina", "Otieno", "A1234567")


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(FORM)))
        self.end_headers()
        self.wfile.write(FORM)

    def log_message(self, *_):
        pass


class PageShare(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}/"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def probe(self, *arguments, expect=SHOWN):
        """Run the probe on the form and return what it printed: one object."""
        runtime = tempfile.mkdtemp(prefix="tilo-page-share-")
        try:
            env = dict(os.environ,
                       XDG_RUNTIME_DIR=runtime,
                       XDG_CONFIG_HOME=runtime + "/config",
                       XDG_CACHE_HOME=runtime + "/cache",
                       XDG_DATA_HOME=runtime + "/data")
            env.pop("DISPLAY", None)
            env.pop("WAYLAND_DISPLAY", None)
            command = [str(PROBE), str(HOST), runtime + "/profile", self.url,
                       str(WIDTH), str(HEIGHT), "--within-ms", str(WITHIN_MS), *arguments]
            for text in expect:
                command += ["--expect", text]
            ran = subprocess.run(command, env=env, stdout=subprocess.PIPE, timeout=180)
            said = [line for line in ran.stdout.decode(errors="replace").splitlines()
                    if line.startswith("{")]
            self.assertEqual(ran.returncode, 0, ran.stdout[-2000:])
            self.assertTrue(said, "the probe printed no result")
            result = json.loads(said[-1])
            print(f"\n  probe {' '.join(arguments) or '(watched once loaded)'}: "
                  f"{json.dumps({k: v for k, v in result.items() if k != 'texts'})}", flush=True)
            return result
        finally:
            # The engine's storage children can finish after the host exits.
            deadline = time.monotonic() + 10
            while Path(runtime).exists():
                try:
                    shutil.rmtree(runtime)
                except OSError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.05)

    def handed_over(self, result, what):
        self.assertTrue(result["whole"], f"{what}: no whole frame of the page came: {result}")
        self.assertFalse(result["missing"],
                         f"{what}: the page handed over lacks {result['missing']}; "
                         f"it carries {result['texts']}")
        self.assertIsNotNone(result["complete_ms"], result)
        self.assertLess(result["complete_ms"], WITHIN_MS, result)
        self.assertGreater(result["whole_bytes"], 0, result)
        self.assertLessEqual(result["largest_payload"], result["largest_carried"],
                             "every payload fits what the app's channel to its session carries")

    def test_a_page_asked_for_is_handed_over_whole_within_seconds(self):
        self.handed_over(self.probe(), "a window watched once its page loaded")

    def test_a_share_watched_before_the_page_loaded_comes_to_hold_the_page(self):
        self.handed_over(self.probe("--watched-before-load"),
                         "a window watched before its page loaded")

    def test_the_page_handed_over_carries_what_its_fields_hold(self):
        self.handed_over(self.probe(expect=SHOWN + HELD), "a form that was answered")


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1], verbosity=2)
