#!/usr/bin/env python3
"""A page's popup window stays its own, as in any browser.

- A sign-in button that opens a sized window (`window.open` with a width and
  a height) opens a popup of the page, with the size it asked for, once the
  app takes the page's popups.
- The popup answers the page that opened it (`window.opener.postMessage`),
  which a separate tab could not, and closes itself when done.
- A popup that closes itself as it loads, before the host has set it up,
  takes nothing else with it.
- A popup left open closes with the tab that opened it.
- The app closing a popup (its panel's close button) leaves the page open.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import queue
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

OPENER = b'''<!doctype html><title>ready</title>
<body style="margin:0"><button id="go" style="position:absolute;left:0;top:0;width:300px;height:100px"
 onclick="window.open('/signin','signin','width=420,height=520')">Sign in</button>
<script>
addEventListener('message', event => { document.title = 'opener:' + event.data; });
</script></body>'''
SIGN_IN = b'''<!doctype html><title>signin</title><body>
<script>
window.opener.postMessage('signed-in:' + location.pathname, '*');
if (location.search === '?now') window.close();
else if (location.search !== '?stay') setTimeout(() => window.close(), 200);
</script></body>'''
NOW = b'''<!doctype html><title>ready</title>
<body style="margin:0"><button style="position:absolute;left:0;top:0;width:300px;height:100px"
 onclick="window.open('/signin?now','signin','width=300,height=300')">Sign in</button></body>'''
STAY = b'''<!doctype html><title>ready</title>
<body style="margin:0"><button style="position:absolute;left:0;top:0;width:300px;height:100px"
 onclick="window.open('/signin?stay','signin','width=300,height=300')">Sign in</button></body>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split('?')[0]
        body = {'/': OPENER, '/signin': SIGN_IN, '/stay': STAY, '/now': NOW}.get(path, b'')
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        if os.environ.get('POPUP_DEBUG'): print(a, flush=True)


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
BASE = f'http://127.0.0.1:{server.server_port}'

with tempfile.TemporaryDirectory(prefix='tilo-cef-page-popups-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile],
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
                else:
                    incoming.put({'event': 'frame'})
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(**command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    seen = []

    def ended(what, error):
        """The reader's `error` while waiting for `what`, with what the host
        said before it and how it ended."""
        try:
            code = host.wait(timeout=5)
        except subprocess.TimeoutExpired:
            code = 'nothing yet'
        return AssertionError(f'{what}: {error!r}, the host exited with {code}; saw {seen[-20:]}')

    def until(what, predicate):
        """The first event `predicate` accepts, every one before it kept."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            try:
                event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{what}: never came; saw {seen[-20:]}') from None
            if isinstance(event, Exception):
                raise ended(what, event)
            if event['event'] != 'frame':
                seen.append(event)
            if predicate(event):
                return event

    def on(tab, name, **fields):
        return lambda e: (e['event'] == name and e.get('tab', 1) == tab
                          and all(e.get(k) == v for k, v in fields.items()))

    def click(tab):
        for down in (True, False):
            send(op='mouse_button', tab=tab, button='left', down=down, x=100.0, y=50.0)

    def open_popup(tab, what):
        """Click the page's sign-in button until its popup opens: a click
        before the page first paints lands nowhere."""
        deadline = time.monotonic() + HANG_BOUND
        while time.monotonic() < deadline:
            click(tab)
            try:
                event = incoming.get(timeout=2)
            except queue.Empty:
                continue
            while True:
                if isinstance(event, Exception):
                    raise ended(what, event)
                if event['event'] != 'frame':
                    seen.append(event)
                if on(tab, 'popup')(event):
                    return event
                try:
                    event = incoming.get(timeout=2)
                except queue.Empty:
                    break
        raise AssertionError(f'{what}: never came; saw {seen[-20:]}')

    try:
        ready = until('ready', lambda e: e['event'] == 'ready')
        assert ready.get('popups') is True, ready
        send(op='popups', tab=1, enabled=True)
        send(op='resize', width=800, height=400, scale_factor=1.0)
        send(op='navigate', url=BASE + '/')
        until('the opener', on(1, 'title', title='ready'))

        # A click opens the sign-in as the page's popup, at its size.
        popup = open_popup(1, 'a popup')
        assert popup['url'] == BASE + '/signin', popup
        assert (popup['width'], popup['height']) == (420, 520), popup
        first = popup['popup']
        assert first >= 1 << 30, popup
        # It answers the page that opened it, then closes itself.
        until('the popup answering its opener', on(1, 'title', title='opener:signed-in:/signin'))
        until('the popup closing itself', on(first, 'closed'))

        # One that closes itself as it loads is gone, and the page with it
        # goes on: its close cut the host's setting up of it short.
        send(op='navigate', url=BASE + '/now')
        until('the opener of a popup closing at once', on(1, 'title', title='ready'))
        hasty = open_popup(1, 'a popup closing at once')['popup']
        until('the popup closing at once', on(hasty, 'closed'))

        # A popup left open closes with the tab that opened it.
        send(op='create_tab', tab=2)
        until('a second tab', on(2, 'tab_ready'))
        send(op='visibility', tab=2, visible=True)
        send(op='popups', tab=2, enabled=True)
        send(op='resize', tab=2, width=800, height=400, scale_factor=1.0)
        send(op='navigate', tab=2, url=BASE + '/stay')
        until('the second opener', on(2, 'title', title='ready'))
        second = open_popup(2, 'a popup that stays')['popup']
        until('the popup loading', on(second, 'title', title='signin'))
        send(op='close_tab', tab=2)
        closing = {2, second}
        while closing:
            closing.discard(until('the opener and its popup closing',
                                  lambda e: e['event'] == 'closed' and e.get('tab') in closing)['tab'])

        # The app closes a popup the moment it opens; its page goes on.
        send(op='navigate', url=BASE + '/stay')
        until('the opener again', on(1, 'title', title='ready'))
        third = open_popup(1, 'a popup to close')['popup']
        send(op='close_tab', tab=third)
        until('the popup the app closed', on(third, 'closed'))
        send(op='navigate', url=BASE + '/')
        until('the page going on', on(1, 'title', title='ready'))

        send(op='shutdown')
        until('closed', lambda e: e['event'] == 'closed' and e.get('tab', 1) == 1)
        assert host.wait(timeout=HANG_BOUND) == 0
        print('PASS: sized popups stay the page\'s, answer it, and close with it')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        server.shutdown()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
