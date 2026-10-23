#!/usr/bin/env python3
"""Extensions see the Browser's tabs, as they see Chrome's.

Chromium gives extensions no tabs in an offscreen embedding; the host answers
their tab APIs itself (src/extension_tabs.h). With real Chromium:

- A toolbar button without a popup reaches the extension (action.onClicked)
  with the page it was clicked over, which it may then script (activeTab).
- A popup's "current tab" is the page it was opened over, with its address
  for an extension with the tabs permission and without it for one without.
- A worker and its content script talk both ways by the tab's id, and the id
  a message comes from is one the tab APIs know.
- A popup can open a tab, which the Browser is asked to open, and is given
  the tab once it opens.
- A worker hears of a page that finished loading.
- A click reaches a worker that went to sleep, which wakes for it.
- A script a click injects where the extension has no content script of its
  own gets a content script's storage, i18n and messaging, both ways.
"""
import hashlib
import http.server
import json
from pathlib import Path
import secrets
import shutil
import tempfile
import threading
import time
import unittest
from native_regressions import HANG_BOUND, Host

FIXTURES = Path(__file__).parent.joinpath('extension-tabs').resolve()
# The Browser's own extension, loaded with every profile: it tells which tab
# a page is.
MANAGER = Path(__file__).parents[3].joinpath('browser-parts/assets/extension-manager').resolve()
SCRIPT = Path(__file__).parents[1].joinpath('src/extension_tabs.js').read_text()


def prepare(source, into):
    """A copy of the fixture at `source` set up as the Browser sets up an
    extension it installs (extensions.rs, prepare_tabs): its worker runs the
    tab script first, with a token the engine is told of."""
    target = Path(shutil.copytree(source, into / source.name)).resolve()
    manifest = json.loads((target / 'manifest.json').read_text())
    worker = manifest.get('background', {}).get('service_worker')
    if not worker:
        return target, None
    token = secrets.token_hex(24)
    (target / 'tilo-extension-tabs.js').write_text(f'globalThis.__tiloBridgeToken = "{token}";\n' + SCRIPT)
    own = worker.lstrip('/')
    if manifest['background'].get('type') == 'module':
        runs = f"import '/tilo-extension-tabs.js';\nimport '/{own}';\n"
    else:
        runs = f"importScripts('/tilo-extension-tabs.js', '/{own}');\n"
    (target / 'tilo-worker.js').write_text(f"// The extension's own worker: {own}\n" + runs)
    manifest['background']['service_worker'] = 'tilo-worker.js'
    (target / 'manifest.json').write_text(json.dumps(manifest))
    return target, token


def extension_id(path):
    """The id Chromium gives an extension loaded from `path`."""
    digest = hashlib.sha256(str(path).encode()).hexdigest()[:32]
    return ''.join(chr(ord('a') + int(c, 16)) for c in digest)


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


class ExtensionTabs(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def start(self, *names, access):
        """A host with the fixtures `names` loaded, each granted what the
        Browser grants an installed extension (`access`: name -> (permissions,
        hosts))."""
        installed = tempfile.TemporaryDirectory(prefix='tilo-extension-tabs-')
        self.addCleanup(installed.cleanup)
        prepared = [prepare(FIXTURES / name, Path(installed.name)) for name in names]
        paths = [path for path, _ in prepared]
        host = Host(args=['--use-mock-keychain', '--load-extension=' + ','.join(map(str, [MANAGER, *paths]))])
        self.addCleanup(host.close)
        host.send(op='extension_identity', id=extension_id(MANAGER))
        # The Browser tells the engine what each extension may do once it has
        # read its profile: after the extensions' workers started.
        host.settle(1.0)
        ids = {}
        for name, (path, token) in zip(names, prepared):
            ids[name] = extension_id(path)
            permissions, hosts = access.get(name, ([], []))
            command = {'id': ids[name], 'permissions': permissions, 'hosts': hosts}
            if token:
                command['token'] = token
            host.send(op='extension_access', **command)
        host.send(op='resize', width=800, height=600, scale_factor=1.0)
        host.send(op='focus', focused=True)
        return host, ids

    def title(self, host, tab, prefix):
        """The tab's first title starting with `prefix`, also one that came
        while the test waited for something else."""
        match = lambda e: e['event'] == 'title' and e.get('tab', 1) == tab and e['title'].startswith(prefix)
        seen = next((e for e in host.seen if match(e)), None)
        return (seen or host.until(match))['title']

    def panel(self, host, tab, url):
        """An extension page shown in a panel over tab 1, as the Browser
        shows a popup."""
        host.send(op='create_tab', tab=tab)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == tab)
        host.send(op='extension_panel', tab=tab)
        host.send(op='resize', tab=tab, width=360, height=200, scale_factor=1.0)
        host.send(op='navigate', tab=tab, url=url)

    def test_a_click_reaches_an_extension_without_a_popup_and_lets_it_script_the_page(self):
        host, ids = self.start('clicker', access={'clicker': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/clicked')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['clicker'])
        # Its second script sees what its first returned.
        self.title(host, 1, 'clicked:true|clicked:true')

    def test_a_popup_finds_the_page_it_was_opened_over(self):
        host, ids = self.start('asker', 'plain', access={'asker': (['tabs'], [])})
        page = self.url + '/opened-over'
        host.navigate(page)
        self.panel(host, 2, f'chrome-extension://{ids["asker"]}/popup.html')
        # The tab it opens: the Browser opens it beside, which here the test
        # does, and the popup is given it.
        opened = host.until(lambda e: e['event'] == 'open_tab')
        self.assertEqual((opened['url'], opened['background']), (self.url + '/created', True))
        host.send(op='create_tab', tab=4)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 4)
        host.send(op='navigate', tab=4, url=opened['url'])
        asked = json.loads(self.title(host, 2, 'asked:')[len('asked:'):])
        self.assertEqual(asked, {'url': page, 'window': True, 'own': True, 'all': 1, 'callback': 1, 'created': self.url + '/created'})
        # Without the tabs permission it finds the tab, not its address.
        self.panel(host, 3, f'chrome-extension://{ids["plain"]}/popup.html')
        plain = json.loads(self.title(host, 3, 'plain:')[len('plain:'):])
        self.assertEqual(plain, {'found': True, 'url': True})

    def test_a_worker_and_its_content_script_talk_both_ways_by_tab(self):
        host, ids = self.start('messenger', access={'messenger': ([], ['http://127.0.0.1/*'])})
        host.navigate(self.url + '/talk')
        answer = json.loads(self.title(host, 1, 'messenger:')[len('messenger:'):])
        self.assertEqual(answer, {'known': True, 'pong': 'pong:/talk:true'})


    def test_a_script_injected_on_a_click_works_as_a_content_script(self):
        host, ids = self.start('injector', access={'injector': (['activeTab', 'scripting', 'storage'], [])})
        host.navigate(self.url + '/injected')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['injector'])
        title = self.title(host, 1, 'injected:')
        if '|' not in title:
            title = self.title(host, 1, title + '|')
        injected, back = title[len('injected:'):].split('|')
        self.assertEqual(json.loads(injected), {'color': 'green', 'known': True, 'greeting': 'hello tilo', 'url': True})
        self.assertEqual(back, 'back:true')

    def test_a_worker_hears_of_a_page_that_finished_loading(self):
        host, ids = self.start('watcher', access={'watcher': (['scripting'], ['http://127.0.0.1/*'])})
        host.settle(1.0)
        host.navigate(self.url + '/watched')
        self.assertRegex(self.title(host, 1, 'watched:'), r'^watched:\d+$')

    def test_a_click_wakes_a_worker_that_went_to_sleep(self):
        host, ids = self.start('clicker', access={'clicker': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/later')
        calls = iter(range(1, 1000))

        def workers():
            request = next(calls)
            host.send(op='devtools_call', request_id=request, method='Target.getTargets', params={})
            reply = host.until(lambda e: e['event'] == 'devtools_result' and e['request_id'] == request)
            return [t for t in json.loads(reply['body'])['targetInfos'] if t['type'] == 'service_worker']

        # Chromium stops an idle extension worker after thirty seconds; the
        # host never keeps one awake.
        deadline = time.monotonic() + max(HANG_BOUND, 60)
        while workers():
            self.assertLess(time.monotonic(), deadline, 'the worker never went to sleep')
            host.settle(2.0)
        host.send(op='extension_click', id=ids['clicker'])
        self.title(host, 1, 'clicked:true|clicked:true')


if __name__ == '__main__':
    unittest.main(verbosity=2)
