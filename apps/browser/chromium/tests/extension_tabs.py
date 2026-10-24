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
- A tab is captured as it shows, as PNG or JPEG at the quality asked for, by
  an extension given it by a click or with access to every site, and by no
  other; at most twice a second, as Chrome allows.
"""
import base64
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
import zlib
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


# A page to capture: a red block in its top left corner above a white gap,
# then a gradient and text, which a JPEG keeps more of the better its quality.
SHOT = (
    '<!doctype html><title>shot</title><body style="margin:0;background:#fff">'
    '<div style="width:200px;height:100px;background:#f00"></div>'
    '<div style="height:40px"></div>'
    '<div style="height:300px;background:linear-gradient(90deg,#123,#9cf,#fc3,#063)"></div>'
    + '<p>Captured as the page shows</p>' * 6
)


def png_pixels(data):
    """Width, height and rows of RGB pixels of an 8-bit RGB or RGBA PNG."""
    assert data[:8] == b'\x89PNG\r\n\x1a\n', 'not a PNG'
    at, idat, header = 8, b'', None
    while at < len(data):
        length = int.from_bytes(data[at:at + 4], 'big')
        kind, body = data[at + 4:at + 8], data[at + 8:at + 8 + length]
        at += 12 + length
        if kind == b'IHDR':
            header = body
        elif kind == b'IDAT':
            idat += body
    width, height = int.from_bytes(header[:4], 'big'), int.from_bytes(header[4:8], 'big')
    depth, color, interlace = header[8], header[9], header[12]
    assert depth == 8 and color in (2, 6) and interlace == 0, f'unexpected PNG {header.hex()}'
    channels = 3 if color == 2 else 4
    raw, stride, rows, previous = zlib.decompress(idat), width * channels, [], bytes(width * channels)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for x in range(stride):
            left = line[x - channels] if x >= channels else 0
            up = previous[x]
            corner = previous[x - channels] if x >= channels else 0
            if kind == 1:
                line[x] = (line[x] + left) & 0xFF
            elif kind == 2:
                line[x] = (line[x] + up) & 0xFF
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 0xFF
            elif kind == 4:
                guess = left + up - corner
                pick = min((abs(guess - left), 0, left), (abs(guess - up), 1, up), (abs(guess - corner), 2, corner))[2]
                line[x] = (line[x] + pick) & 0xFF
        previous = bytes(line)
        rows.append([tuple(previous[i:i + 3]) for i in range(0, stride, channels)])
    return width, height, rows


def jpeg_size(data):
    """Width and height of a baseline or progressive JPEG."""
    assert data[:2] == b'\xff\xd8', 'not a JPEG'
    at = 2
    while at < len(data):
        marker, length = data[at + 1], int.from_bytes(data[at + 2:at + 4], 'big')
        if marker in (0xC0, 0xC1, 0xC2):
            return int.from_bytes(data[at + 7:at + 9], 'big'), int.from_bytes(data[at + 5:at + 7], 'big')
        at += 2 + length
    raise AssertionError('JPEG without a frame header')


def image(url, kind):
    prefix = f'data:image/{kind};base64,'
    assert url.startswith(prefix), url[:40]
    return base64.b64decode(url[len(prefix):])


class Handler(http.server.BaseHTTPRequestHandler):
    # What the fixtures' workers report (`POST /report/<name>`), by name.
    reports = {}
    reported = threading.Condition()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        with Handler.reported:
            Handler.reports[self.path.rsplit('/', 1)[-1]] = json.loads(body)
            Handler.reported.notify_all()
        self.send_response(204)
        self.end_headers()

    def do_GET(self):
        page = SHOT if self.path == '/shot' else f'<!doctype html><title>page {self.path}</title><body>page</body>'
        body = page.encode()
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

    def report(self, name):
        """What the fixture `name` reported, once it has."""
        deadline = time.monotonic() + HANG_BOUND
        with Handler.reported:
            while name not in Handler.reports:
                left = deadline - time.monotonic()
                self.assertGreater(left, 0, f'{name} never reported')
                Handler.reported.wait(left)
            return Handler.reports.pop(name)

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

    def test_a_tab_is_captured_as_it_shows_by_an_extension_chrome_lets(self):
        host, ids = self.start('capturer', 'sweeper', 'peeker', access={
            'capturer': (['activeTab'], []),
            'sweeper': ([], ['<all_urls>']),
            'peeker': (['tabs'], ['http://127.0.0.1/*']),
        })
        host.settle(1.0)
        host.navigate(self.url + '/shot')
        refused = "Either the '<all_urls>' or 'activeTab' permission is required."
        # Access to one site and every tab's address is not enough.
        self.assertEqual(self.report('peeker'), {'ok': False, 'value': refused})
        # Access to every site is, without a click.
        swept = self.report('sweeper')
        self.assertTrue(swept['ok'], swept['value'])
        width, height, rows = png_pixels(image(swept['value'], 'png'))
        self.assertEqual((width, height), (800, 600))
        host.settle(1.0)
        host.send(op='extension_click', id=ids['capturer'])
        captured = self.report('capturer')
        # Before the click, activeTab gives nothing.
        self.assertEqual(captured['before'], {'ok': False, 'value': refused})
        for name in ('png', 'low', 'high'):
            self.assertTrue(captured[name]['ok'], f"{name}: {captured[name]['value']}")
        width, height, rows = png_pixels(image(captured['png']['value'], 'png'))
        self.assertEqual((width, height), (800, 600))
        # The page as it shows: its red block, the white gap below it.
        for x, y in ((10, 10), (100, 50), (190, 90)):
            red, green, blue = rows[y][x]
            self.assertTrue(red > 240 and green < 16 and blue < 16, f'({x}, {y}) is {rows[y][x]}')
        for x, y in ((100, 120), (400, 50)):
            self.assertTrue(min(rows[y][x]) > 240, f'({x}, {y}) is {rows[y][x]}')
        low, high = image(captured['low']['value'], 'jpeg'), image(captured['high']['value'], 'jpeg')
        self.assertEqual(jpeg_size(low), (800, 600))
        self.assertEqual(jpeg_size(high), (800, 600))
        self.assertLess(len(low) * 2, len(high), 'a JPEG keeps more at a higher quality')
        # Three at once: two are answered, as JPEG by default, and one is refused.
        burst = captured['burst']
        answered = [entry['value'] for entry in burst if entry['ok']]
        self.assertEqual(len(answered), 2, burst)
        for url in answered:
            self.assertEqual(jpeg_size(image(url, 'jpeg')), (800, 600))
        self.assertEqual([entry['value'] for entry in burst if not entry['ok']],
                         ['This request exceeds the MAX_CAPTURE_VISIBLE_TAB_CALLS_PER_SECOND quota.'])

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
