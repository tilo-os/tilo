#!/usr/bin/env python3
"""Extensions see the Browser's tabs, as they see Chrome's.

Chromium gives extensions no tabs in an offscreen embedding; the host answers
their tab APIs itself (src/extension_tabs.h). With real Chromium:

- A toolbar button without a popup reaches the extension (action.onClicked)
  with the page it was clicked over, which it may then script (activeTab).
- A popup's "current tab" is the page it was opened over, with its address
  for an extension with the tabs permission and without it for one without.
- A worker and its content script talk both ways by the tab's id, by
  messages and over a port the worker opens, and the id
  a message comes from is one the tab APIs know.
- A popup can open a tab, which the Browser is asked to open, and is given
  the tab once it opens.
- A worker hears of a page that finished loading.
- A click reaches a worker that went to sleep, which wakes for it.
- A script a click injects where the extension has no content script of its
  own gets a content script's storage, i18n and messaging, both ways.
- Such a script and the worker open ports to each other, either way, and
  the script hears of the extension's storage changing. Many messages the
  script sends at once on a port all reach the worker, in order.
- Such a script's message and port reach the extension's open popup when
  its worker answers neither.
- A tab is captured as it shows, as PNG or JPEG at the quality asked for, by
  an extension given it by a click (activeTab) or with access to every site,
  and by no other; at most twice a second, as Chrome allows.
- Content scripts on every site are no host permission, as in Chrome: such
  an extension, clicked without activeTab, sees no tab's address or title,
  finds none by its address, and may neither script nor capture the page,
  while its content script runs there.
- An extension's context menu items show in the page's menu where their
  contexts say (page, selection, link, image); choosing one tells the
  extension with the tab, which it may then script (activeTab), and the
  items and their state outlast a restart.
- A privacy blocker (as Privacy Badger, DuckDuckGo Privacy Essentials,
  Ghostery and AdGuard are) counts what it blocked in a tab by the id
  webRequest reports, and its popup finds that count, and the site, from the
  page it was opened over. Turned off for the site from the popup, it reloads
  the page and closes at once, and the page loads its trackers.
- A popup that starts a script on the page and closes at once still starts
  it, as uBlock Origin Lite's element zapper does.
- A click lets an extension read the page's language and zoom, and give its
  button a badge, title, popup and state for that tab alone, which the
  Browser is told of for that tab and the next page does not keep.
- A worker that runs the tab script keeps its address, which extensions read
  and load their helpers against, and its strictness.
- What an extension sets for its toolbar button on one page (a count, a
  title, an icon, off) reaches the Browser for that page's tab alone and
  goes with the page; what it sets for every tab reaches every tab, also one
  opened later; and its popup opens over the page when it asks.
- An extension whose security policy, or its page's own, lets it connect only
  to its own servers (1Password, LastPass, NordPass) still reaches the host;
  before any page was focused its current window is the page's; a failed
  call sets runtime.lastError for its callback; it may set its button's
  badge for any tab, a blank one too.
- A script reaches the frame of a page an extension names (Bitwarden injects
  its form filler into each frame that asks), one of another site too, or
  every frame, and a message reaches one frame's content script.
- A keyboard shortcut an extension's manifest suggests runs its command with
  the page it was pressed on, which it may then script (activeTab), and the
  page does not get the key; its toolbar button's command asks the Browser
  to act as a click on the button does. commands.getAll lists each command
  with its key, none for one the Browser keeps for itself.
- A worker reaches its panel in the page, a page of its own in a frame, by
  the frame and the document a message from the panel came from (Evernote
  Web Clipper answers its panel so), and the page's frames by their
  documents, while the panel answers every message itself.
"""
import base64
import hashlib
import html
import http.server
import itertools
import json
from pathlib import Path
import queue
import re
import secrets
import shutil
import sys
import struct
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
# What the Browser's toolbar evaluates in an extension's worker to show its
# button (extension_toolbar.rs), as Rust spells the string.
READ_ACTION = re.sub(r'\\\n\s*', '', re.search(
    r'const READ_ACTION: &str = "(.*?)";\n', MANAGER.parent.parent.joinpath('src/extension_toolbar.rs').read_text(),
    re.S).group(1))


def prologue_end(text):
    """Where a script's own statements start: after a byte-order mark and its
    directive prologue, as extensions.rs `prologue_end` finds it."""
    end = 3 if text.startswith(b'\xef\xbb\xbf') else 0
    while True:
        at = end
        while True:
            if text[at:at + 1] in (b' ', b'\t', b'\n', b'\r'):
                at += 1
            elif text[at:at + 2] == b'//':
                newline = text.find(b'\n', at)
                at = len(text) if newline < 0 else newline
            elif text[at:at + 2] == b'/*':
                close = text.find(b'*/', at + 2)
                at = len(text) if close < 0 else close + 2
            else:
                break
        quote = text[at:at + 1]
        if quote not in (b'"', b"'"):
            return end
        i = at + 1
        while True:
            c = text[i:i + 1]
            if c in (b'', b'\n', b'\r'):
                return end
            if c == b'\\':
                i += 2
            elif c == quote:
                break
            else:
                i += 1
        i += 1
        while True:
            if text[i:i + 1] in (b' ', b'\t'):
                i += 1
            elif text[i:i + 2] == b'/*':
                close = text.find(b'*/', i + 2)
                if close < 0:
                    return end
                if b'\n' in text[i + 2:close] or b'\r' in text[i + 2:close]:
                    break
                i = close + 2
            else:
                break
        if text[i:i + 1] == b';':
            end = i + 1
        elif text[i:i + 1] in (b'', b'\n', b'\r') or text[i:i + 2] in (b'//', b'/*'):
            end = i
        else:
            return end


BRIDGE = 'https://extension-bridge.tilo.invalid'


def admit_bridge(policy):
    """`policy` letting pages connect to the bridge, as the Browser makes it
    (extensions.rs, policy_with_bridge)."""
    directives = [d.strip() for d in policy.split(';') if d.strip()]
    names = [d.split()[0].lower() for d in directives]
    index = names.index('connect-src') if 'connect-src' in names else None
    governing = index if index is not None else names.index('default-src') if 'default-src' in names else None
    if governing is None:
        return policy
    sources = [s for s in directives[governing].split()[1:] if s.lower() != "'none'"]
    if any(s.rstrip('/') in ('*', 'https:', BRIDGE) for s in sources):
        return policy
    directive = 'connect-src ' + ' '.join(sources + [BRIDGE])
    if index is None:
        directives.append(directive)
    else:
        directives[index] = directive
    return '; '.join(directives)


def admit_bridge_in_pages(root):
    """Each page's own policy (a `<meta>`) letting it reach the bridge, as the
    Browser makes it (extensions.rs, page_with_bridge)."""
    meta = re.compile(r'(<meta\s+http-equiv="content-security-policy"\s+content=")([^"]*)(")', re.IGNORECASE)
    for page in root.rglob('*.html'):
        text = page.read_text()
        admitted = meta.sub(lambda m: m[1] + html.escape(admit_bridge(html.unescape(m[2])), quote=True) + m[3], text)
        page.write_text(admitted)


def prepare(source, into):
    """A copy of the fixture at `source` set up as the Browser sets up an
    extension it installs (extensions.rs, prepare_tabs): its pages may reach
    the engine whatever their policy, and its own worker runs the tab script
    first, with a token the engine is told of."""
    target = Path(shutil.copytree(source, into / source.name)).resolve()
    manifest = json.loads((target / 'manifest.json').read_text())
    policy = manifest.get('content_security_policy', {})
    if isinstance(policy, dict) and 'extension_pages' in policy:
        policy['extension_pages'] = admit_bridge(policy['extension_pages'])
        (target / 'manifest.json').write_text(json.dumps(manifest))
    admit_bridge_in_pages(target)
    worker = manifest.get('background', {}).get('service_worker')
    if not worker:
        return target, None
    token = secrets.token_hex(24)
    (target / 'tilo-extension-tabs.js').write_text(f'globalThis.__tiloBridgeToken = "{token}";\n' + SCRIPT)
    own = target / worker.lstrip('/')
    if manifest['background'].get('type') == 'module':
        runs = b";import '/tilo-extension-tabs.js';"
    else:
        runs = b";typeof importScripts=='function'&&importScripts('/tilo-extension-tabs.js');"
    text = own.read_bytes()
    at = prologue_end(text)
    own.write_bytes(text[:at] + runs + text[at:])
    return target, token


def _token(path):
    """The token `prepare` wrote into the worker of the extension at `path`."""
    script = path / 'tilo-extension-tabs.js'
    if not script.exists():
        return None
    first = script.read_text().split('\n', 1)[0]
    return first.split('"')[1]


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
# A page with a tracker, which says whether the tracker ran.
SHIELDED = (b'<!doctype html><title>loading</title><script src="/tracker/t.js"></script>'
            b"<script>addEventListener('load', () => {"
            b"document.title = 'page:' + (window.tracked ? 'tracked' : 'untracked'); });</script>")
# What the privacy blocker fixture may do, as the Browser grants it.
SHIELD = (['declarativeNetRequest', 'webRequest', 'tabs', 'scripting'], ['<all_urls>'])


# A page with a link, an image and words, each where the test right-clicks
# it, in CSS pixels; `?select` selects the words.
MENU_PAGE = """<!doctype html><title>menus</title><body style="margin:0">
<a href="/target" style="position:absolute;left:10px;top:10px;width:200px;height:30px;display:block">a link</a>
<img src="/image.svg" style="position:absolute;left:10px;top:60px;width:80px;height:80px">
<p id="words" style="position:absolute;left:10px;top:160px;margin:0;font-size:24px">selected words</p>
<script>if (location.search === '?select') getSelection().selectAllChildren(document.getElementById('words'));</script>
</body>"""
# A page that shows each key it gets in its title.
KEYS_PAGE = (b'<!doctype html><title>keys</title><body>keys'
             b"<script>addEventListener('keydown', (e) => { document.title = 'key:' + e.key; });</script>")
IMAGE = b'<svg xmlns="http://www.w3.org/2000/svg" width="80" height="80"><rect width="80" height="80" fill="teal"/></svg>'


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

    def handle(self):
        # A page left before its response was read closes the connection.
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        kind = 'text/html'
        if self.path == '/shot':
            body = SHOT.encode()
        elif self.path.startswith('/menus'):
            body = MENU_PAGE.encode()
        elif self.path == '/image.svg':
            body, kind = IMAGE, 'image/svg+xml'
        elif self.path == '/keys':
            body = KEYS_PAGE
        else:
            # A page under /de says it is in Austrian German.
            lang = ' lang="de-AT"' if self.path.startswith('/de') else ''
            # A page with login forms in frames of their own, as some sites
            # have: one of the same site, one of another, which another
            # process renders.
            port = self.server.server_port
            frame = f'<iframe src="/inner"></iframe><iframe src="http://localhost:{port}/outer"></iframe>' if self.path == '/framed' else ''
            body = f'<!doctype html><html{lang}><title>page {self.path}</title><body>page{frame}</body></html>'.encode()
        if self.path.startswith('/tracker/'):
            kind, body = 'text/javascript', b'window.tracked = true;'
        elif self.path.startswith('/shielded'):
            body = SHIELDED
        self.send_response(200)
        self.send_header('Content-Type', kind)
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

    def start(self, *names, access, installed=None, profile=None, files=None, focus=True):
        """A host with the fixtures `names` loaded, each granted what the
        Browser grants an installed extension (`access`: name -> (permissions,
        hosts[, scriptable])), with `files` (name -> {file: bytes}) written into its copy. A
        host started again with the `installed` directory and the `profile` of
        an earlier one finds the same extensions there. With `focus`, the
        person has focused the first page."""
        if installed is None:
            directory = tempfile.TemporaryDirectory(prefix='tilo-extension-tabs-')
            self.addCleanup(directory.cleanup)
            installed = Path(directory.name)
        # One prepared before keeps its token in the worker it wrote.
        prepared = [((installed / name).resolve(), _token(installed / name)) if (installed / name).exists()
                    else prepare(FIXTURES / name, installed) for name in names]
        for name, (path, _) in zip(names, prepared):
            for file, data in (files or {}).get(name, {}).items():
                (path / file).write_bytes(data)
        paths = [path for path, _ in prepared]
        host = Host(args=['--use-mock-keychain', '--load-extension=' + ','.join(map(str, [MANAGER, *paths]))], profile=profile)
        self.addCleanup(host.close)
        host.send(op='extension_identity', id=extension_id(MANAGER))
        # The Browser tells the engine what each extension may do once it has
        # read its profile: after the extensions' workers started.
        host.settle(1.0)
        ids = {}
        for name, (path, token) in zip(names, prepared):
            ids[name] = extension_id(path)
            permissions, hosts, *scriptable = access.get(name, ([], []))
            command = {'id': ids[name], 'permissions': permissions, 'hosts': hosts,
                       'scriptable': scriptable[0] if scriptable else []}
            if token:
                command['token'] = token
            # Its keyboard shortcuts, as its manifest declares them
            # (extension_toolbar.rs, access).
            commands = json.loads((path / 'manifest.json').read_text()).get('commands')
            if isinstance(commands, dict):
                command['commands'] = commands
            host.send(op='extension_access', **command)
        host.send(op='resize', width=800, height=600, scale_factor=1.0)
        if focus:
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

    def toolbar_reads(self, host, extension):
        """What the Browser's toolbar reads of the extension's button, from
        its running worker."""
        read = self.in_worker(host, extension, READ_ACTION)
        self.assertIsNotNone(read, 'the extension has no worker running')
        return json.loads(read)

    def in_worker(self, host, extension, expression):
        """`expression`'s value in the extension's running worker, read as the
        toolbar reads it."""
        self.calls = getattr(self, 'calls', 0)

        def call(method, params, session=None):
            self.calls += 1
            request = self.calls
            command = {'op': 'devtools_call', 'request_id': request, 'method': method, 'params': params}
            if session:
                command['session'] = session
            host.send(**command)
            reply = host.until(lambda e: e['event'] == 'devtools_result' and e['request_id'] == request)
            self.assertTrue(reply['ok'], reply)
            return json.loads(reply['body'])

        prefix = f'chrome-extension://{extension}/'
        target = next((t['targetId'] for t in call('Target.getTargets', {})['targetInfos']
                       if t['type'] == 'service_worker' and t['url'].startswith(prefix)), None)
        if target is None:
            return None
        session = call('Target.attachToTarget', {'targetId': target, 'flatten': True})['sessionId']
        try:
            read = call('Runtime.evaluate', {'expression': expression, 'awaitPromise': True, 'returnByValue': True}, session)
        finally:
            call('Target.detachFromTarget', {'sessionId': session})
        self.assertNotIn('exceptionDetails', read)
        return read['result']['value']

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

    def test_a_keyboard_shortcut_runs_the_extensions_command_on_the_page(self):
        host, ids = self.start('commander', access={'commander': (['activeTab', 'scripting'], [])})
        page = self.url + '/keys'
        host.navigate(page)
        host.settle(1.0)
        mac = sys.platform == 'darwin'

        def press(key, ctrl=False, alt=False, shift=False):
            # On macOS a manifest's "Ctrl" is Command, which arrives as meta.
            modifiers = {'ctrl': ctrl and not mac, 'meta': ctrl and mac, 'alt': alt, 'shift': shift}
            host.send(op='key', key=key, down=True, **modifiers)
            host.send(op='key', key=key, down=False, **modifiers)

        press('K', alt=True, shift=True)
        seen = json.loads(self.title(host, 1, 'command:')[len('command:'):])
        self.assertEqual({key: seen[key] for key in ('command', 'url')}, {'command': 'mark', 'url': True})
        self.assertEqual(seen['all'], [
            {'name': '_execute_action', 'description': '', 'shortcut': '\u21e7\u2318Y' if mac else 'Ctrl+Shift+Y'},
            # The Browser keeps it for Find.
            {'name': 'find', 'description': 'Find on the page', 'shortcut': ''},
            {'name': 'mark', 'description': 'Mark the page', 'shortcut': '\u2325\u21e7K' if mac else 'Alt+Shift+K'},
            {'name': 'quiet', 'description': 'No key of its own', 'shortcut': ''},
        ])
        # The page got neither the command's key nor its release; the next
        # key is its own.
        press('a')
        self.title(host, 1, 'key:a')
        self.assertEqual([e['title'] for e in host.seen if e['event'] == 'title' and e['title'].startswith('key:')],
                         ['key:a'])

        # The button's own command: the Browser is asked to act as a click
        # on the button would, over this page.
        press('Y', ctrl=True, shift=True)
        asked = host.until(lambda e: e['event'] == 'extension_execute_action')
        self.assertEqual((asked.get('tab', 1), asked['id']), (1, ids['commander']))
        # A key no command has reaches the page, with the same modifiers.
        press('U', ctrl=True, shift=True)
        self.title(host, 1, 'key:U')

    def test_a_worker_keeps_its_own_address(self):
        host, ids = self.start('located', access={'located': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/located')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['located'])
        found = json.loads(self.title(host, 1, 'located:')[len('located:'):])
        self.assertEqual(found, {'background': True, 'path': '/background/worker.js',
                                 'manifest': 'background/worker.js', 'helper': True, 'strict': True})

    def test_a_click_lets_an_extension_read_the_page_and_mark_its_button_for_it(self):
        host, ids = self.start('badger', access={'badger': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/de/clip')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['badger'])
        report = json.loads(self.title(host, 1, 'marked:')[len('marked:'):])
        popup = f'chrome-extension://{ids["badger"]}/clipped.html'
        self.assertEqual(report, {
            'clicks': 1, 'language': 'de', 'zoom': 1, 'missing': 'No tab with id: 987654.',
            'badge': '7', 'global': '', 'title': 'Clipped', 'popup': popup, 'enabled': False, 'callback': '7'})
        # The Browser is told of the page's own, for its tab; its toolbar
        # reads the extension's own for every tab.
        told = {key: value for e in host.seen if e['event'] == 'extension_action' and e.get('tab') == 1
                and e['id'] == ids['badger'] and not e['global'] for key, value in e['change'].items()}
        self.assertEqual(told, {'badge': '7', 'title': 'Clipped', 'popup': popup, 'enabled': False})
        self.assertEqual(self.toolbar_reads(host, ids['badger']),
                         {'text': '', 'title': 'Clip this page', 'popup': '', 'enabled': True})
        # A new page has the button as the extension set it for every tab.
        mark = len(host.seen)
        host.navigate(self.url + '/next')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['badger'])
        report = json.loads(self.title(host, 1, 'marked:{"clicks":2')[len('marked:'):])
        self.assertEqual(report, {'clicks': 2, 'badge': '', 'global': '', 'title': 'Clip this page',
                                  'popup': '', 'enabled': True, 'callback': ''})
        self.assertIn('extension_action_reset', [e['event'] for e in host.seen[mark:] if e.get('tab') == 1])

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
        # Its content script's site is no host permission (the Browser sends
        # it apart), and the two talk all the same.
        host, ids = self.start('messenger', access={'messenger': ([], [], ['http://127.0.0.1/*'])})
        host.navigate(self.url + '/talk')
        answer = json.loads(self.title(host, 1, 'messenger:')[len('messenger:'):])
        self.assertEqual(answer, {'known': True, 'pong': 'pong:/talk:true', 'ported': 'ping:declared:true'})


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
            'capturer': (['activeTab', 'scripting'], []),
            'sweeper': (['scripting'], ['<all_urls>']),
            'peeker': (['tabs', 'scripting'], ['http://127.0.0.1/*']),
        })
        # Another page, in a tab of its own, which no click gives anyone.
        host.send(op='create_tab', tab=2)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 2)
        host.send(op='resize', tab=2, width=800, height=600, scale_factor=1.0)
        host.send(op='navigate', tab=2, url=self.url + '/other')
        host.navigate(self.url + '/shot')
        self.title(host, 2, 'page /other')
        host.settle(1.0)
        refused = "Either the '<all_urls>' or 'activeTab' permission is required."
        # Access to one site, every tab's address and a click are not enough
        # without activeTab.
        host.send(op='extension_click', id=ids['peeker'])
        self.assertEqual(self.report('peeker'), {'ok': False, 'value': refused})
        # Access to every site is.
        host.send(op='extension_click', id=ids['sweeper'])
        swept = self.report('sweeper')
        self.assertTrue(swept['ok'], swept['value'])
        width, height, rows = png_pixels(image(swept['value'], 'png'))
        self.assertEqual((width, height), (800, 600))
        host.send(op='extension_click', id=ids['capturer'])
        captured = self.report('capturer')
        # activeTab gives the page clicked over, and no other.
        self.assertEqual(captured['other'], {'ok': False, 'value': refused})
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

    def test_content_scripts_on_every_site_give_no_access_to_the_tabs(self):
        # As the Browser reads its manifest: content scripts on <all_urls>,
        # `scripting`, and neither a host permission nor activeTab.
        host, ids = self.start('reacher', access={'reacher': (['scripting'], [], ['<all_urls>'])})
        host.navigate(self.url + '/reached')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['reacher'])
        self.assertEqual(self.report('reacher'), {
            # Its content script runs in the page all the same.
            'content': '/reached', 'found': True,
            # The tab it was clicked over, and every tab, without an address
            # or a title, and none found by its address.
            'clicked': [], 'shown': [], 'by_url': 0,
            'script': {'ok': False, 'value': 'Cannot access contents of the page. Extension manifest '
                                             'must request permission to access the respective host.'},
            'capture': {'ok': False, 'value': "Either the '<all_urls>' or 'activeTab' permission is required."},
        })

    def test_a_script_injected_on_a_click_and_the_worker_talk_over_ports(self):
        host, ids = self.start('porter', access={'porter': (['activeTab', 'scripting', 'storage'], [])})
        host.navigate(self.url + '/ported')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['porter'])
        seen = json.loads(self.title(host, 1, 'ported:')[len('ported:'):])
        self.assertEqual(seen, {
            'named': 'from-worker', 'echo': 'hello', 'pong': 'ping:true', 'unanswered': True,
            'changed': 'green>blue', 'area': True, 'closed': True,
        })

    def test_a_burst_on_a_port_from_an_injected_script_reaches_the_worker_whole(self):
        # More messages at once than the host lets wait for a worker one by
        # one: each arrives, in order, and the port stays open.
        host, ids = self.start('burster', access={'burster': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/burst')
        host.settle(1.0)
        host.send(op='extension_click', id=ids['burster'])
        self.assertEqual(self.title(host, 1, 'burst:'), 'burst:{"count":300,"ordered":true}')

    def test_a_script_a_popup_injects_reaches_the_popup_when_the_worker_does_not_answer(self):
        host, ids = self.start('pager', access={'pager': (['activeTab', 'scripting'], [])})
        host.navigate(self.url + '/paged')
        host.settle(1.0)
        # The Browser opens the popup over the page, which the popup may then
        # script, as after a click.
        host.send(op='create_tab', tab=2)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 2)
        host.send(op='extension_panel', tab=2, action=ids['pager'])
        host.send(op='resize', tab=2, width=360, height=200, scale_factor=1.0)
        host.send(op='navigate', tab=2, url=f'chrome-extension://{ids["pager"]}/popup.html')
        self.assertEqual(self.title(host, 1, ('paged:', 'failed:')), 'paged:page:true|port:hi')
    def shielded(self, host, page, expected):
        """The tracker page in tab 1 as it loads once the blocker's rules
        are in, which the engine applies a moment after it loads it."""
        deadline = time.monotonic() + HANG_BOUND
        for attempt in itertools.count():
            host.send(op='navigate', url=f'{page}?{attempt}')
            title = host.until(lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('page:'))['title']
            if title == expected or time.monotonic() > deadline:
                return title
            host.settle(0.5)

    def test_a_privacy_blocker_counts_what_it_blocked_in_the_tab_and_turns_off_for_the_site(self):
        host, ids = self.start('shield', access={'shield': SHIELD})
        # The engine blocks with the extension's rules before it first runs
        # its worker, and requests before the worker listens go uncounted,
        # as in Chrome: the page loads once it listens.
        listens = "typeof chrome.webRequest == 'object' && chrome.webRequest.onErrorOccurred.hasListeners()"
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['shield'], listens) is not True:
            self.assertLess(time.monotonic(), deadline, 'the worker never listened')
            host.settle(0.25)
        page = self.url + '/shielded'
        self.assertEqual(self.shielded(host, page, 'page:untracked'), 'page:untracked')
        self.panel(host, 2, f'chrome-extension://{ids["shield"]}/popup.html')
        shown = json.loads(self.title(host, 2, 'shield:')[len('shield:'):])
        self.assertEqual(shown, {'site': self.url, 'blocked': 1})
        # Off for the site: the popup reloads the page and closes at once.
        mark = len(host.seen)
        self.panel(host, 3, f'chrome-extension://{ids["shield"]}/popup.html?allow')
        closed = host.until(lambda e: e.get('tab') == 3 and (e['event'] == 'closed' or (e['event'] == 'title' and e['title'].startswith('failed:'))))
        self.assertEqual(closed['event'], 'closed', closed)
        reloaded = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('page:')
        title = next((e for e in host.seen[mark:] if reloaded(e)), None) or host.until(reloaded)
        self.assertEqual(title['title'], 'page:tracked', 'the page reloaded without the blocker')

    def test_a_popup_that_starts_a_script_and_closes_at_once_still_starts_it(self):
        host, ids = self.start('shield', access={'shield': SHIELD})
        host.navigate(self.url + '/zapped')
        self.panel(host, 2, f'chrome-extension://{ids["shield"]}/popup.html?mark')
        self.assertEqual(self.title(host, 1, 'marked'), 'marked')

    def test_a_worker_hears_of_a_page_that_finished_loading(self):
        host, ids = self.start('watcher', access={'watcher': (['scripting'], ['http://127.0.0.1/*'])})
        # The worker starts with the engine, in its own time, and hears of
        # pages once it listens.
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['watcher'], 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        host.settle(1.0)
        host.navigate(self.url + '/watched')
        watched = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('watched:')
        try:
            title = (next((e for e in host.seen if watched(e)), None) or host.until(watched, timeout=20))['title']
        except (TimeoutError, queue.Empty):
            # Read while the worker is still awake.
            self.fail(f"the worker heard {self.in_worker(host, ids['watcher'], 'JSON.stringify(heard)')}")
        self.assertRegex(title, r'^watched:\d+$')

    def test_an_extension_that_may_connect_only_to_its_own_servers_finds_its_tabs(self):
        # Before the person focused any page, as when the Browser starts: the
        # last focused window is still the page's, as in Chrome.
        host, ids = self.start('vault', access={'vault': (['tabs'], [])}, focus=False)
        page = self.url + '/vault'
        host.navigate(page)
        # A tab still blank, as a new one is.
        host.send(op='create_tab', tab=3)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 3)
        self.panel(host, 2, f'chrome-extension://{ids["vault"]}/popup.html')
        vault = json.loads(self.title(host, 2, 'vault:')[len('vault:'):])
        self.assertEqual(vault, {
            'url': page,
            'worker': {
                'url': page,
                'missing': {'value': True, 'error': 'No window with id: 2000000000.'},
                'badge': {'blank': 'set', 'page': 'set', 'shown': '3', 'missing': 'No tab with id: 2000000000.'},
            },
            'missing': {'value': True, 'error': 'No tab with id: 2000000000.'},
            'cleared': True,
        })

    def test_a_script_reaches_the_frame_a_message_came_from(self):
        host, ids = self.start('framer', access={'framer': (['scripting'], ['http://127.0.0.1/*', 'http://localhost/*'])})
        host.navigate(self.url + '/framed')
        title = self.title(host, 1, 'framed')
        self.assertTrue(title.startswith('framed:'), title)
        framed = json.loads(title[len('framed:'):])
        # After the frame of the same site went to a page of the other site
        # the extension's content script does not match, a script into every
        # frame still runs in the page and in the frame of the other site.
        after = framed.pop('after')
        self.assertIsInstance(after, list, after)
        self.assertIn(['/framed', '/framed'], after)
        self.assertIn(['/outer', '/outer'], after)
        self.assertEqual(framed, {
            'frames': {path: {'frame': True, 'injected': path, 'id': True} for path in ('/framed', '/inner', '/outer')},
            'all': [['/framed', '/framed'], ['/inner', '/inner'], ['/outer', '/outer']],
            'where': {'/inner': '/inner', '/outer': '/outer'},
            'large': '/outer:1500000',
            'missing': True,
            'moved': True,
        })

    def test_a_worker_reaches_its_panel_in_the_page_and_frames_by_their_documents(self):
        host, ids = self.start('clipper', access={'clipper': (['scripting'], ['http://127.0.0.1/*', 'http://localhost/*'])})
        host.navigate(self.url + '/framed')
        report = self.report('clipper')
        tab = report.pop('tab')
        panel = {'panel': '/panel.html', 'from': ids['clipper']}
        self.assertEqual(report, {
            # The panel, a page of the extension in a frame of the page, by
            # the frame and the document its message came from, as Evernote
            # Web Clipper answers its panel; again by the frame alone.
            'byFrame': panel,
            'byDocument': panel,
            'again': panel,
            # A frame and a document that are not one frame's name none.
            'mismatched': 'error: Could not establish connection. Receiving end does not exist.',
            # A content script of another site's frame, by its document.
            'outer': '/outer',
            'documents': [['/inner', '/inner', True], ['/outer', '/outer', True]],
            # Every frame but the panel, which takes no script.
            'all': [['/framed', '/framed'], ['/inner', '/inner'], ['/outer', '/outer']],
            'intoPanel': 'error: The page could not run the script',
            'both': "error: Cannot specify both 'allFrames' and 'frameIds'.",
            'gone': 'error: No document with id 0123456789ABCDEF0123456789ABCDEF in tab with id %d.' % tab,
            'ids': True,
        })

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

    # chrome.contextMenus: items in the page's menu, and a choice reaching
    # the extension.
    def menu(self, host, x, y):
        """Right-click at (x, y) and read the menu that opens, then close it:
        its items by label."""
        host.send(op='mouse_move', x=x, y=y)
        host.send(op='mouse_button', button='right', down=True, x=x, y=y)
        host.send(op='mouse_button', button='right', down=False, x=x, y=y)
        menu = host.until(lambda e: e['event'] == 'context_menu')
        return menu, {item['label']: item['id'] for item in menu['items']}

    def close_menu(self, host, menu):
        host.send(op='context_action', id=menu['id'], command=0)
        host.until(lambda e: e['event'] == 'context_closed' and e['id'] == menu['id'])

    def page_menu(self, host, x, y, wanted):
        """The menu at (x, y) once the extension's item `wanted` is in it: the
        worker makes its items as it is installed, a moment after the start."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            menu, items = self.menu(host, x, y)
            if wanted in items or time.monotonic() > deadline:
                return menu, items
            self.close_menu(host, menu)
            host.settle(0.5)

    def choose(self, host, menu, items, label):
        """Choose the item `label`: what the extension marked the page with."""
        before = len(host.seen)
        host.send(op='context_action', id=menu['id'], command=items[label])
        match = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('chosen:')
        seen = next((e for e in host.seen[before:] if match(e)), None) or host.until(match)
        return json.loads(seen['title'][len('chosen:'):])

    def test_items_show_where_their_contexts_say_and_a_choice_reaches_the_extension(self):
        installed = Path(tempfile.mkdtemp(prefix='tilo-extension-menus-'))
        self.addCleanup(shutil.rmtree, installed, True)
        profile = Path(tempfile.mkdtemp(prefix='tilo-extension-menus-profile-'))
        self.addCleanup(shutil.rmtree, profile, True)
        access = {'menus': (['contextMenus', 'activeTab', 'scripting'], [])}
        host, ids = self.start('menus', access=access, installed=installed, profile=profile)
        page = self.url + '/menus'
        host.navigate(page)
        ours = {'Mark the page', 'Look up "selected words"', 'Mark the link', 'Mark the image', 'Remember the page', 'Everywhere'}

        # The page itself: its items, and those for every context.
        menu, items = self.page_menu(host, 400, 400, 'Mark the page')
        self.assertEqual(ours & set(items), {'Mark the page', 'Remember the page', 'Everywhere'})
        self.assertFalse({'Elsewhere', 'Gone', 'Before'} & set(items), items)
        self.close_menu(host, menu)

        # A link: its item, not the page's.
        menu, items = self.menu(host, 20, 20)
        self.assertEqual(ours & set(items), {'Mark the link', 'Everywhere'})
        chosen = self.choose(host, menu, items, 'Mark the link')
        # The tab's address shows the click gave the extension the page.
        self.assertEqual(chosen['url'], page)
        if sys.platform == 'darwin':
            # macOS selects the words under a right-click.
            chosen['info'].pop('selectionText', None)
        self.assertEqual(chosen['info'], {'menuItemId': 'link', 'linkUrl': self.url + '/target', 'pageUrl': page, 'frameId': 0, 'editable': False})

        # An image.
        menu, items = self.menu(host, 40, 100)
        self.assertEqual(ours & set(items), {'Mark the image', 'Everywhere'})
        chosen = self.choose(host, menu, items, 'Mark the image')
        self.assertEqual(chosen['info']['mediaType'], 'image')
        self.assertEqual(chosen['info']['srcUrl'], self.url + '/image.svg')

        # A selection, named in the item's title.
        host.navigate(page + '?select')
        menu, items = self.menu(host, 40, 172)
        self.assertEqual(ours & set(items), {'Look up "selected words"', 'Everywhere'})
        chosen = self.choose(host, menu, items, 'Look up "selected words"')
        self.assertEqual((chosen['info']['menuItemId'], chosen['info']['selectionText']), ('selection', 'selected words'))

        # A checkbox turns on.
        menu, items = self.menu(host, 400, 400)
        chosen = self.choose(host, menu, items, 'Remember the page')
        self.assertEqual((chosen['info']['wasChecked'], chosen['info']['checked']), (False, True))

        # The items and their state outlast the engine: an extension makes
        # them once, when installed.
        host.close()
        host, _ = self.start('menus', access=access, installed=installed, profile=profile)
        host.navigate(page)
        menu, items = self.page_menu(host, 400, 400, 'Remember the page')
        chosen = self.choose(host, menu, items, 'Remember the page')
        self.assertEqual((chosen['info']['wasChecked'], chosen['info']['checked']), (True, False))

    def test_a_button_shows_what_its_extension_set_for_each_page(self):
        host, ids = self.start('counter', access={'counter': (['scripting'], ['http://127.0.0.1/*'])},
                               files={'counter': {'on.png': png(16, 16)}})
        counter = ids['counter']

        def seek(since, predicate):
            """The first event since `since` (an index of host.seen) that
            matches, and its index."""
            for index in range(since, len(host.seen)):
                if predicate(host.seen[index]):
                    return host.seen[index], index
            event = host.until(predicate)
            return event, len(host.seen) - 1

        def action(tab, every, key, value):
            return lambda e: (e['event'] == 'extension_action' and e.get('tab') == tab and e['id'] == counter
                              and e['global'] == every and e['change'].get(key) == value)

        mark = len(host.seen)
        host.navigate(self.url + '/count')
        # For this page: its count, a title and an icon among its files.
        seek(mark, action(1, False, 'badge', '6'))
        seek(mark, action(1, False, 'title', '6 blocked'))
        seek(mark, action(1, False, 'icon', {'path': '/on.png'}))
        # For every page: a badge, and pixels it drew.
        seek(mark, action(1, True, 'badge', 'all'))
        seek(mark, action(1, True, 'icon', {'width': 2, 'height': 1, 'data': 'AQIDBAUGBwg='}))
        read = json.loads(self.title(host, 1, 'badged:')[len('badged:'):])
        self.assertEqual(read, {'text': '6', 'every': 'all', 'title': '6 blocked', 'enabled': True})

        # The next page starts with what every page shows, then its own.
        mark = len(host.seen)
        host.navigate(self.url + '/off')
        _, reset = seek(mark, lambda e: e['event'] == 'extension_action_reset' and e.get('tab') == 1)
        _, own = seek(mark, action(1, False, 'badge', '4'))
        self.assertLess(reset, own, 'what the last page showed goes first')
        seek(mark, action(1, False, 'enabled', False))
        read = json.loads(self.title(host, 1, 'badged:{"text":"4"')[len('badged:'):])
        self.assertEqual((read['text'], read['enabled']), ('4', False))

        # A tab opened later is told what every tab shows; its page's count
        # is its own.
        mark = len(host.seen)
        host.send(op='create_tab', tab=2)
        replayed, _ = seek(mark, lambda e: e['event'] == 'extension_action' and e.get('tab') == 2)
        self.assertEqual((replayed['global'], replayed['change']['badge']), (True, 'all'))
        self.assertEqual(replayed['change']['icon']['width'], 2)
        host.send(op='resize', tab=2, width=800, height=600, scale_factor=1.0)
        host.send(op='navigate', tab=2, url=self.url + '/second')
        seek(mark, action(2, False, 'badge', '7'))
        host.settle(1.0)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'extension_action' and e.get('tab') == 1
                          and not e['global']], 'another tab is not told of this page')

        # It asks to open its popup over the page in front.
        mark = len(host.seen)
        host.navigate(self.url + '/open')
        opened, _ = seek(mark, lambda e: e['event'] == 'extension_open_popup')
        self.assertEqual((opened.get('tab'), opened['id'], opened['url']),
                         (1, counter, f'chrome-extension://{counter}/popup.html'))


def png(width, height):
    """An opaque PNG of `width` by `height` pixels."""
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\0' + b'\x20\x80\x40\xff' * width for _ in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


if __name__ == '__main__':
    unittest.main(verbosity=2)
