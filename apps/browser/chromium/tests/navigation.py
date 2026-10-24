#!/usr/bin/env python3
"""Back, forward and leaving a page go where a person expects.

- A tab's first page has nothing behind it. The engine opens every tab on a
  blank page before its first address; that page is no step back: the back
  button is off on the first page, going back from it does nothing, and the
  back list never offers it.
- A page that asks to be kept open (a `beforeunload` handler, armed by a click
  on the page) still lets the person leave it and reload it in a tab that does
  not answer page questions, as closing its tab does: nobody would see the
  question, so a refused navigation would leave the page stuck. A tab that
  answers them is asked (page_dialogs.py).
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

loads = {'/guard': 0}
GUARD = '''<!doctype html><title>guard-{n}</title>
<body style="margin:0"><div style="position:absolute;left:0;top:0;width:400px;height:300px"
 onclick="document.title='armed-{n}'"></div>
<script>
addEventListener('beforeunload', event => {{ event.preventDefault(); event.returnValue = 'unsaved'; }});
</script></body>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split('?')[0]
        if path == '/guard':
            loads['/guard'] += 1
            body = GUARD.format(n=loads['/guard']).encode()
        elif path in ('/a', '/b', '/c', '/after'):
            body = f'<!doctype html><title>{path[1:]}</title><body>{path}</body>'.encode()
        else:
            self.send_response(404)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        if os.environ.get('NAVIGATION_DEBUG'): print(a, flush=True)


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

with tempfile.TemporaryDirectory(prefix='tilo-cef-navigation-') as profile:
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
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(**command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    seen = []
    # The newest back and forward state each tab reported.
    history = {}

    def next_event(timeout):
        event = incoming.get(timeout=timeout)
        if isinstance(event, Exception):
            raise event
        seen.append(event)
        if event['event'] == 'history':
            history[event.get('tab', 1)] = (event['back'], event['forward'])
        return event

    def until(what, predicate, timeout=HANG_BOUND):
        """The first event `predicate` accepts, every one before it kept."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                event = next_event(max(.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{what}: never came; saw {seen[-20:]}') from None
            if predicate(event):
                return event

    def on(tab, name, **fields):
        return lambda e: (e['event'] == name and e.get('tab', 1) == tab
                          and all(e.get(k) == v for k, v in fields.items()))

    def settled(tab, url):
        """The tab has finished loading `url`: its history state is final."""
        until(f'{url} loading', on(tab, 'loaded', url=url))
        # The overall loading state settles after the main frame's load,
        # unless it was what finished the load: then it came just before.
        before = [e for e in seen[:-1] if e.get('tab') == tab]
        if not before or before[-1]['event'] != 'history':
            until(f'{url} settling', on(tab, 'history'))

    def arm(tab, n):
        """Click the guarded page until its handler counts as armed: a click
        before the page first paints lands nowhere."""
        deadline = time.monotonic() + HANG_BOUND
        while time.monotonic() < deadline:
            for down in (True, False):
                send(op='mouse_button', tab=tab, button='left', down=down, x=100.0, y=50.0)
            try:
                until('the click arming the page', on(tab, 'title', title=f'armed-{n}'), timeout=2)
                return
            except AssertionError:
                continue
        raise AssertionError(f'the page never took a click; saw {seen[-20:]}')

    try:
        until('ready', lambda e: e['event'] == 'ready')
        send(op='resize', width=800, height=400, scale_factor=1.0)
        send(op='visibility', visible=True)

        # A page that asks to be kept open still lets the person leave it.
        send(op='navigate', url=BASE + '/guard')
        settled(1, BASE + '/guard')
        arm(1, 1)
        send(op='navigate', url=BASE + '/after')
        settled(1, BASE + '/after')

        # And reload it.
        send(op='navigate', url=BASE + '/guard')
        settled(1, BASE + '/guard')
        until('the guarded page again', on(1, 'title', title='guard-2'))
        arm(1, 2)
        send(op='reload')
        until('the guarded page reloaded', on(1, 'title', title='guard-3'))

        for tab in (2, 3):
            send(op='create_tab', tab=tab)
            until('a fresh tab', on(tab, 'tab_ready'))
            send(op='navigate', tab=tab, url=BASE + '/a')
            settled(tab, BASE + '/a')
            assert history[tab] == (False, False), (tab, history[tab], seen[-20:])

            # Nothing behind the first page: going back stays on it, and the
            # back list is empty. The next navigation runs after the back.
            send(op='back', tab=tab)
            send(op='navigation_history', tab=tab, request_id=10 + tab, direction='back')
            entries = until('the back list', on(tab, 'navigation_history'))['entries']
            assert entries == [], (tab, entries)
            start = len(seen)
            send(op='navigate', tab=tab, url=BASE + '/b')
            settled(tab, BASE + '/b')
            blank = [e for e in seen[start:] if on(tab, 'uri', url='about:blank')(e)]
            assert not blank, (tab, blank)
            assert history[tab] == (True, False), (tab, history[tab])

            send(op='navigation_history', tab=tab, request_id=20 + tab, direction='back')
            entries = until('the back list', on(tab, 'navigation_history'))['entries']
            assert [e['url'] for e in entries] == [BASE + '/a'], (tab, entries)

            send(op='back', tab=tab)
            settled(tab, BASE + '/a')
            assert history[tab] == (False, True), (tab, history[tab])

        send(op='shutdown')
        until('closed', lambda e: e['event'] == 'closed' and e.get('tab', 1) == 1)
        assert host.wait(timeout=HANG_BOUND) == 0
        print('PASS: no step back to the blank page, and a guarded page can be left and reloaded')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        server.shutdown()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
