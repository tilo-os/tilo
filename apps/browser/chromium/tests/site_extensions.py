#!/usr/bin/env python3
"""Extensions that change the sites a person visits work as they do in Chrome.

Video speed controllers, SponsorBlock, Return YouTube Dislike, dark modes and
reading aids were each installed from the Web Store and used on real pages;
these fixtures keep what broke, without the store or the sites:

- A keyboard shortcut a content script takes counts every press: a page sees
  each key at its own time, so a script that drops a press it saw already
  (by key and time, as video speed controllers do) drops none.
- An extension added while a page is open runs its script there, as High
  Contrast and dark modes do once installed, and that script follows the
  settings the person changes in its popup and gets its worker's answers,
  bound as Chrome binds a null or undefined extension id.
"""
import http.server
import json
from pathlib import Path
import tempfile
import threading
import unittest
from extension_tabs import MANAGER, extension_id, prepare
from native_regressions import Host

FIXTURES = Path(__file__).parent.joinpath('site-extensions').resolve()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f'<!doctype html><title>page {self.path}</title><body>page</body>'.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class SiteExtensions(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def host(self, *loaded):
        """A host with the Browser's own extension and the fixtures `loaded`."""
        installed = tempfile.TemporaryDirectory(prefix='tilo-site-extensions-')
        self.addCleanup(installed.cleanup)
        self.installed = Path(installed.name)
        paths = [prepare(FIXTURES / name, self.installed)[0] for name in loaded]
        host = Host(args=['--use-mock-keychain', '--load-extension=' + ','.join(map(str, [MANAGER, *paths]))])
        self.addCleanup(host.close)
        host.send(op='extension_identity', id=extension_id(MANAGER))
        host.send(op='resize', width=800, height=600, scale_factor=1.0)
        host.send(op='focus', focused=True)
        return host

    def title(self, host, tab, accept):
        match = lambda e: e['event'] == 'title' and e.get('tab', 1) == tab and accept(e['title'])
        seen = next((e for e in reversed(host.seen) if match(e)), None)
        return (seen or host.until(match))['title']

    def test_a_shortcut_pressed_three_times_counts_three(self):
        host = self.host('shortcut')
        host.navigate(self.url + '/keys')
        for presses in (1, 2, 3):
            host.send(op='key', key='d', down=True)
            host.send(op='key', key='d', down=False)
            self.title(host, 1, lambda title, presses=presses: title == f'presses:{presses}')

    def test_an_extension_added_while_a_page_is_open_follows_its_settings_there(self):
        host = self.host()
        host.navigate(self.url + '/open')
        # Added as the Browser adds one from the Web Store: into the running
        # engine, then told what it may do once the Browser has read it.
        path, token = prepare(FIXTURES / 'settler', self.installed)
        host.send(op='devtools_call', request_id=1, method='Extensions.loadUnpacked', params={'path': str(path)})
        loaded = host.until(lambda e: e['event'] == 'devtools_result' and e['request_id'] == 1)
        self.assertTrue(loaded['ok'], loaded)
        extension = json.loads(loaded['body'])['id']
        host.send(op='extension_access', id=extension, permissions=['scripting', 'storage'],
                  hosts=['<all_urls>'], token=token)
        # Its install-time script reached the open page and its worker answered
        # it, whether the script named no extension with null or undefined.
        greeted = self.title(host, 1, lambda title: title.startswith('settler:')
                             and all(f'"{key}":' in title for key in ('reply', 'again', 'port')))
        self.assertEqual(json.loads(greeted[len('settler:'):]),
                         {'reply': 'hello /open', 'again': 'hello again', 'port': 'settings'})
        # The person picks a setting in its popup.
        host.send(op='create_tab', tab=2)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 2)
        host.send(op='extension_panel', tab=2, action=extension)
        host.send(op='resize', tab=2, width=360, height=200, scale_factor=1.0)
        host.send(op='navigate', tab=2, url=f'chrome-extension://{extension}/popup.html')
        followed = self.title(host, 1, lambda title: title.startswith('settler:') and '"local":' in title and '"area":' in title)
        self.assertEqual(json.loads(followed[len('settler:'):]),
                         {'reply': 'hello /open', 'again': 'hello again', 'port': 'settings',
                          'area': 'local', 'mode': 'dark', 'local': 'dark'})


if __name__ == '__main__':
    unittest.main(verbosity=2)
