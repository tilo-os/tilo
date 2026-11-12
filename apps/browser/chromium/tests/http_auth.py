#!/usr/bin/env python3
"""A server's sign-in (HTTP authentication) reaches the person, and their
answer reaches the server that asked and nobody else.

- A tab that never asked for sign-ins has each challenge cancelled at once,
  by the host itself: its page shows the server's own 401 page from the one
  request it made, and nobody is asked. So has a tab whose embedder stopped
  answering. No challenge is ever left to Chromium, whose own prompt is a
  window on the desktop of whoever runs the host.
- A tab that asked gets each challenge as an `http_auth` event naming who
  asks (the origin the credentials go to), the server's realm and the scheme.
  The request waits until `http_auth_reply` answers it: the right credentials
  open the page, wrong ones are asked for again (`retry`), and an answer
  without credentials cancels, which shows the server's 401 page.
- An answer naming another challenge, or another asker than the one the
  challenge named, is no answer: nothing is sent anywhere.
- Something a page loads from another origin (an image, a frame) is never
  asked about: its challenge is cancelled, and that server sees no
  credentials. What the page loads from its own origin is asked about.
- Requests waiting on one question are one question, answered together.
- Digest is asked as Basic is, and the password never crosses in the clear.
- Going elsewhere, and closing the tab, let an open challenge go; an answer
  that comes after that sends nothing.
- No password is in any event the host sends or in anything it logs.
"""
import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import re
import struct
import subprocess
import tempfile
import threading
import time

# How long a wait on the host may take before it counts as hung: the
# shared hang bound (tools/tests/hang.py), so a loaded runner is not a failure.
HANG_BOUND = float(os.environ.get('TILO_TEST_HANG_BOUND_SECS', '120'))

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
args = parser.parse_args()

USER = 'ada'
PASSWORD = 'correct-horse-9'
WRONG = 'wrong-staple-7'
LEAK = 'never-sent-anywhere-5'
SECRETS = [PASSWORD, WRONG, LEAK]
SECRETS += [base64.b64encode(f'{USER}:{secret}'.encode()).decode() for secret in SECRETS]

# The pages behind a Basic challenge, by path: their realm. Each realm has a
# folder of its own: Chromium sends Basic credentials a server accepted along
# with every later request below the folder they were accepted in, unasked,
# so pages that share a folder would not each be asked about.
BASIC = {
    '/unasked/page': 'Unasked',
    '/staff/page': 'Staff room',
    '/cellar/page': 'Cellar',
    '/attic/page': 'Attic',
    '/vault/data.txt': 'Vault',
    '/twins/a.txt': 'Twins',
    '/twins/b.txt': 'Twins',
    '/loft/page': 'Loft',
    '/shed/page': 'Shed',
    '/barn/page': 'Barn',
    # On the other server: what a page of the first loads across origins.
    '/pixel.png': 'Elsewhere',
    '/frame': 'Elsewhere',
}
DIGEST_REALM = 'Archive'
NONCE = 'fixture-nonce-1'

EMBEDS = '''<!doctype html><title>embedding</title><body><script>
const sameOrigin = (image, frame) => fetch('/vault/data.txt')
  .then(reply => {{ document.title = 'embeds ' + image + ' ' + frame + ' ' + reply.status; }});
const frame = image => {{
  const element = document.createElement('iframe');
  element.onload = () => sameOrigin(image, 'framed');
  element.src = '{other}/frame';
  document.body.appendChild(element);
}};
const image = new Image();
image.onload = () => frame('loaded');
image.onerror = () => frame('blocked');
image.src = '{other}/pixel.png';
</script></body>'''
TWINS = '''<!doctype html><title>twinning</title><body><script>
Promise.all([fetch('/twins/a.txt'), fetch('/twins/b.txt')])
  .then(replies => { document.title = 'twins ' + replies.map(reply => reply.status).join(' '); });
</script></body>'''

# Every request each server got: its path and the Authorization it carried.
got = {}
servers = {}


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, status, body, headers=()):
        body = body.encode()
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def page(self, path):
        self.reply(200, f'<!doctype html><title>{path[1:]}</title><body>{path}</body>')

    def refuse(self, path, challenge):
        self.reply(401, f'<!doctype html><title>401 {path}</title><body>Sign in to see this page</body>',
                   [('WWW-Authenticate', challenge)])

    def digest_ok(self, sent):
        if not sent or not sent.startswith('Digest '):
            return False
        said = {key: quoted or bare
                for key, quoted, bare in re.findall(r'(\w+)=(?:"([^"]*)"|([^,\s]*))', sent[7:])}
        md5 = lambda text: hashlib.md5(text.encode()).hexdigest()
        first = md5(f'{USER}:{DIGEST_REALM}:{PASSWORD}')
        second = md5(f'GET:{said.get("uri", "")}')
        expected = md5(':'.join([first, said.get('nonce', ''), said.get('nc', ''),
                                 said.get('cnonce', ''), said.get('qop', ''), second]))
        return (said.get('username') == USER and said.get('nonce') == NONCE
                and said.get('response') == expected)

    def do_GET(self):
        path = self.path.split('?')[0]
        sent = self.headers.get('Authorization')
        got[self.server.server_port].append((path, sent))
        if path == '/archive/page':
            if self.digest_ok(sent):
                self.page(path)
            else:
                self.refuse(path, f'Digest realm="{DIGEST_REALM}", qop="auth", nonce="{NONCE}", algorithm=MD5')
        elif path in BASIC:
            expected = 'Basic ' + base64.b64encode(f'{USER}:{PASSWORD}'.encode()).decode()
            if sent == expected:
                self.page(path)
            else:
                self.refuse(path, f'Basic realm="{BASIC[path]}"')
        elif path == '/embeds':
            self.reply(200, EMBEDS.format(other=servers['other']))
        elif path == '/twins':
            self.reply(200, TWINS)
        elif path in ('/plain', '/elsewhere'):
            self.page(path)
        else:
            self.reply(404, '')

    def log_message(self, *a):
        if os.environ.get('HTTP_AUTH_DEBUG'): print(a, flush=True)


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


def serve(name):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    got[server.server_port] = []
    servers[name] = f'http://127.0.0.1:{server.server_port}'
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


site, other = serve('site'), serve('other')
BASE, OTHER = servers['site'], servers['other']


def sent_to(server, path):
    """The Authorization of every request `server` got for `path`, in order."""
    return [sent for asked, sent in got[server.server_port] if asked == path]


with tempfile.TemporaryDirectory(prefix='tilo-cef-http-auth-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile,
                             '--use-mock-keychain'],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
    incoming = queue.Queue()

    def reader():
        try:
            while True:
                length, = struct.unpack('<I', read_exact(host.stdout, 4))
                assert 1 <= length <= 64 * 1024 * 1024
                data = read_exact(host.stdout, length)
                if data[0] == 1:
                    incoming.put(json.loads(data[1:]))
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(**command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    seen = []
    # Whether the host said it holds sign-ins for its embedder.
    holds = False

    def next_event(timeout):
        event = incoming.get(timeout=timeout)
        if isinstance(event, Exception):
            raise event
        seen.append(event)
        return event

    def until(what, predicate, timeout=HANG_BOUND):
        """The first event `predicate` accepts, every one before it kept."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                event = next_event(max(.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{what}: never came; saw {seen[-12:]}') from None
            if predicate(event):
                return event

    def on(tab, name, **fields):
        return lambda e: (e['event'] == name and e.get('tab', 1) == tab
                          and all(e.get(k) == v for k, v in fields.items()))

    def asked(what, tab=1, **fields):
        """The next challenge the tab is asked about, which must be this one."""
        event = until(what, on(tab, 'http_auth'))
        assert event['id'] > 0, event
        for key, value in fields.items():
            assert event.get(key) == value, (what, key, event)
        return event

    def answer(event, password=None, tab=1, asker=None, id=None):
        if not holds:
            return
        reply = dict(op='http_auth_reply', tab=tab, id=id or event['id'],
                     asker=asker or event['asker'])
        if password is not None:
            reply.update(username=USER, password=password)
        send(**reply)

    def closed(event, tab=1):
        until(f'challenge {event["id"]} closing', on(tab, 'http_auth_closed', id=event['id']))

    def titled(title, tab=1):
        until(f'the page titled {title!r}', on(tab, 'title', title=title))

    def go(path, tab=1):
        send(op='navigate', tab=tab, url=BASE + path)

    def never_asked():
        """Nobody asked for sign-ins: the challenge is cancelled at once."""
        go('/unasked/page')
        titled('401 /unasked/page')
        assert not [e for e in seen if e['event'] == 'http_auth'], seen[-12:]
        # One request, cancelled by the host: a challenge left to Chromium
        # is asked about in a prompt of its own and requested again.
        assert sent_to(site, '/unasked/page') == [None], got

    def right_credentials():
        """The right credentials open the page; only an answer to this
        challenge, naming who asked, is one."""
        go('/staff/page')
        event = asked('the challenge', asker=BASE, realm='Staff room', scheme='basic',
                      proxy=False, retry=False)
        # An answer to another challenge, and one naming another asker than
        # the one this challenge named, send nothing.
        answer(event, LEAK, id=event['id'] + 1000)
        answer(event, LEAK, asker=OTHER)
        answer(event, LEAK, asker=BASE + '.evil.example')
        answer(event, PASSWORD)
        closed(event)
        titled('staff/page')
        expected = 'Basic ' + base64.b64encode(f'{USER}:{PASSWORD}'.encode()).decode()
        assert sent_to(site, '/staff/page') == [None, expected], got

    def wrong_then_right():
        """Wrong credentials are asked for again, and said to be a retry."""
        go('/cellar/page')
        first = asked('the challenge', asker=BASE, realm='Cellar', retry=False)
        answer(first, WRONG)
        closed(first)
        second = asked('the challenge again', asker=BASE, realm='Cellar', retry=True)
        assert second['id'] != first['id'], (first, second)
        answer(second, PASSWORD)
        closed(second)
        titled('cellar/page')
        assert [sent is not None for sent in sent_to(site, '/cellar/page')] == [False, True, True], got

    def cancel():
        """An answer without credentials cancels: the server's 401 page."""
        go('/attic/page')
        event = asked('the challenge', asker=BASE, realm='Attic')
        answer(event)
        closed(event)
        titled('401 /attic/page')
        assert sent_to(site, '/attic/page') == [None], got

    def cross_origin():
        """What a page loads from another origin is never asked about, and
        that server gets no credentials; what it loads from its own is."""
        go('/embeds')
        event = asked('the page\'s own request', asker=BASE, realm='Vault', scheme='basic')
        answer(event, PASSWORD)
        closed(event)
        titled('embeds blocked framed 200')
        assert not [e for e in seen if e['event'] == 'http_auth' and e['asker'] != BASE], seen
        assert sent_to(other, '/pixel.png') == [None], got
        assert sent_to(other, '/frame') == [None], got
        assert all(sent is None for _, sent in got[other.server_port]), got

    def twins():
        """Two requests waiting on one question are one question."""
        go('/twins')
        event = asked('the one question', asker=BASE, realm='Twins')
        answer(event, PASSWORD)
        closed(event)
        titled('twins 200 200')
        assert len([e for e in seen if e['event'] == 'http_auth' and e['realm'] == 'Twins']) == 1, seen

    def digest():
        """Digest is asked as Basic is; the password is never sent."""
        go('/archive/page')
        event = asked('the challenge', asker=BASE, realm=DIGEST_REALM, scheme='digest', retry=False)
        answer(event, PASSWORD)
        closed(event)
        titled('archive/page')
        sent = sent_to(site, '/archive/page')
        assert sent[0] is None and sent[-1].startswith('Digest '), got
        assert not [s for s in sent if s and any(secret in s for secret in SECRETS)], got

    def navigation_away():
        """Going elsewhere lets the challenge go; a late answer sends nothing."""
        go('/loft/page')
        event = asked('the challenge', asker=BASE, realm='Loft')
        go('/plain')
        closed(event)
        titled('plain')
        answer(event, PASSWORD)
        # Had the late answer been taken, the page would open unasked.
        go('/loft/page')
        again = asked('the challenge on coming back', asker=BASE, realm='Loft', retry=False)
        assert again['id'] != event['id'], (event, again)
        answer(again)
        closed(again)
        titled('401 /loft/page')
        assert all(sent is None for sent in sent_to(site, '/loft/page')), got

    def closing_tab():
        """A tab that closes lets its challenge go."""
        send(op='create_tab', tab=2)
        until('a second tab', on(2, 'tab_ready'))
        if holds:
            send(op='http_auth', tab=2, enabled=True)
        go('/shed/page', tab=2)
        event = asked('the second tab\'s challenge', tab=2, asker=BASE, realm='Shed')
        send(op='close_tab', tab=2)
        closed(event, tab=2)
        until('the second tab closed', on(2, 'closed'))
        answer(event, PASSWORD, tab=2)
        assert sent_to(site, '/shed/page') == [None], got

    def stopped_answering():
        """An embedder that stops answering has each challenge cancelled."""
        if holds:
            send(op='http_auth', enabled=False)
        before = len(seen)
        go('/barn/page')
        titled('401 /barn/page')
        assert not [e for e in seen[before:] if e['event'] == 'http_auth'], seen[before:]
        assert sent_to(site, '/barn/page') == [None], got

    failed = []
    try:
        ready = until('ready', lambda e: e['event'] == 'ready')
        send(op='resize', width=800, height=400, scale_factor=1.0)
        send(op='visibility', visible=True)

        def run(scenario):
            try:
                scenario()
                print(f'ok: {scenario.__name__}', flush=True)
            except AssertionError as exc:
                failed.append(scenario.__name__)
                print(f'FAILED: {scenario.__name__}: {exc}', flush=True)

        run(never_asked)
        # A host is told only what it said it takes: one that does not hold
        # sign-ins would end on the command, and every scenario with it.
        holds = ready.get('http_auth') is True
        if holds:
            send(op='http_auth', enabled=True)
        else:
            failed.append('advertised')
            print(f'FAILED: advertised: the host does not say it holds sign-ins: {ready}',
                  flush=True)
        for scenario in (right_credentials, wrong_then_right, cancel, cross_origin, twins,
                         digest, navigation_away, closing_tab, stopped_answering):
            run(scenario)

        send(op='shutdown')
        until('closed', lambda e: e['event'] == 'closed' and e.get('tab', 1) == 1)
        assert host.wait(timeout=HANG_BOUND) == 0
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        site.shutdown()
        other.shutdown()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
    # No password is in anything the host said or logged.
    said = json.dumps(seen)
    for secret in SECRETS:
        if secret in said or secret in diagnostics:
            failed.append('secrets')
            print('FAILED: secrets: a password is in what the host said or logged', flush=True)
            break
    if failed:
        raise SystemExit(f'FAILED: {", ".join(failed)}')
    print('PASS: a server asks the person to sign in, and only the one who asked is answered')
