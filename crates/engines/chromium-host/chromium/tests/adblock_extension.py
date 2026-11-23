#!/usr/bin/env python3
"""An installed ad blocker blocks ads in the real engine.

Ad blockers in today's Chrome (uBlock Origin Lite, AdGuard, Adblock Plus) are
Manifest V3 extensions that block requests by declarativeNetRequest rules.
A fixture blocker with one such rule (block `/ads/`) keeps a page's ad script
and its tracking request from loading, when added to the running engine as
the Browser adds one from the Web Store, and when the engine starts with it,
as after a restart. Without it the same page loads both. A second rule puts
the blocker's own stand-in in place of the page's analytics script, at an
address of its own for the session (`use_dynamic_url`), as uBlock Origin
Lite does: the page's calls to the analytics API then still work.
"""
import http.server
import json
from pathlib import Path
import shutil
import tempfile
import threading
import time
import unittest
from native_regressions import HANG_BOUND, Host

BLOCKED = {'ad': 'blocked', 'tracking': 'untracked', 'analytics': 'stand-in'}
SHOWN = {'ad': 'shown', 'tracking': 'tracked', 'analytics': 'site'}

SOURCE = Path(__file__).parent.joinpath('adblock-fixture')
# Loaded from a copy: Chromium writes its compiled rules beside the manifest.
FIXTURE = ''


def setUpModule():
    global FIXTURE
    copy = tempfile.TemporaryDirectory(prefix='tilo-adblock-fixture-')
    unittest.addModuleCleanup(copy.cleanup)
    FIXTURE = str(Path(shutil.copytree(SOURCE, Path(copy.name, 'blocker'))).resolve())
PAGE = b'''<!doctype html><title>loading</title>
<script src="/ads/ad.js"></script>
<script src="/analytics/ga.js"></script>
<script>
addEventListener('load', async () => {
  let tracked = 'tracked';
  try { await fetch('/ads/beacon'); } catch (error) { tracked = 'untracked'; }
  let analytics = 'missing';
  try { ga('send', 'pageview'); analytics = ga.standIn ? 'stand-in' : 'site'; } catch (error) {}
  document.title = JSON.stringify({ad: window.adShown ? 'shown' : 'blocked', tracking: tracked, analytics});
});
</script>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def handle(self):
        # A page left before its response was read closes the connection.
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        self.send_response(200)
        if self.path == '/ads/ad.js':
            self.send_header('Content-Type', 'text/javascript')
            self.end_headers()
            self.wfile.write(b'window.adShown = true;')
        elif self.path == '/analytics/ga.js':
            self.send_header('Content-Type', 'text/javascript')
            self.end_headers()
            self.wfile.write(b'window.ga = function () {};')
        elif self.path == '/ads/beacon':
            self.send_header('Content-Type', 'text/plain')
            self.end_headers()
            self.wfile.write(b'ok')
        else:
            self.send_header('Content-Type', 'text/html')
            self.end_headers()
            self.wfile.write(PAGE)

    def log_message(self, *args):
        pass


class AdBlocking(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def page(self, host, query):
        host.send(op='navigate', url=f'{self.url}/?{query}')
        title = host.until(lambda e: e['event'] == 'title' and e['title'].startswith('{'))['title']
        return json.loads(title)

    def page_settled(self, host, query, expected):
        """The page as it loads once the blocker's rules are in or out,
        which the engine applies a moment after it adds or removes it: a
        loaded runner takes longer than an idle one."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            seen = self.page(host, query)
            if seen == expected or time.monotonic() > deadline:
                return seen
            host.settle(0.5)

    def devtools(self, host, method, params):
        self.calls = getattr(self, 'calls', 0) + 1
        host.send(op='devtools_call', request_id=self.calls, method=method, params=params)
        return host.until(lambda e: e['event'] == 'devtools_result' and e['request_id'] == self.calls)

    def test_without_a_blocker_the_page_shows_its_ad(self):
        host = Host()
        self.addCleanup(host.close)
        self.assertEqual(self.page(host, 'plain'), SHOWN)

    def test_a_blocker_added_while_browsing_blocks_ads_and_removed_stops(self):
        host = Host()
        self.addCleanup(host.close)
        loaded = self.devtools(host, 'Extensions.loadUnpacked', {'path': FIXTURE})
        self.assertTrue(loaded['ok'], loaded)
        extension = json.loads(loaded['body'])['id']
        self.assertEqual(self.page_settled(host, 'added', BLOCKED), BLOCKED)
        removed = self.devtools(host, 'Extensions.uninstall', {'id': extension})
        self.assertTrue(removed['ok'], removed)
        self.assertEqual(self.page_settled(host, 'removed', SHOWN), SHOWN)

    def test_a_blocker_the_engine_starts_with_blocks_ads(self):
        host = Host(args=['--load-extension=' + FIXTURE])
        self.addCleanup(host.close)
        self.assertEqual(self.page_settled(host, 'startup', BLOCKED), BLOCKED)


if __name__ == '__main__':
    unittest.main(verbosity=2)
