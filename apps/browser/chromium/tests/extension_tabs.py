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
- A tab keeps the id an extension was told of it by on a blank page or the
  extension's own page, once it shows a web page: the one Chromium gives
  that page's messages. Two pages of an extension that trade what the
  Browser has them say to tell their tabs apart are not taken for each
  other's tab once they show web pages.
- A worker hears of a page that finished loading, also one that finished
  before the Browser told the engine whose worker it is, as when the Browser
  starts, and then by the id every other call names its tab by, also when
  the Browser had not said which extension is its own yet.
- An extension that follows navigations (webNavigation) hears of each
  frame's by the ids the tab APIs give and finds the page's frames; a page's
  link that opens a tab, in its main frame or in a frame, tells it which
  page and frame opened which tab, which knows its opener. A change of zoom
  and a tab made active (highlighted, as the only one of its window) are
  told as well, and a tab is never said to move or to be replaced.
- Tabs a page opens at once, which may show their pages in any order, are
  each told of with the link that opened it.
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
- What the person types after an extension's keyword in the address
  reaches its worker (onInputStarted, onInputChanged, onInputCancelled), its
  suggestions for the latest change and its default suggestion come back for
  the page's tab, and Enter reaches onInputEntered, after which the
  extension reads the page (activeTab) and sends it where it says.
- A worker reaches its panel in the page, a page of its own in a frame, by
  the frame and the document a message from the panel came from (Evernote
  Web Clipper answers its panel so), and the page's frames by their
  documents, while the panel answers every message itself.
- An extension's notifications reach the Browser to show, a progress and
  two buttons with them, and change and go as the extension says; getAll
  lists those shown. The person choosing one, or one of its buttons, reaches
  the worker, which hears it closed.
- An ad blocker whose button shows how many requests its rules blocked or
  redirected on the page (uBlock Origin Lite) shows that count for that tab
  alone, changed as it says, under text of its own set for the page, and the
  next page counts anew. With two blockers each counts what its own rules
  did, and one that may read what they matched finds its rules for a tab.
- An extension asks for the access its manifest offers for later
  (`optional_permissions`, `optional_host_permissions`) when the person
  clicks in its popup: the Browser is asked, and what the person allows the
  extension holds at once, reaches the page with, and hears of in its
  worker; what it gives up it loses. What it was granted in an earlier run
  it holds from the start. What the person revokes in the Library it loses
  at once and hears of; a question whose window closed unanswered lets it
  ask again; signing in (`identity`) granted later lets it sign in.
- An extension's download is the Browser's: the person is asked, under the
  name the extension chose; the extension learns its id, finds it, pauses,
  resumes and cancels it and hears of each change; a refusal reaches it as
  Chrome's; its own data, and a file its page made, download too; it opens
  a download only on the person's click, and shows one in its folder,
  through the Browser.
- An extension signs in (identity.launchWebAuthFlow): the Browser is asked
  to show the sign-in page over the page in front, and the page going to the
  extension's own address, by a redirect or a script, ends the call with
  that address and closes the panel; the page never loads there. Closing the
  panel turns the sign-in down, also after the extension waited longer than
  one request is held. A sign-in that may not ask the person loads out of
  sight: a site that sends them straight back signs them in, one whose page
  loads and waits for them says it needs them. getAuthToken says no one
  signed in to the Browser.
- An offscreen document an extension makes, finds and closes
  (chrome.offscreen) parses for its worker, which answers it for the tabs it
  has no API for, and a script the extension injected into the page reaches
  it, as in Chrome.
- An extension's side panel opens beside the page on a click on its button
  when it asked for that (openPanelOnActionClick), and the extension hears of
  no click; from its keyboard shortcut's listener (sidePanel.open), never
  outside a gesture; and from the Browser. It shows the page its options name
  for the tab, follows them where it shows, closes when asked or turned off
  for the tab, and its page is no tab and finds the page beside it as its
  window.
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
import urllib.parse
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


def prepare_manager(into):
    """A copy of the Browser's own extension at `into`, with the token its
    worker reports to the engine with, as the Browser writes it
    (extensions.rs, write_manager)."""
    target = into / 'extension-manager'
    if not target.exists():
        shutil.copytree(MANAGER, target)
        (target / 'tilo-bridge-token.js').write_text(f'globalThis.__tiloBridgeToken = "{secrets.token_hex(24)}";\n')
    target = target.resolve()
    return target, (target / 'tilo-bridge-token.js').read_text().split('"')[1]


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
# A page with two trackers, two scripts an ad blocker puts its own in place
# of, and a beacon, which says what loaded. The stand-in at an address of
# its own for the session counts, though it does not run here (README).
COUNTED = (b'<!doctype html><title>loading</title><script src="/tracker/a.js"></script>'
           b'<script src="/surrogate/s.js"></script><script src="/standin/s.js"></script><img src="/tracker/b.png">'
           b"<script>addEventListener('load', async () => { let beacon = 'sent';"
           b"try { await fetch('/beacon/c'); } catch (error) { beacon = 'stopped'; }"
           b"document.title = 'counted:' + JSON.stringify({tracked: !!window.tracked,"
           b"standin: window.standin || 'none', beacon}); });</script>")
# The counted page as it loads with the ad blocker fixture.
TALLIED = {'tracked': False, 'standin': 'extension', 'beacon': 'sent'}
# What the ad blocker fixtures may do, as the Browser grants it.
TALLY = (['declarativeNetRequest', 'activeTab', 'scripting'], ['<all_urls>'])
SPOTTER = (['declarativeNetRequest', 'declarativeNetRequestFeedback'], ['<all_urls>'])
# What Chromium answers for the badge of an extension that shows each page's
# count, and may not read it.
COUNT_PLACEHOLDER = '<<declarativeNetRequestActionCount>>'
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
# A page with a link that opens a new tab, across its top, and a frame
# below with one of its own, each where the test clicks it, in CSS pixels.
OPENER_PAGE = (b'<!doctype html><title>opener</title><body style="margin:0">'
               b'<a href="/opened" target="_blank" style="display:block;width:400px;height:100px">open</a>'
               b'<iframe src="/opener-frame" style="position:absolute;left:0;top:150px;width:400px;height:200px;border:0"></iframe>')
OPENER_FRAME = (b'<!doctype html><title>frame</title><body style="margin:0">'
                b'<a href="/from-frame" target="_blank" style="display:block;width:400px;height:200px">open</a>')
# A page with two links that open new tabs, one above the other, the
# second's address extending the first's.
OPENERS_PAGE = (b'<!doctype html><title>openers</title><body style="margin:0">'
                b'<a href="/opened" target="_blank" style="display:block;width:400px;height:100px">open</a>'
                b'<a href="/opened/more" target="_blank" style="display:block;width:400px;height:100px">more</a>')
IMAGE = b'<svg xmlns="http://www.w3.org/2000/svg" width="80" height="80"><rect width="80" height="80" fill="teal"/></svg>'
# A file served under /download/, slowly enough to pause it on its way.
DOWNLOAD = bytes(range(256)) * (4 * 4096)


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
        if self.path.startswith('/download/'):
            self.send_response(200)
            self.send_header('Content-Type', 'application/octet-stream')
            self.send_header('Content-Disposition', 'attachment; filename=served.bin')
            self.send_header('Content-Length', str(len(DOWNLOAD)))
            self.end_headers()
            for at in range(0, len(DOWNLOAD), 65536):
                self.wfile.write(DOWNLOAD[at:at + 65536])
                self.wfile.flush()
                time.sleep(0.05)
            return
        kind = 'text/html'
        if self.path.startswith('/authorize'):
            self.authorize()
            return
        if self.path == '/shot':
            body = SHOT.encode()
        elif self.path.startswith('/menus'):
            body = MENU_PAGE.encode()
        elif self.path == '/image.svg':
            body, kind = IMAGE, 'image/svg+xml'
        elif self.path == '/keys':
            body = KEYS_PAGE
        elif self.path == '/opener':
            body = OPENER_PAGE
        elif self.path == '/opener-frame':
            body = OPENER_FRAME
        elif self.path == '/openers':
            body = OPENERS_PAGE
        elif self.path.startswith('/download-framed/'):
            # A page that shows the extension named last in its address in
            # a frame at its top left, with room of its own beside it.
            extension = self.path.rsplit('/', 1)[-1]
            body = (f'<!doctype html><title>framed</title><body style="margin:0;height:100vh">'
                    f'<iframe src="chrome-extension://{extension}/frame.html" '
                    f'style="position:absolute;left:0;top:0;width:300px;height:150px;border:0"></iframe>').encode()
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
        elif self.path.startswith('/surrogate/'):
            kind, body = 'text/javascript', b"window.surrogate = 'server';"
        elif self.path.startswith('/standin/'):
            kind, body = 'text/javascript', b"window.standin = 'server';"
        elif self.path.startswith('/beacon/'):
            kind, body = 'text/plain', b'ok'
        elif self.path.startswith('/shielded'):
            body = SHIELDED
        elif self.path.startswith('/counted'):
            body = COUNTED
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authorize(self):
        """A sign-in page: with `auto` it sends the person straight back to
        `redirect_uri` with a code, as a site does where they are signed in
        already; with `script` its script does once it loads; else it waits
        for them."""
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        back = query['redirect_uri'][0] + '?code=' + query.get('code', ['none'])[0]
        if 'auto' in query:
            self.send_response(302)
            self.send_header('Location', back)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        script = f'<script>setTimeout(() => {{ location.href = {json.dumps(back)}; }}, 100);</script>' if 'script' in query else ''
        body = f'<!doctype html><title>Sign in</title><body>Sign in{script}</body>'.encode()
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

    def start(self, *names, access, installed=None, profile=None, files=None, focus=True, withhold=()):
        """A host with the fixtures `names` loaded, each granted what the
        Browser grants an installed extension (`access`: name -> (permissions,
        hosts[, scriptable])), with `files` (name -> {file: bytes}) written into its copy. A
        host started again with the `installed` directory and the `profile` of
        an earlier one finds the same extensions there. With `focus`, the
        person has focused the first page. The engine is not told yet of those
        in `withhold`: what it would be told is in `self.withheld`. With
        'manager' among them, it is not told which extension is the Browser's
        own either, as when the Browser starts: it tells that with the rest."""
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
        manager, manager_token = prepare_manager(installed)
        host = Host(args=['--use-mock-keychain', '--load-extension=' + ','.join(map(str, [manager, *paths]))], profile=profile)
        self.addCleanup(host.close)
        ids = {'manager': extension_id(manager)}
        # Its worker tells the engine what rules acted on in the tabs.
        own = {'id': ids['manager'], 'permissions': [], 'hosts': [], 'scriptable': [], 'token': manager_token}
        if 'manager' in withhold:
            self.withheld = {**getattr(self, 'withheld', {}), 'manager': own}
        else:
            host.send(op='extension_identity', id=ids['manager'])
        # The Browser tells the engine what each extension may do once it has
        # read its profile: after the extensions' workers started.
        host.settle(1.0)
        if 'manager' not in withhold:
            host.send(op='extension_access', **own)
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
            if name in withhold:
                self.withheld = {**getattr(self, 'withheld', {}), name: command}
                continue
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

    def test_what_follows_an_extensions_keyword_reaches_it_and_enter_goes_where_it_says(self):
        host, ids = self.start('keyword', access={'keyword': (['activeTab'], [])})
        extension = ids['keyword']
        # The worker starts with the engine, in its own time.
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, extension, 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        host.navigate(self.url + '/typed')
        host.settle(1.0)

        def omnibox(input, **fields):
            host.send(op='extension_omnibox', id=extension, input=input, **fields)

        def suggested(request):
            return host.until(lambda e: e['event'] == 'extension_omnibox_suggestions' and e['request'] == request)

        omnibox('started')
        # Its default suggestion, which it set as it started, for this tab:
        # told as the person starts, or as it arrives.
        default = host.until(lambda e: e['event'] == 'extension_omnibox_default' and e.get('description'))
        self.assertEqual((default.get('tab', 1), default['id'], default['description']),
                         (1, extension, 'Look up <match>%s</match>'))
        omnibox('changed', text='ca', request=1)
        omnibox('changed', text='cat', request=2)
        told = suggested(2)
        self.assertEqual((told.get('tab', 1), told['id']), (1, extension))
        self.assertEqual(told['suggestions'], [
            {'content': 'cat one', 'description': 'First <match>cat</match> &amp; more'},
            {'content': 'cat two', 'description': 'Second'},
        ])
        # Left, and begun again: Enter on a suggestion sends its content.
        omnibox('cancelled')
        omnibox('started')
        omnibox('changed', text='dog', request=3)
        suggested(3)
        omnibox('entered', text='dog two')
        found = self.title(host, 1, 'page /found?')
        heard = json.loads(urllib.parse.unquote(found[len('page /found?'):]))
        self.assertEqual(heard, ['started', 'changed:ca', 'changed:cat', 'cancelled',
                                 'started', 'changed:dog', 'entered:dog two:currentTab'])

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

    def test_a_tab_keeps_its_id_from_a_blank_or_extension_page_to_a_web_page(self):
        host, ids = self.start('keeper', access={'keeper': (['tabs', 'storage'], ['http://127.0.0.1/*'])})
        host.navigate(self.url + '/first')
        # A new tab, still blank, and the extension's welcome page in a tab of
        # its own, as one opens on install: it keeps both tabs' ids.
        for tab in (3, 4):
            host.send(op='create_tab', tab=tab)
            host.until(lambda e, tab=tab: e['event'] == 'tab_ready' and e.get('tab') == tab)
        host.send(op='navigate', tab=4, url=f'chrome-extension://{ids["keeper"]}/welcome.html')
        welcome = json.loads(self.title(host, 4, 'welcome:')[len('welcome:'):])
        self.assertEqual(welcome, {'own': True, 'blank': True, 'distinct': True})
        # Both go on to web pages, whose messages carry the id Chromium gives
        # the tab: the one kept, which the tab APIs still know it by.
        host.send(op='navigate', tab=3, url=self.url + '/was-blank')
        host.send(op='navigate', tab=4, url=self.url + '/was-welcome')
        blank = json.loads(self.title(host, 3, 'kept:')[len('kept:'):])
        self.assertEqual(blank, {'welcome': False, 'blank': True, 'found': self.url + '/was-blank', 'listed': 1})
        welcomed = json.loads(self.title(host, 4, 'kept:')[len('kept:'):])
        self.assertEqual(welcomed, {'welcome': True, 'blank': False, 'found': self.url + '/was-welcome', 'listed': 1})

    def test_two_extension_pages_that_trade_their_words_keep_their_own_tabs(self):
        host, ids = self.start('swapper', access={'swapper': (['tabs'], ['http://127.0.0.1/*'])})
        host.navigate(self.url + '/first')
        # Two of the extension's pages, each in a tab of its own, trade the
        # words the Browser has each send to say which tab it is.
        for tab in (3, 4):
            host.send(op='create_tab', tab=tab)
            host.until(lambda e, tab=tab: e['event'] == 'tab_ready' and e.get('tab') == tab)
        for tab, side in ((3, 'a'), (4, 'b')):
            host.send(op='navigate', tab=tab, url=f'chrome-extension://{ids["swapper"]}/page.html?side={side}')
        self.assertEqual(self.title(host, 3, 'traded'), 'traded')
        self.assertEqual(self.title(host, 4, 'traded'), 'traded')
        # On web pages, a message carries the id Chromium gives its tab, by
        # which the tab APIs find that tab, not the other.
        host.send(op='navigate', tab=3, url=self.url + '/traded-a')
        host.send(op='navigate', tab=4, url=self.url + '/traded-b')
        self.assertEqual(json.loads(self.title(host, 3, 'found:')[len('found:'):]), self.url + '/traded-a')
        self.assertEqual(json.loads(self.title(host, 4, 'found:')[len('found:'):]), self.url + '/traded-b')

    def grants(self, host, extension, optional, granted=((), ())):
        """Tell the engine what `extension` may ask for later and what of it
        the person granted, as the Browser does once it read the profile
        (extension_controller.rs, tell_access)."""
        host.send(op='extension_grants', id=extension,
                  optional={'permissions': list(optional[0]), 'hosts': list(optional[1])},
                  granted={'permissions': list(granted[0]), 'hosts': list(granted[1])})

    def worker_value(self, host, extension, expression, predicate):
        """`expression` in the extension's worker once `predicate` holds of it."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            value = self.in_worker(host, extension, expression)
            if value is not None and predicate(value):
                return value
            self.assertLess(time.monotonic(), deadline, f'the worker still has {value!r}')
            host.settle(0.25)

    def told(self, host, predicate):
        """The first event `predicate` holds of, also one that came while the
        test waited for something else, which is then used up."""
        seen = next((e for e in host.seen if predicate(e)), None)
        if seen is not None:
            host.seen.remove(seen)
            return seen
        event = host.until(predicate)
        host.seen.remove(event)
        return event

    def click_panel(self, host, tab):
        self.click(host, tab, 50, 50)

    def click(self, host, tab, x, y):
        """The person's click at (`x`, `y`) of the tab."""
        host.send(op='mouse_move', tab=tab, x=x, y=y)
        host.send(op='mouse_button', tab=tab, button='left', down=True, x=x, y=y)
        host.send(op='mouse_button', tab=tab, button='left', down=False, x=x, y=y)

    def test_an_extension_asks_for_more_access_and_the_person_decides(self):
        host, ids = self.start('permit', access={'permit': (['storage', 'scripting'], [])})
        permit = ids['permit']
        self.grants(host, permit, (['tabs'], ['http://127.0.0.1/*']))
        page = self.url + '/permit-page'
        host.navigate(page)
        self.worker_value(host, permit, 'typeof heard', lambda value: value == 'object')
        # It holds what its manifest requires, and may not script the page.
        self.assertEqual(self.in_worker(host, permit, 'script()'),
                         'refused: Cannot access contents of the page. Extension manifest must request permission to access the respective host.')
        self.assertEqual(self.in_worker(host, permit, 'urls()'), [None])
        self.panel(host, 2, f'chrome-extension://{permit}/popup.html')
        ready = json.loads(self.title(host, 2, 'ready:')[len('ready:'):])
        self.assertEqual(ready, {'unasked': 'This function must be called during a user gesture',
                                 'all': {'permissions': ['scripting', 'storage'], 'origins': []},
                                 'tabs': False, 'site': False, 'url': None})
        # A click in its popup asks the Browser, for what it lacks of what
        # the manifest offers.
        self.click_panel(host, 2)
        asked = self.told(host, lambda e: e['event'] == 'extension_permission_request')
        self.assertEqual((asked.get('tab'), asked['id'], asked['permissions'], asked['origins']),
                         (2, permit, ['tabs'], ['http://127.0.0.1/*']))
        self.assertFalse(any(e['event'] == 'title' and e['title'].startswith('asked:') for e in host.seen))
        host.send(op='extension_permission_decision', request=asked['request'], allow=True)
        answered = json.loads(self.title(host, 2, 'asked:')[len('asked:'):])
        self.assertEqual(answered, {
            'granted': True, 'error': None, 'undeclared': 'Only permissions specified in the manifest may be requested.',
            'all': {'permissions': ['scripting', 'storage', 'tabs'], 'origins': ['http://127.0.0.1/*']},
            'tabs': True, 'site': True, 'url': page})
        # The Browser keeps what it was granted with the extension.
        kept = self.told(host, lambda e: e['event'] == 'extension_permissions')
        self.assertEqual((kept['id'], kept['permissions'], kept['origins']), (permit, ['tabs'], ['http://127.0.0.1/*']))
        # Its worker hears of it, sees the page and may script it.
        added = {'added': {'permissions': ['tabs'], 'origins': ['http://127.0.0.1/*']}}
        self.worker_value(host, permit, 'JSON.stringify(heard)', lambda value: json.loads(value) == [added])
        self.assertEqual(self.in_worker(host, permit, 'urls()'), [page])
        self.assertEqual(self.in_worker(host, permit, 'script()'), '/permit-page')

        # It gives the site up, never what its manifest requires.
        self.panel(host, 3, f'chrome-extension://{permit}/popup.html?drop')
        dropped = json.loads(self.title(host, 3, 'dropped:')[len('dropped:'):])
        self.assertEqual(dropped, {
            'required': 'You cannot remove required permissions.', 'removed': True,
            'all': {'permissions': ['scripting', 'storage', 'tabs'], 'origins': []},
            'tabs': True, 'site': False, 'url': page})
        kept = self.told(host, lambda e: e['event'] == 'extension_permissions')
        self.assertEqual(kept['permissions'], ['tabs'])
        removed = {'removed': {'permissions': [], 'origins': ['http://127.0.0.1/*']}}
        self.worker_value(host, permit, 'JSON.stringify(heard)', lambda value: json.loads(value) == [added, removed])
        self.assertTrue(self.in_worker(host, permit, 'script()').startswith('refused: '))

        # Asked again, for the site alone, and refused: it holds no more.
        self.panel(host, 4, f'chrome-extension://{permit}/popup.html')
        self.title(host, 4, 'ready:')
        self.click_panel(host, 4)
        asked = self.told(host, lambda e: e['event'] == 'extension_permission_request')
        self.assertEqual((asked['permissions'], asked['origins']), ([], ['http://127.0.0.1/*']))
        host.send(op='extension_permission_decision', request=asked['request'], allow=False)
        refused = json.loads(self.title(host, 4, 'asked:')[len('asked:'):])
        self.assertEqual((refused['granted'], refused['site'], refused['tabs']), (False, False, True))
        self.assertTrue(self.in_worker(host, permit, 'script()').startswith('refused: '))

    def test_access_granted_in_an_earlier_run_holds_from_the_start(self):
        host, ids = self.start('permit', access={'permit': (['storage', 'scripting'], [])})
        permit = ids['permit']
        # What the manifest no longer offers is no grant.
        self.grants(host, permit, (['tabs'], ['http://127.0.0.1/*']),
                    (['tabs', 'history'], ['http://127.0.0.1/*', 'https://example.org/*']))
        page = self.url + '/granted-before'
        host.navigate(page)
        self.worker_value(host, permit, 'typeof heard', lambda value: value == 'object')
        self.assertEqual(self.in_worker(host, permit, 'chrome.permissions.getAll()'),
                         {'permissions': ['scripting', 'storage', 'tabs'], 'origins': ['http://127.0.0.1/*']})
        self.assertEqual(self.in_worker(host, permit, 'urls()'), [page])
        self.assertEqual(self.in_worker(host, permit, 'script()'), '/granted-before')
        # A site granted on a port of its own, part of the manifest's site on
        # every port, reaches the pages on that port and no other.
        port = self.server.server_port
        other = port + 1 if port < 65535 else port - 1
        self.grants(host, permit, (['tabs'], ['http://127.0.0.1/*']), (['tabs'], [f'http://127.0.0.1:{other}/*']))
        self.assertTrue(self.in_worker(host, permit, 'script()').startswith('refused: '))
        self.grants(host, permit, (['tabs'], ['http://127.0.0.1/*']), (['tabs'], [f'http://127.0.0.1:{port}/*']))
        self.assertEqual(self.in_worker(host, permit, 'chrome.permissions.getAll()'),
                         {'permissions': ['scripting', 'storage', 'tabs'], 'origins': [f'http://127.0.0.1:{port}/*']})
        self.assertEqual(self.in_worker(host, permit, 'script()'), '/granted-before')

    def test_access_the_person_revokes_goes_at_once_and_the_extension_hears_of_it(self):
        host, ids = self.start('permit', access={'permit': (['storage', 'scripting'], [])})
        permit = ids['permit']
        offered = (['tabs'], ['http://127.0.0.1/*'])
        self.grants(host, permit, offered, offered)
        page = self.url + '/revoked'
        host.navigate(page)
        self.worker_value(host, permit, 'typeof heard', lambda value: value == 'object')
        self.assertEqual(self.in_worker(host, permit, 'urls()'), [page])
        self.assertEqual(self.in_worker(host, permit, 'script()'), '/revoked')
        # The person revokes it in the Library: the Browser tells the engine
        # the extension holds none of it, which the extension hears of, and
        # its next tab calls are refused.
        self.grants(host, permit, offered)
        removed = {'removed': {'permissions': ['tabs'], 'origins': ['http://127.0.0.1/*']}}
        self.worker_value(host, permit, 'JSON.stringify(heard)', lambda value: json.loads(value) == [removed])
        self.assertEqual(self.in_worker(host, permit, 'urls()'), [None])
        self.assertTrue(self.in_worker(host, permit, 'script()').startswith('refused: '))
        self.assertEqual(self.in_worker(host, permit, 'chrome.permissions.getAll()'),
                         {'permissions': ['scripting', 'storage'], 'origins': []})
        # Told again that it holds none, it hears nothing more.
        self.grants(host, permit, offered)
        host.settle(0.5)
        self.assertEqual(json.loads(self.in_worker(host, permit, 'JSON.stringify(heard)')), [removed])

    def test_a_question_left_unanswered_goes_with_its_window(self):
        host, ids = self.start('permit', access={'permit': (['storage', 'scripting'], [])})
        permit = ids['permit']
        self.grants(host, permit, (['tabs'], ['http://127.0.0.1/*']))
        host.navigate(self.url + '/asked-again')
        self.panel(host, 2, f'chrome-extension://{permit}/popup.html')
        self.title(host, 2, 'ready:')
        self.click_panel(host, 2)
        first = self.told(host, lambda e: e['event'] == 'extension_permission_request')
        # Its popup closes with the window, the question unanswered.
        host.send(op='close_tab', tab=2)
        host.until(lambda e: e['event'] == 'closed' and e.get('tab') == 2)
        # Opened again, it may ask again, and the person's answer counts.
        self.panel(host, 3, f'chrome-extension://{permit}/popup.html')
        self.title(host, 3, 'ready:')
        self.click_panel(host, 3)
        second = self.told(host, lambda e: e['event'] == 'extension_permission_request')
        self.assertNotEqual(second['request'], first['request'])
        host.send(op='extension_permission_decision', request=first['request'], allow=True)
        host.send(op='extension_permission_decision', request=second['request'], allow=True)
        answered = json.loads(self.title(host, 3, 'asked:')[len('asked:'):])
        self.assertEqual((answered['granted'], answered['error'], answered['tabs'], answered['site']),
                         (True, None, True, True))

    def test_an_extension_granted_signing_in_later_signs_in(self):
        host, ids = self.start('latecomer', access={'latecomer': ([], [])})
        late = ids['latecomer']
        self.grants(host, late, (['identity'], []))
        host.navigate(self.url + '/front')
        host.settle(1.0)
        read = lambda expression: self.in_worker(host, late, expression)
        # Until the person grants it, it may not.
        self.assertEqual(read("chrome.identity.launchWebAuthFlow({url: 'https://example.org/', interactive: true})"
                              ".then(() => 'signed in', (e) => e.message)"),
                         'The extension did not ask for the identity permission')
        # Granted, as the Browser keeps it, it signs in as one whose manifest
        # requires it.
        self.grants(host, late, (['identity'], []), (['identity'], []))
        back = f'https://{late}.chromiumapp.org/'
        query = urllib.parse.urlencode({'redirect_uri': back + 'cb', 'code': 'later', 'auto': '1'})
        url = f'{self.url}/authorize?{query}'
        self.assertEqual(read(f'signIn({{url: {json.dumps(url)}, interactive: true}})'), 'started')
        asked = self.told(host, lambda e: e['event'] == 'extension_sign_in')
        self.assertEqual((asked['id'], asked['url'], asked['interactive']), (late, url, True))
        host.send(op='create_tab', tab=2)
        host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 2)
        host.send(op='extension_sign_in_panel', tab=2, id=late, flow=asked['flow'])
        host.send(op='resize', tab=2, width=800, height=600, scale_factor=1.0)
        host.send(op='navigate', tab=2, url=url)
        signed = self.worker_value(host, late, 'JSON.stringify(signIns)', lambda value: json.loads(value))
        self.assertEqual(json.loads(signed), [{'url': back + 'cb?code=later'}])

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

    def test_an_extension_follows_navigations_and_tab_events_by_the_tabs_ids(self):
        host, ids = self.start('navigator', access={'navigator': (['webNavigation', 'tabs'], [])})
        heard = lambda: json.loads(self.in_worker(host, ids['navigator'], 'JSON.stringify(heard)'))

        def until(what, found):
            """What the worker heard once `found` (of it) holds."""
            deadline = time.monotonic() + HANG_BOUND
            while True:
                seen = heard()
                if found(seen):
                    return seen
                if time.monotonic() > deadline:
                    self.fail(f'the worker never heard {what}: {seen}')
                host.settle(0.25)

        def details(seen, name, **match):
            return [h['args'][0] for h in seen if h['name'] == name
                    and all(h['args'][0].get(key) == value for key, value in match.items())]

        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['navigator'], 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        framed = self.url + '/framed'
        host.navigate(framed)
        until('the framed page load', lambda seen: details(seen, 'webNavigation.onCompleted', frameId=0, url=framed))
        [first] = json.loads(self.in_worker(host, ids['navigator'], 'chrome.tabs.query({}).then((tabs) => JSON.stringify(tabs))'))
        tab = first['id']
        seen = until('both frames load', lambda seen: len(details(seen, 'webNavigation.onCompleted')) >= 3)
        # Every frame's navigation, by the id the tab APIs know the tab by.
        navigations = [h['args'][0] for h in seen if h['name'].startswith('webNavigation.')]
        self.assertEqual({n['tabId'] for n in navigations}, {tab})
        frames = json.loads(self.in_worker(host, ids['navigator'],
                                           f'chrome.webNavigation.getAllFrames({{tabId: {tab}}}).then(JSON.stringify)'))
        port = self.server.server_port
        self.assertEqual(sorted((f['url'], f['parentFrameId'] if f['frameId'] else -1) for f in frames), [
            (framed, -1), (self.url + '/inner', 0), (f'http://localhost:{port}/outer', 0)])
        main = json.loads(self.in_worker(host, ids['navigator'],
                                         f'chrome.webNavigation.getFrame({{tabId: {tab}, frameId: 0}}).then(JSON.stringify)'))
        self.assertEqual(main['url'], framed)

        # The Browser zooms the page.
        host.send(op='zoom', level=1.0)
        [zoom] = details(until('the zoom', lambda seen: details(seen, 'tabs.onZoomChange')), 'tabs.onZoomChange')
        self.assertEqual(zoom['tabId'], tab)
        self.assertAlmostEqual(zoom['oldZoomFactor'], 1)
        self.assertAlmostEqual(zoom['newZoomFactor'], 1.2)
        self.assertEqual(zoom['zoomSettings']['scope'], 'per-origin')
        host.send(op='zoom', level=0.0)
        until('the zoom back', lambda seen: len(details(seen, 'tabs.onZoomChange')) == 2)

        # A link in the page opens a tab, which the Browser opens beside, as
        # here the test does; then one in the page's frame.
        opener = self.url + '/opener'
        host.navigate(opener)
        seen = until('the opener load', lambda seen: details(seen, 'webNavigation.onCompleted', url=self.url + '/opener-frame'))
        [committed] = details(seen, 'webNavigation.onCommitted', url=opener)
        [in_frame] = details(seen, 'webNavigation.onCommitted', url=self.url + '/opener-frame')
        host.settle(0.5)
        opened_ids = {}
        for new, (x, y), path in ((3, (40, 30), '/opened'), (4, (40, 200), '/from-frame')):
            mark = len(host.seen)
            host.send(op='mouse_button', button='left', down=True, x=x, y=y)
            host.send(op='mouse_button', button='left', down=False, x=x, y=y)
            asked = next((e for e in host.seen[mark:] if e['event'] == 'open_tab'), None) or host.until(lambda e: e['event'] == 'open_tab')
            self.assertEqual(asked['url'], self.url + path)
            host.send(op='create_tab', tab=new)
            host.until(lambda e, new=new: e['event'] == 'tab_ready' and e.get('tab') == new)
            host.send(op='navigate', tab=new, url=asked['url'])
            seen = until('the opened tab', lambda seen, path=path: details(seen, 'webNavigation.onCreatedNavigationTarget', url=self.url + path))
            [target] = details(seen, 'webNavigation.onCreatedNavigationTarget', url=self.url + path)
            opened_ids[new] = target['tabId']
            source = committed if new == 3 else in_frame
            self.assertEqual({key: target[key] for key in ('sourceTabId', 'sourceFrameId', 'sourceProcessId')},
                             {'sourceTabId': tab, 'sourceFrameId': source['frameId'], 'sourceProcessId': source['processId']})
            self.assertNotIn('sourceFrameUrl', target)
            # The tab told of is the one the tab APIs and its navigations know.
            until('the opened tab load', lambda seen, path=path: details(seen, 'webNavigation.onCommitted', url=self.url + path))
            [loaded] = details(heard(), 'webNavigation.onCommitted', url=self.url + path)
            self.assertEqual(loaded['tabId'], target['tabId'])
            got = json.loads(self.in_worker(host, ids['navigator'], f"chrome.tabs.get({target['tabId']}).then(JSON.stringify)"))
            self.assertEqual((got['url'], got['openerTabId']), (self.url + path, tab))
        self.assertNotEqual(opened_ids[3], opened_ids[4])
        self.assertTrue({opened_ids[3], opened_ids[4]} <= {t['id'] for t in details(heard(), 'tabs.onCreated')})

        # The person goes to the first opened tab: active, and highlighted.
        host.send(op='focus', tab=3, focused=True)
        seen = until('the tab made active', lambda seen: details(seen, 'tabs.onHighlighted', windowId=opened_ids[3]))
        self.assertEqual(details(seen, 'tabs.onHighlighted', windowId=opened_ids[3]), [{'windowId': opened_ids[3], 'tabIds': [opened_ids[3]]}])
        self.assertIn({'tabId': opened_ids[3], 'windowId': opened_ids[3]}, details(seen, 'tabs.onActivated'))
        # Each tab stays in a window of its own, as it was made.
        never = {'tabs.onReplaced', 'tabs.onAttached', 'tabs.onDetached', 'tabs.onMoved', 'webNavigation.onTabReplaced'}
        self.assertEqual([h for h in heard() if h['name'] in never], [])

    def test_tabs_a_page_opens_at_once_are_each_told_of_with_the_link_it_opened(self):
        # Two links clicked one after the other, the second's address
        # extending the first's; each tab the Browser opens is a program of
        # its own, so the second may show its page first, as here.
        host, ids = self.start('navigator', access={'navigator': (['webNavigation', 'tabs'], [])})
        heard = lambda: json.loads(self.in_worker(host, ids['navigator'], 'JSON.stringify(heard)'))
        targets = lambda: [h['args'][0] for h in heard() if h['name'] == 'webNavigation.onCreatedNavigationTarget']
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['navigator'], 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        page = self.url + '/openers'
        host.navigate(page)
        [first] = json.loads(self.in_worker(host, ids['navigator'], 'chrome.tabs.query({}).then((tabs) => JSON.stringify(tabs))'))
        host.settle(0.5)
        asked = []
        for y in (50, 150):
            mark = len(host.seen)
            host.send(op='mouse_button', button='left', down=True, x=40, y=y)
            host.send(op='mouse_button', button='left', down=False, x=40, y=y)
            asked.append((next((e for e in host.seen[mark:] if e['event'] == 'open_tab'), None)
                          or host.until(lambda e: e['event'] == 'open_tab'))['url'])
        self.assertEqual(asked, [self.url + '/opened', self.url + '/opened/more'])
        for count, (new, path) in enumerate(((3, '/opened/more'), (4, '/opened')), start=1):
            host.send(op='create_tab', tab=new)
            host.until(lambda e, new=new: e['event'] == 'tab_ready' and e.get('tab') == new)
            host.send(op='navigate', tab=new, url=self.url + path)
            deadline = time.monotonic() + HANG_BOUND
            while len(told := targets()) < count:
                self.assertLess(time.monotonic(), deadline, f'no tab opened was told of after {path}: {told}')
                host.settle(0.25)
            # The tab told of shows the link it was told of, which the page opened.
            target = told[-1]
            self.assertEqual((target['url'], target['sourceTabId']), (self.url + path, first['id']))
            committed = lambda: [h for h in heard() if h['name'] == 'webNavigation.onCommitted' and h['args'][0]['url'] == self.url + path]
            while not (loaded := committed()):
                self.assertLess(time.monotonic(), deadline, f'{path} never committed')
                host.settle(0.25)
            self.assertEqual(loaded[0]['args'][0]['tabId'], target['tabId'])
            got = json.loads(self.in_worker(host, ids['navigator'], f"chrome.tabs.get({target['tabId']}).then(JSON.stringify)"))
            self.assertEqual((got['url'], got['openerTabId']), (self.url + path, first['id']))
        self.assertEqual(len(targets()), 2)

    def test_a_worker_hears_of_a_page_that_loaded_before_the_browser_said_whose_it_is(self):
        # As the Browser starts: the worker runs and listens, a page loads,
        # and only then does the Browser tell the engine whose worker it is.
        host, ids = self.start('watcher', access={'watcher': (['scripting'], ['http://127.0.0.1/*'])}, withhold=('watcher',))
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['watcher'], 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        host.settle(0.5)
        host.navigate(self.url + '/watched')
        watched = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('watched:')
        self.assertFalse(any(watched(e) for e in host.seen), 'an extension the engine was not told of scripted the page')
        host.send(op='extension_access', **self.withheld['watcher'])
        try:
            title = (next((e for e in host.seen if watched(e)), None) or host.until(watched, timeout=20))['title']
        except (TimeoutError, queue.Empty):
            self.fail(f"the worker heard {self.in_worker(host, ids['watcher'], 'JSON.stringify(heard)')}")
        self.assertRegex(title, r'^watched:\d+$')

    def test_a_page_heard_of_before_the_browser_said_whose_it_is_has_its_own_id(self):
        # As the Browser starts it tells the engine which extension is its own
        # with the rest, after a page may have loaded: the worker hears of the
        # page by the id Chromium gives it, the one every other call names,
        # not by the engine's own number for the tab.
        host, ids = self.start('watcher', access={'watcher': (['scripting'], ['http://127.0.0.1/*'])}, withhold=('manager', 'watcher'))
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['watcher'], 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)
        host.settle(0.5)
        host.navigate(self.url + '/watched')
        host.settle(0.5)
        host.send(op='extension_identity', id=ids['manager'])
        host.send(op='extension_access', **self.withheld['manager'])
        host.send(op='extension_access', **self.withheld['watcher'])
        watched = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('watched:')
        try:
            title = (next((e for e in host.seen if watched(e)), None) or host.until(watched, timeout=20))['title']
        except (TimeoutError, queue.Empty):
            self.fail(f"the worker heard {self.in_worker(host, ids['watcher'], 'JSON.stringify(heard)')}")
        real = self.in_worker(host, ids['watcher'], "chrome.tabs.query({}).then((tabs) => tabs.filter((t) => (t.url || '').endsWith('/watched')).map((t) => t.id))")
        heard = self.in_worker(host, ids['watcher'], "[...new Set(heard.filter((h) => (h.url || '').endsWith('/watched')).map((h) => h.tabId))]")
        self.assertEqual(len(real), 1, real)
        self.assertEqual(heard, real)
        self.assertEqual(title, f'watched:{real[0]}')

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
            # Chromium's words for a target it refuses (scripting_utils.cc).
            'both': "error: Cannot specify 'allFrames' if either 'frameIds' or 'documentIds' is specified.",
            'allAndDocuments': "error: Cannot specify 'allFrames' if either 'frameIds' or 'documentIds' is specified.",
            'idsAndDocuments': "error: Cannot specify both 'frameIds' and 'documentIds'.",
            'malformed': 'error: Invalid document id nope',
            'gone': 'error: No document with id 0123456789ABCDEF0123456789ABCDEF in tab with id %d' % tab,
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

    def test_notifications_reach_the_browser_and_a_choice_reaches_the_worker(self):
        host, ids = self.start('notifier', access={'notifier': (['notifications'], [])})
        notifier = ids['notifier']
        host.navigate(self.url + '/notified')
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, notifier, 'typeof heard') != 'object':
            self.assertLess(time.monotonic(), deadline, 'the worker never started')
            host.settle(0.25)

        def told(mark, notification):
            """What the Browser was told of `notification` after `mark`."""
            match = lambda e: e['event'] == 'extension_notification' and e['notification'] == notification
            return next((e for e in host.seen[mark:] if match(e)), None) or host.until(match)

        def heard(count):
            """What the worker heard, once it heard `count` things."""
            deadline = time.monotonic() + HANG_BOUND
            while True:
                seen = json.loads(self.in_worker(host, notifier, 'JSON.stringify(heard)'))
                if len(seen) >= count:
                    return seen
                self.assertLess(time.monotonic(), deadline, f'the worker heard only {seen}')
                host.settle(0.25)

        # A progress notification with buttons: the Browser is told what it
        # shows, through the page in front, with the two buttons Chrome shows.
        mark = len(host.seen)
        created = self.in_worker(host, notifier, """chrome.notifications.create('copy', {
            type: 'progress', iconUrl: 'icon.png', title: 'Copying', message: 'Two files', progress: 40,
            buttons: [{title: 'Pause'}, {title: 'Stop'}, {title: 'Third'}]})""")
        self.assertEqual(created, 'copy')
        shown = told(mark, 'copy')
        self.assertEqual({key: shown.get(key) for key in ('tab', 'id', 'title', 'message', 'progress', 'buttons', 'items')},
                         {'tab': 1, 'id': notifier, 'title': 'Copying', 'message': 'Two files', 'progress': 40,
                          'buttons': ['Pause', 'Stop'], 'items': []})
        # Shown anew it may sound; changed, as a progress is, it does not.
        self.assertIs(shown.get('silent'), False)

        # Without an id, with a callback: one is made up. A missing property
        # sets runtime.lastError, as in Chrome.
        made = self.in_worker(host, notifier, """new Promise((resolve) => chrome.notifications.create(
            {type: 'basic', iconUrl: 'icon.png', title: 'Hello', message: 'World'}, resolve))""")
        self.assertRegex(made, r'^[0-9a-f-]{36}$')
        self.assertNotIn('progress', told(mark, made))
        refused = self.in_worker(host, notifier, """new Promise((resolve) => chrome.notifications.create(
            'bad', {type: 'basic', title: 'No message'}, () => resolve(chrome.runtime.lastError.message)))""")
        self.assertEqual(refused, 'Some of the required properties are missing: type, iconUrl, title and message.')

        # An update keeps what it does not name; one of a notification not
        # shown changes nothing.
        mark = len(host.seen)
        self.assertIs(self.in_worker(host, notifier, "chrome.notifications.update('copy', {progress: 80})"), True)
        updated = told(mark, 'copy')
        self.assertEqual((updated['title'], updated['progress'], updated['silent']), ('Copying', 80, True))
        self.assertIs(self.in_worker(host, notifier, "chrome.notifications.update('none', {progress: 1})"), False)
        self.assertEqual(self.in_worker(host, notifier, 'chrome.notifications.getAll()'), {'copy': True, made: True})

        # The person chooses a button, then the other's body: the worker
        # hears which, and that each closed by the person's hand.
        host.send(op='extension_notification_chosen', id=notifier, notification='copy', button=1)
        self.assertEqual(heard(2), [['button', 'copy', 1], ['closed', 'copy', True]])
        host.send(op='extension_notification_chosen', id=notifier, notification=made)
        self.assertEqual(heard(4)[2:], [['clicked', made], ['closed', made, True]])
        self.assertEqual(self.in_worker(host, notifier, 'chrome.notifications.getAll()'), {})

        # Cleared by the extension: the Browser takes it away, and the worker
        # hears it closed, not by the person.
        self.in_worker(host, notifier, """chrome.notifications.create('later', {
            type: 'basic', iconUrl: 'icon.png', title: 'Later', message: 'Soon'})""")
        mark = len(host.seen)
        self.assertIs(self.in_worker(host, notifier, "chrome.notifications.clear('later')"), True)
        self.assertIs(told(mark, 'later').get('cleared'), True)
        self.assertEqual(heard(5)[4:], [['closed', 'later', False]])
        self.assertIs(self.in_worker(host, notifier, "chrome.notifications.clear('later')"), False)

    # chrome.contextMenus: items in the page's menu, and a choice reaching
    # the extension.
    def menu(self, host, x, y):
        """Right-click at (x, y) and read the menu that opens, then close it:
        its items by label."""
        host.send(op='mouse_move', x=x, y=y)
        host.send(op='mouse_button', button='right', down=True, x=x, y=y)
        host.send(op='mouse_button', button='right', down=False, x=x, y=y)
        menu = host.until(lambda e: e['event'] == 'context_menu')
        return menu, labelled(menu['items'])

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

    def test_check_marks_radio_groups_submenus_and_the_buttons_menu(self):
        access = {'menus': (['contextMenus', 'activeTab', 'scripting'], [])}
        host, ids = self.start('menus', access=access)
        page = self.url + '/menus'
        host.navigate(page)
        menu, items = self.page_menu(host, 400, 400, 'Save && share')
        rows = {item['label']: item for item in walk(menu['items']) if 'label' in item}
        # A checkbox and a radio group with their state, set apart from the
        # items around them; a submenu; a disabled item greyed; an ampersand
        # kept, not read as marking a key.
        self.assertIs(rows['Remember the page']['checked'], False)
        self.assertEqual((rows['Light']['checked'], rows['Dark']['checked']), (False, True))
        labels = [item.get('label', '-') for item in menu['items']]
        light = labels.index('Light')
        self.assertEqual(labels[light - 1:light + 3], ['-', 'Light', 'Dark', '-'], labels)
        self.assertEqual([item['label'] for item in rows['Translate']['items']], ['To German'])
        self.assertNotIn(rows['Translate']['id'], items.values(), 'a submenu is no command')
        self.assertIs(rows['Not here']['enabled'], False)
        self.assertNotIn('Sort', rows, "the button's items stay out of the page's menu")

        # A radio item chosen: it is checked and its group's other is not.
        chosen = self.choose(host, menu, items, 'Light')
        self.assertEqual((chosen['info']['wasChecked'], chosen['info']['checked']), (False, True))
        menu, items = self.menu(host, 400, 400)
        rows = {item['label']: item for item in walk(menu['items']) if 'label' in item}
        self.assertEqual((rows['Light']['checked'], rows['Dark']['checked']), (True, False))
        # An item in a submenu.
        chosen = self.choose(host, menu, items, 'To German')
        self.assertEqual((chosen['info']['menuItemId'], chosen['info']['parentMenuItemId']), ('german', 'translate'))

        # The button's menu reaches the Browser: the items made for it and
        # those for every context, which Chrome shows there too, whatever
        # the page.
        def button_menu(since, checked):
            match = lambda e: (e['event'] == 'extension_menu' and e.get('tab', 1) == 1 and e['id'] == ids['menus']
                               and any(item.get('label') == 'Sort' and item.get('items', [{}])[0].get('checked') == checked
                                       for item in e['items']))
            return next((e for e in host.seen[since:] if match(e)), None) or host.until(match)
        told = button_menu(0, True)
        self.assertEqual(told['items'], [
            {'type': 'normal', 'key': '"elsewhere"', 'label': 'Elsewhere', 'enabled': True},
            {'type': 'normal', 'key': '"renamed"', 'label': 'Everywhere', 'enabled': True},
            {'type': 'normal', 'key': '"sort"', 'label': 'Sort', 'enabled': True, 'items': [
                {'type': 'checkbox', 'key': '"by-name"', 'label': 'By name', 'enabled': True, 'checked': True}]},
        ])
        # Chosen over the page: the extension hears of it with the tab, with
        # no page context, and may script the page.
        before = len(host.seen)
        host.send(op='extension_menu_action', id=ids['menus'], key='"by-name"')
        match = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('chosen:')
        seen = next((e for e in host.seen[before:] if match(e)), None) or host.until(match)
        chosen = json.loads(seen['title'][len('chosen:'):])
        self.assertEqual(chosen['url'], page)
        self.assertEqual(chosen['info'], {'menuItemId': 'by-name', 'parentMenuItemId': 'sort', 'editable': False,
                                          'wasChecked': True, 'checked': False})
        button_menu(before, False)
        # A page item is not the button's to choose.
        before = len(host.seen)
        host.send(op='extension_menu_action', id=ids['menus'], key='"page"')
        host.settle(1.0)
        self.assertFalse([e for e in host.seen[before:] if match(e)], 'a page item chosen as the button\'s')

        # A tab opened later is told what the button's menu holds.
        before = len(host.seen)
        host.send(op='create_tab', tab=2)
        told = host.until(lambda e: e['event'] == 'extension_menu' and e.get('tab') == 2)
        self.assertEqual(told['id'], ids['menus'])
        self.assertEqual([item['label'] for item in told['items']], ['Elsewhere', 'Everywhere', 'Sort'])

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

    def counted(self, host, query, expected):
        """The counted page in tab 1 as it loads once the blockers' rules are
        in, which the engine applies a moment after it starts; and the index
        in host.seen from which the page it settled on is told of."""
        deadline = time.monotonic() + HANG_BOUND
        for attempt in itertools.count():
            mark = len(host.seen)
            host.send(op='navigate', url=f'{self.url}/counted?{query}{attempt}')
            title = host.until(lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('counted:'))['title']
            seen = json.loads(title[len('counted:'):])
            if seen == expected or time.monotonic() > deadline:
                self.assertEqual(seen, expected)
                return mark
            host.settle(0.5)

    def counting(self, host, ids, names):
        """Wait until the Browser's own extension listens for what rules do,
        and each of `names` has asked its button to show the page's count."""
        # A worker attached to as it starts has no `chrome` yet.
        listens = ("typeof globalThis.chrome == 'object' && typeof chrome.webRequest == 'object'"
                   " && chrome.webRequest.onErrorOccurred.hasListeners()")
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, ids['manager'], listens) is not True:
            self.assertLess(time.monotonic(), deadline, 'the Browser\'s extension never listened')
            host.settle(0.25)
        for name in names:
            show = ("typeof globalThis.chrome != 'object' ? 'starting' : chrome.declarativeNetRequest"
                    ".setExtensionActionOptions({displayActionCountAsBadgeText: true}).then(() => 'shown', (e) => e.message)")
            while (shown := self.in_worker(host, ids[name], show)) in ('starting', None):
                self.assertLess(time.monotonic(), deadline, f'{name} never started')
                host.settle(0.25)
            self.assertEqual(shown, 'shown')

    def badge(self, host, since, extension, text, tab=1):
        """The first count told for the extension's button on the tab since
        `since` (an index of host.seen) that is `text`, and its index."""
        told = lambda e: (e['event'] == 'extension_action' and e.get('tab') == tab and e['id'] == extension
                          and not e['global'] and 'badge' in e['change'] and e['change']['badge'] == text)
        for index in range(since, len(host.seen)):
            if told(host.seen[index]):
                return index
        host.until(told)
        return len(host.seen) - 1

    def test_an_ad_blocker_shows_how_many_requests_it_blocked_on_the_page(self):
        host, ids = self.start('tally', access={'tally': TALLY})
        tally = ids['tally']
        self.counting(host, ids, ['tally'])
        mark = self.counted(host, 'first', TALLIED)
        # Two trackers blocked and two scripts put in place of others.
        self.badge(host, mark, tally, '4')
        tab = "(await chrome.tabs.query({}))[0].id"
        # It may not read the count, as in Chrome, nor find which rules matched.
        self.assertEqual(self.in_worker(host, tally, f"(async () => chrome.action.getBadgeText({{tabId: {tab}}}))()"),
                         COUNT_PLACEHOLDER)
        refused = self.in_worker(host, tally, f"(async () => chrome.declarativeNetRequest.getMatchedRules({{tabId: {tab}}})"
                                 ".then(() => 'answered', (e) => e.message))()")
        self.assertIn('declarativeNetRequestFeedback', refused)

        # It changes the count for the page; text of its own shows over it
        # until it takes it back.
        mark = len(host.seen)
        changed = self.in_worker(host, tally, f"(async () => chrome.declarativeNetRequest.setExtensionActionOptions("
                                 f"{{tabUpdate: {{tabId: {tab}, increment: 2}}}}).then(() => 'changed'))()")
        self.assertEqual(changed, 'changed')
        self.badge(host, mark, tally, '6')
        self.in_worker(host, tally, f"(async () => chrome.action.setBadgeText({{tabId: {tab}, text: 'off'}}).then(() => 'set'))()")
        self.badge(host, mark, tally, 'off')
        self.in_worker(host, tally, f"(async () => chrome.action.setBadgeText({{tabId: {tab}, text: null}}).then(() => 'set'))()")
        took_back = self.badge(host, mark, tally, None)
        self.badge(host, took_back, tally, '6')
        missing = self.in_worker(host, tally, "chrome.declarativeNetRequest.setExtensionActionOptions("
                                 "{tabUpdate: {tabId: 2000000000, increment: 1}}).then(() => 'changed', (e) => e.message)")
        self.assertEqual(missing, 'No tab with id: 2000000000.')

        # The next page counts anew: what it changed went with the last.
        mark = self.counted(host, 'next', TALLIED)
        own = self.badge(host, mark, tally, '4')
        reset = next((i for i, e in enumerate(host.seen) if i >= mark and e['event'] == 'extension_action_reset'
                      and e.get('tab') == 1), None)
        self.assertIsNotNone(reset, 'what the last page showed goes')
        self.assertLess(reset, own)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'extension_action' and e['id'] == tally
                          and e['change'].get('badge') == '6'], 'the next page keeps no change of the last')

        # Asked to show no count, the button shows none.
        mark = len(host.seen)
        self.in_worker(host, tally, "chrome.declarativeNetRequest.setExtensionActionOptions("
                       "{displayActionCountAsBadgeText: false}).then(() => 'hidden')")
        self.badge(host, mark, tally, None)

    def test_two_blockers_each_count_what_their_own_rules_did(self):
        host, ids = self.start('tally', 'spotter', access={'tally': TALLY, 'spotter': SPOTTER})
        tally, spotter = ids['tally'], ids['spotter']
        self.counting(host, ids, ['tally', 'spotter'])
        mark = self.counted(host, 'both', {**TALLIED, 'beacon': 'stopped'})
        self.badge(host, mark, tally, '4')
        self.badge(host, mark, spotter, '1')
        host.settle(1.0)
        told = lambda extension: [e['change']['badge'] for e in host.seen[mark:] if e['event'] == 'extension_action'
                                  and e['id'] == extension and 'badge' in e['change']]
        self.assertNotIn('2', told(spotter), 'a blocker counts no request another blocked')
        self.assertNotIn('5', told(tally))

        # The one that may read what its rules matched finds them for the
        # tab, and reads its count.
        tab = "(await chrome.tabs.query({}))[0].id"
        matched = self.in_worker(host, spotter, f"(async () => {{ const id = {tab};"
                                 "const found = await chrome.declarativeNetRequest.getMatchedRules({tabId: id});"
                                 "const since = await chrome.declarativeNetRequest.getMatchedRules({tabId: id, minTimeStamp: Date.now() + 1000});"
                                 "return JSON.stringify({rules: found.rulesMatchedInfo.map((m) => [m.rule.ruleId, m.rule.rulesetId, m.tabId === id,"
                                 "typeof m.timeStamp]), later: since.rulesMatchedInfo.length,"
                                 "badge: await chrome.action.getBadgeText({tabId: id})}); })()")
        self.assertEqual(json.loads(matched), {'rules': [[1, 'beacons', True, 'number']], 'later': 0, 'badge': '1'})
        missing = self.in_worker(host, spotter, "chrome.declarativeNetRequest.getMatchedRules({tabId: 2000000000})"
                                 ".then(() => 'answered', (e) => e.message)")
        self.assertEqual(missing, 'No tab with id: 2000000000.')

        # Asked to show no count, each still counts; asked again on the same
        # page, as uBlock Origin Lite's setting does or its first call once
        # the Browser says whose worker it is, each shows what its own rules
        # did there so far, as in Chrome.
        show = ("chrome.declarativeNetRequest.setExtensionActionOptions({displayActionCountAsBadgeText: %s})"
                ".then(() => 'done', (e) => e.message)")
        for extension in (tally, spotter):
            self.assertEqual(self.in_worker(host, extension, show % 'false'), 'done')
        mark = self.counted(host, 'hidden', {**TALLIED, 'beacon': 'stopped'})
        # The beacon is the page's last request: once the host knows of it,
        # it knows of them all.
        heard = f"(async () => (await chrome.declarativeNetRequest.getMatchedRules({{tabId: {tab}}})).rulesMatchedInfo.length)()"
        deadline = time.monotonic() + HANG_BOUND
        while self.in_worker(host, spotter, heard) != 1:
            self.assertLess(time.monotonic(), deadline, 'the blocked beacon never reached the host')
            host.settle(0.25)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'extension_action' and e['id'] in (tally, spotter)
                          and e['change'].get('badge')], 'a button asked to show no count shows none')
        mark = len(host.seen)
        for extension in (tally, spotter):
            self.assertEqual(self.in_worker(host, extension, show % 'true'), 'done')
        self.badge(host, mark, tally, '4')
        self.badge(host, mark, spotter, '1')

    def test_an_extensions_download_is_the_browsers_and_the_extension_follows_it(self):
        host, ids = self.start('downloader', access={'downloader': (['downloads', 'downloads.open'], [])})
        host.navigate(self.url + '/page')
        worker = lambda expression: self.in_worker(host, ids['downloader'], f'outcome({expression})')

        def first(match):
            """The first event since the last one `first` returned that
            `match`es, also one that came while the test waited for another."""
            for at in range(self.mark, len(host.seen)):
                if match(host.seen[at]):
                    self.mark = at + 1
                    return host.seen[at]
            found = host.until(match)
            self.mark = len(host.seen)
            return found
        self.mark = 0

        # Refused before anything is fetched, with Chrome's errors.
        self.assertEqual(worker("chrome.downloads.download({url: 'file:///etc/hosts'})"), {'error': 'Invalid URL'})
        self.assertEqual(worker(f"chrome.downloads.download({{url: '{self.url}/download/a', filename: '../up.bin'}})"),
                         {'error': 'Invalid filename'})
        self.assertFalse(any(e['event'] == 'download_prompt' for e in host.seen))

        # The person is asked, under the name the extension chose, and the
        # extension has the download's id as it starts.
        address = f'{self.url}/download/kept'
        started = worker(f"chrome.downloads.download({{url: '{address}', filename: 'reports/kept.bin', saveAs: true}})")
        prompt = first(lambda e: e['event'] == 'download_prompt')
        self.assertEqual(started, {'value': prompt['id']})
        self.assertEqual((prompt['name'], prompt['url']), ('kept.bin', address))
        download = prompt['id']
        host.send(op='download_decision', id=download, allow=True)
        first(lambda e: e['event'] == 'download' and e['id'] == download and e['received'] >= 65536)

        # Paused and resumed by the extension, which finds it as it is.
        self.assertEqual(worker(f'chrome.downloads.pause({download})'), {'value': None})
        paused = worker(f'chrome.downloads.search({{id: {download}}})')['value']
        self.assertEqual([(item['paused'], item['state'], item['url']) for item in paused], [(True, 'in_progress', address)])
        self.assertEqual(worker(f'chrome.downloads.resume({download})'), {'value': None})
        done = first(lambda e: e['event'] == 'download' and e['id'] == download and e['state'] != 'progress')
        self.assertEqual(done['state'], 'complete', done)
        self.assertEqual(Path(done['path']).name, 'kept.bin')
        self.assertEqual(Path(done['path']).read_bytes(), DOWNLOAD)
        found = worker("chrome.downloads.search({filenameRegex: 'kept'})")['value']
        self.assertEqual([(item['id'], item['state'], item['filename']) for item in found], [(download, 'complete', done['path'])])

        # It heard of each change, as from Chrome.
        heard = self.in_worker(host, ids['downloader'], 'JSON.stringify(heard)')
        changes = [entry['changed'] for entry in json.loads(heard) if 'changed' in entry and entry['changed']['id'] == download]
        created = [entry['created'] for entry in json.loads(heard) if 'created' in entry]
        self.assertEqual([(item['id'], item['url']) for item in created], [(download, address)])
        self.assertIn({'current': True, 'previous': False}, [change.get('paused') for change in changes])
        self.assertIn({'current': False, 'previous': True}, [change.get('paused') for change in changes])
        self.assertEqual(changes[-1]['state'], {'current': 'complete', 'previous': 'in_progress'})

        # Opened only on the person's click, as in Chrome; shown in its
        # folder at any time. The Browser showing the page does either.
        self.assertEqual(worker(f'chrome.downloads.open({download})'), {'error': 'User gesture required'})
        self.assertFalse(any(e['event'] == 'extension_download_open' for e in host.seen))
        # Chrome's show() answers at once, with nothing.
        self.assertTrue(self.in_worker(host, ids['downloader'], f'chrome.downloads.show({download}) === undefined'))
        shown = first(lambda e: e['event'] == 'extension_download_open')
        self.assertEqual((shown.get('tab', 1), shown['id'], shown['reveal']), (1, download, True))
        host.send(op='extension_click', id=ids['downloader'])
        host.settle(0.5)
        self.assertEqual(worker(f'chrome.downloads.open({download})'), {'value': None})
        opened = first(lambda e: e['event'] == 'extension_download_open' and not e['reveal'])
        self.assertEqual((opened.get('tab', 1), opened['id']), (1, download))

        # A download the person refuses is one the extension hears was
        # cancelled; one it cancels itself stops.
        started = worker(f"chrome.downloads.download({{url: '{self.url}/download/refused'}})")
        refused = first(lambda e: e['event'] == 'download_prompt')
        self.assertEqual(started, {'value': refused['id']})
        self.assertEqual(refused['name'], 'served.bin')
        host.send(op='download_decision', id=refused['id'], allow=False)
        first(lambda e: e['event'] == 'download' and e['id'] == refused['id'] and e['state'] == 'cancelled')
        item = worker(f"chrome.downloads.search({{id: {refused['id']}}})")['value']
        self.assertEqual([(i['state'], i['error']) for i in item], [('interrupted', 'USER_CANCELED')])
        self.assertEqual(worker(f'chrome.downloads.open({refused["id"]})'), {'error': 'Download must be complete'})

        started = worker(f"chrome.downloads.download({{url: '{self.url}/download/cancelled'}})")
        cancelled = first(lambda e: e['event'] == 'download_prompt')
        host.send(op='download_decision', id=cancelled['id'], allow=True)
        first(lambda e: e['event'] == 'download' and e['id'] == cancelled['id'] and e['received'] > 0)
        self.assertEqual(worker(f"chrome.downloads.cancel({cancelled['id']})"), {'value': None})
        first(lambda e: e['event'] == 'download' and e['id'] == cancelled['id'] and e['state'] == 'cancelled')

        # Its own data, which needs no server.
        data = 'data:text/csv;base64,' + base64.b64encode(b'name,count\nkept,1\n').decode()
        started = worker(f"chrome.downloads.download({{url: '{data}', filename: 'export.csv'}})")
        exported = first(lambda e: e['event'] == 'download_prompt')
        self.assertEqual(started, {'value': exported['id']})
        self.assertEqual(exported['name'], 'export.csv')
        host.send(op='download_decision', id=exported['id'], allow=True)
        saved = first(lambda e: e['event'] == 'download' and e['id'] == exported['id'] and e['state'] != 'progress')
        self.assertEqual(saved['state'], 'complete', saved)
        self.assertEqual(Path(saved['path']).read_bytes(), b'name,count\nkept,1\n')

        # Two requests for one address at once are two downloads, each with
        # its own id and the name it asked for.
        twice = f'{self.url}/download/twice'
        pair = self.in_worker(host, ids['downloader'], 'outcome(Promise.all(['
                              f"chrome.downloads.download({{url: '{twice}', filename: 'first.bin'}}),"
                              f"chrome.downloads.download({{url: '{twice}', filename: 'second.bin'}})]))")
        prompts = [first(lambda e: e['event'] == 'download_prompt') for _ in range(2)]
        self.assertEqual(len(set(pair['value'])), 2, pair)
        self.assertEqual(sorted(pair['value']), sorted(p['id'] for p in prompts))
        named = {p['id']: p['name'] for p in prompts}
        self.assertEqual([named[i] for i in pair['value']], ['first.bin', 'second.bin'])
        for p in prompts:
            host.send(op='download_decision', id=p['id'], allow=False)

        # A file one of its pages made, from the page's own address for it.
        self.panel(host, 2, f'chrome-extension://{ids["downloader"]}/export.html')
        exported = first(lambda e: e['event'] == 'download_prompt')
        self.assertEqual(self.title(host, 2, 'exported:'), f'exported:{exported["id"]}')
        self.assertEqual(exported['name'], 'settings.csv')
        host.send(op='download_decision', id=exported['id'], allow=True)
        saved = first(lambda e: e['event'] == 'download' and e['id'] == exported['id'] and e['state'] != 'progress')
        self.assertEqual(saved['state'], 'complete', saved)
        self.assertEqual(Path(saved['path']).read_bytes(), b'setting,on\n')

        # Its page in a frame of a web page opens a download on the person's
        # click in that frame, as in Chrome, not on one elsewhere on the page,
        # which the host counts for the page's tab.
        host.navigate(f'{self.url}/download-framed/{ids["downloader"]}')
        self.worker_value(host, ids['downloader'], 'outcome(chrome.runtime.sendMessage({ping: true}))',
                          lambda value: value == {'value': 'ready'})
        in_frame = lambda: self.in_worker(host, ids['downloader'], f'outcome(chrome.runtime.sendMessage({{open: {download}}}))')
        self.click(host, 1, 600, 400)
        host.settle(0.5)
        self.assertEqual(in_frame(), {'value': 'User gesture required'})
        self.click(host, 1, 50, 50)
        host.settle(0.5)
        self.assertEqual(in_frame(), {'value': 'opened'})
        opened = first(lambda e: e['event'] == 'extension_download_open' and not e['reveal'])
        self.assertEqual((opened.get('tab', 1), opened['id']), (1, download))

    def test_an_extension_signs_in_on_a_page_shown_over_the_window(self):
        host, ids = self.start('signer', access={'signer': (['identity'], [])})
        signer = ids['signer']
        host.navigate(self.url + '/front')
        host.settle(1.0)
        back = f'https://{signer}.chromiumapp.org/'
        read = lambda expression: self.in_worker(host, signer, expression)
        self.assertEqual(read("chrome.identity.getRedirectURL('cb')"), back + 'cb')
        self.assertEqual(read('chrome.identity.getRedirectURL()'), back)
        self.assertEqual(read("chrome.identity.getAuthToken({interactive: true}).then(() => 'a token', (e) => e.message)"),
                         'The user is not signed in.')

        def outcomes(count):
            """The worker's first `count` outcomes, once it has them."""
            deadline = time.monotonic() + HANG_BOUND
            while True:
                seen = json.loads(read('JSON.stringify(signIns)'))
                if len(seen) >= count:
                    return seen[:count]
                self.assertGreater(deadline - time.monotonic(), 0, f'no outcome in {seen}')
                time.sleep(0.2)

        def page(how, code):
            query = urllib.parse.urlencode({'redirect_uri': back + 'cb', 'code': code, how: '1'})
            return f'{self.url}/authorize?{query}'

        def sign_in(url, tab, interactive=True):
            """Start a sign-in on `url` and show its page in the panel `tab`,
            or load it there out of sight, as the Browser does when asked."""
            mark = len(host.seen)
            self.assertEqual(read(f'signIn({{url: {json.dumps(url)}, interactive: {json.dumps(interactive)}}})'), 'started')
            asked = next((e for e in host.seen[mark:] if e['event'] == 'extension_sign_in'), None) \
                or host.until(lambda e: e['event'] == 'extension_sign_in')
            self.assertEqual((asked.get('tab'), asked['id'], asked['url'], asked['interactive']),
                             (1, signer, url, interactive))
            host.send(op='create_tab', tab=tab)
            host.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == tab)
            host.send(op='extension_sign_in_panel', tab=tab, id=signer, flow=asked['flow'])
            if interactive:
                host.send(op='resize', tab=tab, width=800, height=600, scale_factor=1.0)
            host.send(op='navigate', tab=tab, url=url)
            return mark

        def closed(tab, since):
            return next((e for e in host.seen[since:] if e['event'] == 'closed' and e.get('tab') == tab), None) \
                or host.until(lambda e: e['event'] == 'closed' and e.get('tab') == tab)

        # A site that signs the person in at once redirects to the
        # extension's address: the call ends with it, and the panel closes
        # without the page ever going there.
        mark = sign_in(page('auto', 'first'), 2)
        closed(2, mark)
        self.assertEqual(outcomes(1)[0], {'url': back + 'cb?code=first'})
        # Its script sends the person back once they are done.
        mark = sign_in(page('script', 'second'), 3)
        closed(3, mark)
        self.assertEqual(outcomes(2)[1], {'url': back + 'cb?code=second'})

        # Out of sight, for one that may not ask the person: a site that
        # sends them straight back signs them in; one whose page loads and
        # waits for them needs them, and its tab closes.
        mark = sign_in(page('auto', 'quiet'), 4, interactive=False)
        closed(4, mark)
        self.assertEqual(outcomes(3)[2], {'url': back + 'cb?code=quiet'})
        mark = sign_in(page('wait', 'needed'), 5, interactive=False)
        closed(5, mark)
        self.assertEqual(outcomes(4)[3], {'error': 'User interaction required.'})
        self.assertFalse([e for e in host.seen if e['event'] == 'uri' and e.get('url', '').startswith(back)],
                         'the extension\'s address never loads')

        # The person closes the panel instead, after longer than one request
        # for the outcome is held: the sign-in is turned down.
        mark = sign_in(page('wait', 'third'), 6)
        host.until(lambda e: e['event'] == 'title' and e.get('tab') == 6 and e['title'] == 'Sign in')
        host.settle(21.0)
        self.assertEqual(len(json.loads(read('JSON.stringify(signIns)'))), 4, 'still going on')
        host.send(op='close_tab', tab=6)
        self.assertEqual(outcomes(5)[4], {'error': 'The user did not approve access.'})

    def test_an_offscreen_document_works_for_its_worker_and_hears_the_page(self):
        host, ids = self.start('offscreener', access={'offscreener': (['offscreen', 'activeTab', 'scripting', 'tabs'], [])})
        page = self.url + '/parsed'
        host.navigate(page)
        host.settle(1.0)
        host.send(op='extension_click', id=ids['offscreener'])
        seen = json.loads(self.title(host, 1, 'offscreen:')[len('offscreen:'):])
        # The document has Chrome's one API there, runtime, and the worker
        # answers for the tabs: the page in front, asked of by no tab.
        front = {'url': page, 'from': '/offscreen.html', 'tabless': True}
        self.assertEqual(seen, {
            'before': False, 'after': True, 'contexts': ['/offscreen.html'],
            'second': 'Only a single offscreen document may be created.',
            'parsed': {'title': 'page /parsed', 'apis': ['runtime'], 'front': front},
            'direct': {'title': 'from the page', 'apis': ['runtime'], 'front': front},
            'closed': False})

    def side_panel_event(self, host, mark, mode):
        """The first `extension_side_panel` event of `mode` after `mark`."""
        match = lambda e: e['event'] == 'extension_side_panel' and e['mode'] == mode
        seen = next((e for e in host.seen[mark:] if match(e)), None)
        return seen or host.until(match)

    def worker_saw(self, host, extension, entry):
        """Wait until the fixture's worker noted `entry` in its `seen`."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            seen = json.loads(self.in_worker(host, extension, 'JSON.stringify(globalThis.seen || [])') or '[]')
            if entry in seen:
                return seen
            self.assertGreater(deadline - time.monotonic(), 0, f'the worker never saw {entry}: {seen}')
            host.settle(0.2)

    def test_a_side_panel_opens_on_a_click_beside_the_page_and_follows_its_options(self):
        host, ids = self.start('paneler', access={'paneler': (['sidePanel', 'tabs'], [])})
        page = self.url + '/notes'
        host.navigate(page)
        host.settle(1.0)
        paneler = ids['paneler']
        own = lambda path: f'chrome-extension://{paneler}/{path}'
        # Asked outside a gesture, it is refused, as in Chrome.
        self.worker_saw(host, paneler, 'refused:`sidePanel.open()` may only be called in response to a user gesture.')
        tab = self.in_worker(host, paneler, 'chrome.tabs.query({}).then((tabs) => tabs[0].id)')

        # It asked for its panel to open on a click: a click on its button
        # opens it beside the page, and the extension hears of no click.
        mark = len(host.seen)
        host.send(op='extension_click', id=paneler)
        opened = self.side_panel_event(host, mark, 'toggle')
        self.assertEqual((opened.get('tab'), opened['id'], opened['url']), (1, paneler, own('panel.html')))
        # Its page, in a tab of its own as the Browser shows it, is no tab
        # to the extension, and the page beside it is its window.
        self.panel(host, 2, own('panel.html'))
        shown = json.loads(self.title(host, 2, 'panel:')[len('panel:'):])
        self.assertEqual(shown, {'tab': True, 'window': page})

        # Its options for this tab name another page: where it shows, it
        # shows that one; turned off for the tab, it closes.
        mark = len(host.seen)
        self.in_worker(host, paneler, f'chrome.sidePanel.setOptions({{tabId: {tab}, path: "other.html"}}).then(() => true)')
        self.assertEqual(self.side_panel_event(host, mark, 'options')['url'], own('other.html'))
        mark = len(host.seen)
        self.in_worker(host, paneler, f'chrome.sidePanel.setOptions({{tabId: {tab}, enabled: false}}).then(() => true)')
        self.assertIsNone(self.side_panel_event(host, mark, 'options')['url'])
        self.assertEqual(self.in_worker(host, paneler, f'chrome.sidePanel.getOptions({{tabId: {tab}}}).then((o) => o.enabled)'),
                         False, 'Chromium keeps the options and answers for them')
        mark = len(host.seen)
        self.in_worker(host, paneler, f'chrome.sidePanel.close({{tabId: {tab}}}).then(() => true)')
        self.assertIsNone(self.side_panel_event(host, mark, 'close')['url'])
        # A tab with no side panel of its own has none to close.
        self.assertIn('No tab with id', self.in_worker(
            host, paneler, 'chrome.sidePanel.close({tabId: 987654}).then(() => "", (error) => error.message)'))

        # Without openPanelOnActionClick a click reaches the extension, as
        # without a side panel.
        self.in_worker(host, paneler, 'chrome.sidePanel.setPanelBehavior({openPanelOnActionClick: false}).then(() => true)')
        mark = len(host.seen)
        host.send(op='extension_click', id=paneler)
        seen = self.worker_saw(host, paneler, f'clicked:{tab}')
        self.assertEqual(seen.count(f'clicked:{tab}'), 1)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'extension_side_panel' and e['mode'] == 'toggle'])

    def test_a_shortcut_or_the_browser_opens_the_side_panel_the_tab_has(self):
        host, ids = self.start('paneler', access={'paneler': (['sidePanel', 'tabs'], [])})
        host.navigate(self.url + '/notes')
        host.settle(1.0)
        paneler = ids['paneler']
        tab = self.in_worker(host, paneler, 'chrome.tabs.query({}).then((tabs) => tabs[0].id)')
        self.in_worker(host, paneler, f'chrome.sidePanel.setOptions({{tabId: {tab}, path: "tab.html"}}).then(() => true)')

        # Its keyboard shortcut is the person's gesture: its listener opens
        # the panel the tab has.
        mark = len(host.seen)
        alt = {'ctrl': False, 'meta': False, 'alt': True, 'shift': True}
        host.send(op='key', key='P', down=True, **alt)
        host.send(op='key', key='P', down=False, **alt)
        opened = self.side_panel_event(host, mark, 'open')
        self.assertEqual((opened.get('tab'), opened['url']), (1, f'chrome-extension://{paneler}/tab.html'))
        self.worker_saw(host, paneler, 'opened')

        # The person opens it from the Browser: the same page, for this tab.
        mark = len(host.seen)
        host.send(op='extension_side_panel', id=paneler)
        opened = self.side_panel_event(host, mark, 'open')
        self.assertEqual((opened.get('tab'), opened['url']), (1, f'chrome-extension://{paneler}/tab.html'))


def walk(items):
    """Every row of a menu the host sent, its submenus' rows after it."""
    for item in items:
        yield item
        yield from walk(item.get('items', []))


def labelled(items):
    """A menu's commands, at any depth, by label."""
    return {item['label']: item['id'] for item in walk(items) if 'label' in item and 'items' not in item}


def png(width, height):
    """An opaque PNG of `width` by `height` pixels."""
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\0' + b'\x20\x80\x40\xff' * width for _ in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


if __name__ == '__main__':
    unittest.main(verbosity=2)
