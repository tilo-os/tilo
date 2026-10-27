#!/usr/bin/env python3
"""Turning an extension off and on keeps its data.

Chromium's DevTools can only uninstall an extension, which deletes what it
stored: an ad blocker's settings and allowed sites, a password manager's
login. The Browser turns extensions off and on instead through its own
manager extension (apps/browser-parts/assets/extension-manager), with
`chrome.management.setEnabled`, as Chrome's extensions page does. The MV3
fixture counts its visits in chrome.storage: off and on again, it goes on
counting; uninstalled and added again, it starts over.
"""
import json
import time
from pathlib import Path
import unittest
import capabilities
from native_regressions import HANG_BOUND, Host
from extensions import extension_page

TESTS = Path(__file__).parent
FIXTURE = str(TESTS.joinpath('mv3-fixture').resolve())
MANAGER = str(TESTS.parents[1].joinpath('..', 'browser-parts', 'assets', 'extension-manager').resolve())


class ExtensionToggle(unittest.TestCase):
    # The capability tests' page server, on this class.
    @classmethod
    def setUpClass(cls):
        capabilities.Capabilities.setUpClass.__func__(cls)

    @classmethod
    def tearDownClass(cls):
        capabilities.Capabilities.tearDownClass.__func__(cls)

    def setUp(self):
        self.h = Host()
        self.addCleanup(self.h.close)
        self.calls = 0

    def devtools(self, method, params):
        self.calls += 1
        self.h.send(op='devtools_call', request_id=self.calls, method=method, params=params)
        return self.h.until(lambda e: e['event'] == 'devtools_result' and e['request_id'] == self.calls)

    def visit(self, query):
        return extension_page(self.h, f'{self.url}/?{query}')

    def visit_off(self, query):
        self.h.navigate(f'{self.url}/?{query}')
        return self.h.latest_title().startswith('{')

    def switch(self, manager, extension, enabled):
        side = 'on' if enabled else 'off'
        self.h.navigate(f'chrome-extension://{manager}/manage.html?{side}={extension}')
        deadline = time.monotonic() + HANG_BOUND
        while self.h.latest_title() == 'managing' and time.monotonic() < deadline:
            self.h.settle(.2)
        self.assertEqual(self.h.latest_title(), 'done')
        self.h.settle(1.0)

    def test_off_and_on_keeps_the_extensions_data(self):
        loaded = self.devtools('Extensions.loadUnpacked', {'path': FIXTURE})
        self.assertTrue(loaded['ok'], loaded)
        extension = json.loads(loaded['body'])['id']
        manager = self.manager()
        first = self.visit('first')['count']
        self.switch(manager, extension, False)
        self.assertFalse(self.visit_off('off'), 'off, it does not run')
        self.switch(manager, extension, True)
        self.assertEqual(self.visit('on')['count'], first + 1, 'on again, it remembers')

    def manager(self):
        loaded = self.devtools('Extensions.loadUnpacked', {'path': MANAGER})
        self.assertTrue(loaded['ok'], loaded)
        return json.loads(loaded['body'])['id']


if __name__ == '__main__':
    unittest.main(verbosity=2)
