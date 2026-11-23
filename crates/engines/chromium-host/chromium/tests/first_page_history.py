#!/usr/bin/env python3
"""A tab's first page is the whole of its session history, from its first script on.

- The page a tab opens first sees `history.length` 1 in an inline script in
  its head, the earliest a page's script can run, and still sees 1 once it
  has loaded.
- Its own `history.back()` and `history.go(-1)` there go nowhere: the page
  finishes loading and its tab shows no other address.

Whether a first page saw more than itself once depended on how soon its first
script ran, so this opens many tabs, one after another and several at once,
and counts every open: a failing run says how often it happened. The opens
made one at a time also print what opening a tab costs, from `create_tab` to
`tab_ready`, to the page's first script and to the first frame showing the
page.
"""
import argparse
import collections
import http.server
import json
import os
from pathlib import Path
import queue
import statistics
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
parser.add_argument('--one-at-a-time', type=int, default=64,
                    help='tabs opened one after another, each timed')
parser.add_argument('--batches', type=int, default=24, help='rounds of tabs opened at once')
parser.add_argument('--at-once', type=int, default=8, help='tabs in each such round')
args = parser.parse_args()

WIDTH, HEIGHT = 320, 200
# The page's colour, as the frame's centre pixel has it (BGR).
PAGE_BGR = (0x80, 0x40, 0x20)
# The first statement of the first script reads the history. The title says
# what it saw at once, and again with what the loaded page sees.
PAGE = '''<!doctype html><html><head><script>
var first = history.length;
document.title = 'first-' + first;
GO_BACK
addEventListener('load', function () {
  setTimeout(function () { document.title = 'loaded-' + first + '-' + history.length; }, 0);
});
</script><style>html,body{margin:0;height:100%;background:#204080}</style></head><body></body></html>'''
STAYS = PAGE.replace('GO_BACK', '').encode()
GOES_BACK = PAGE.replace('GO_BACK', 'history.back(); history.go(-1);').encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = {'/stays': STAYS, '/goes-back': GOES_BACK}.get(self.path.split('?')[0])
        if body is None:
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
        if os.environ.get('FIRST_PAGE_DEBUG'): print(a, flush=True)


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

with tempfile.TemporaryDirectory(prefix='tilo-cef-first-page-') as profile:
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
                at = time.monotonic()
                if data[0] == 1:
                    event = json.loads(data[1:])
                    event.setdefault('tab', 1)
                    event['at'] = at
                    incoming.put(event)
                elif data[0] in (2, 3):
                    # A whole frame: the first tab's, or another tab's behind its id.
                    tab, offset = (1, 1) if data[0] == 2 else (struct.unpack_from('<I', data, 1)[0], 5)
                    width, height = struct.unpack_from('<II', data, offset)
                    centre = offset + 8 + ((height // 2) * width + width // 2) * 4
                    incoming.put({'event': 'frame', 'tab': tab, 'at': at,
                                  'centre': tuple(data[centre:centre + 3])})
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(**command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    seen = []

    class Open:
        """One tab's first page: what it saw and when."""

        def __init__(self, tab, url):
            self.tab, self.url = tab, url
            self.created = self.ready = self.script = self.painted = None
            self.there = False      # the tab shows the page's address
            self.first = None       # history.length in the first script
            self.loaded = None      # history.length once loaded
            self.left = None        # another address the tab went on to show

        def done(self, painted):
            """Nothing more to wait for: the page loaded (and painted, where
            that is timed), or its tab left it."""
            return self.left is not None or (self.loaded is not None
                                             and (self.painted is not None or not painted))

        def take(self, event):
            name = event['event']
            if name == 'tab_ready':
                self.ready = event['at']
            elif name == 'uri':
                if event['url'] == self.url:
                    self.there = True
                elif self.there:
                    self.left = event['url']
            elif name == 'title' and self.there:
                title = event['title']
                if title.startswith('first-') and self.first is None:
                    self.first, self.script = int(title[6:]), event['at']
                elif title.startswith('loaded-'):
                    self.loaded = int(title.split('-')[2])
            elif name == 'frame' and self.there and self.painted is None:
                if event['centre'] == PAGE_BGR:
                    self.painted = event['at']

    opens = {}
    # The tabs the host said are ready (its first) and closed.
    started, closed = set(), set()

    def pump(what, until):
        """Hand every event to its tab's open until `until()` holds."""
        deadline = time.monotonic() + HANG_BOUND
        while not until():
            try:
                event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{what}: never came; saw {seen[-30:]}') from None
            if isinstance(event, Exception):
                raise event
            if event['event'] != 'frame':
                seen.append(event)
            if event['event'] in ('ready', 'closed'):
                (started if event['event'] == 'ready' else closed).add(event['tab'])
            if event['tab'] in opens:
                opens[event['tab']].take(event)

    def close(tabs):
        for tab in tabs:
            send(op='close_tab', tab=tab)
        pump('the tabs closing', lambda: closed.issuperset(tabs))

    finished = []
    try:
        # The tab the host starts with opens its first page too.
        first_tab = opens[1] = Open(1, BASE + '/goes-back?1')
        pump('ready', lambda: 1 in started)
        send(op='resize', width=WIDTH, height=HEIGHT, scale_factor=1.0)
        send(op='visibility', visible=True)
        send(op='navigate', url=first_tab.url)
        pump('the first tab loading', lambda: first_tab.done(painted=False))
        finished.append(first_tab)
        # Out of sight from here on, so only the tab being timed paints.
        send(op='visibility', visible=False)

        # One at a time, each timed from the command that opens it.
        next_tab = 2
        for _ in range(args.one_at_a_time):
            tab, next_tab = next_tab, next_tab + 1
            one = opens[tab] = Open(tab, f'{BASE}/stays?{tab}')
            one.created = time.monotonic()
            send(op='create_tab', tab=tab)
            pump('a fresh tab', lambda: one.ready is not None)
            send(op='resize', tab=tab, width=WIDTH, height=HEIGHT, scale_factor=1.0)
            send(op='navigate', tab=tab, url=one.url)
            pump(f'tab {tab} loading and painting', lambda: one.done(painted=True))
            finished.append(one)
            close([tab])
            del opens[tab]

        # Several at once, each going back as its first script runs.
        for _ in range(args.batches):
            batch = []
            for _ in range(args.at_once):
                tab, next_tab = next_tab, next_tab + 1
                batch.append(Open(tab, f'{BASE}/goes-back?{tab}'))
                opens[tab] = batch[-1]
                send(op='create_tab', tab=tab)
                send(op='navigate', tab=tab, url=batch[-1].url)
            pump('a round of tabs loading', lambda: all(one.done(painted=False) for one in batch))
            finished.extend(batch)
            close([one.tab for one in batch])
            for one in batch:
                del opens[one.tab]

        timed = [one for one in finished if one.created is not None and one.painted is not None]
        if timed:
            def spread(mark):
                costs = sorted((getattr(one, mark) - one.created) * 1000 for one in timed)
                return statistics.median(costs), costs[(len(costs) * 9) // 10]
            ready, script, painted = spread('ready'), spread('script'), spread('painted')
            print(f'perf chromium tab open: tab_ready {ready[0]:.1f} ms, first script '
                  f'{script[0]:.1f} ms, first paint {painted[0]:.1f} ms after create_tab '
                  f'(median of {len(timed)} tabs opened one at a time; nine in ten within '
                  f'{ready[1]:.1f}, {script[1]:.1f} and {painted[1]:.1f} ms)', flush=True)

        lengths = collections.Counter(one.first for one in finished)
        more = [one for one in finished if one.first != 1 or one.loaded not in (1, None)]
        left = [one for one in finished if one.left is not None]
        unloaded = [one for one in finished if one.loaded is None and one.left is None]
        print(f'first page history: {len(more)} of {len(finished)} opens saw more than the '
              f'page itself, and {len(left)} tabs left their first page (history.length in '
              f'the first script: {dict(sorted(lengths.items(), key=str))})', flush=True)
        assert not more and not left and not unloaded, (
            f'{len(more)} of {len(finished)} first pages saw a session history that was not '
            f'the page alone, and {len(left)} left for another address; the first of them: '
            f'{[(one.tab, one.first, one.loaded, one.left) for one in (more + left + unloaded)[:8]]}')

        send(op='shutdown')
        pump('closed', lambda: 1 in closed)
        assert host.wait(timeout=HANG_BOUND) == 0
        print(f'PASS: {len(finished)} first pages each saw a session history of one, '
              'and none could go back')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        server.shutdown()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
