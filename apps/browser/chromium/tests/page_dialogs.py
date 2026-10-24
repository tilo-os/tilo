#!/usr/bin/env python3
"""A page's own questions reach the person, and their answers reach the page.

- `alert`, `confirm` and `prompt` arrive as `js_dialog` events with the page's
  origin, message and suggestion; the page waits until `js_dialog_reply`
  answers, and gets exactly that answer: OK or Cancel, and the prompt's text.
- A reply naming another question changes nothing.
- "Leave this page?" (`beforeunload`) asks too: staying keeps the page and
  fails the navigation that asked, leaving goes, and a reload asks as one.
- A navigation lets an open question go: the page no longer waits.
- A tab that never asked for questions has each declined at once, so no page
  waits on a question nobody sees, and its page can always be left.
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
ASK = '''<!doctype html><title>asking</title><body><script>
const sure = confirm('Delete the draft?');
alert('Draft deleted');
const name = prompt('Your name?', 'Ada');
const again = confirm('Delete another?');
document.title = 'answered ' + JSON.stringify([sure, name, again]);
</script></body>'''
DECLINED = '''<!doctype html><title>asking</title><body><script>
document.title = 'declined ' + JSON.stringify([confirm('Sure?'), prompt('Name?', 'Ada')]);
</script></body>'''
WAITING = '''<!doctype html><title>waiting</title><body><script>
confirm('Keep waiting?');
document.title = 'went on';
</script></body>'''
GUARD = '''<!doctype html><title>guard-{n}</title>
<body style="margin:0"><div style="position:absolute;left:0;top:0;width:400px;height:300px"
 onclick="document.title='armed-{n}-' + (window.clicks = (window.clicks || 0) + 1)"></div>
<script>
addEventListener('beforeunload', event => {{ event.preventDefault(); event.returnValue = 'unsaved'; }});
</script></body>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split('?')[0]
        if path == '/guard':
            loads['/guard'] += 1
            body = GUARD.format(n=loads['/guard']).encode()
        elif path == '/ask':
            body = ASK.encode()
        elif path == '/declined':
            body = DECLINED.encode()
        elif path == '/waiting':
            body = WAITING.encode()
        elif path in ('/after', '/elsewhere'):
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
        if os.environ.get('PAGE_DIALOGS_DEBUG'): print(a, flush=True)


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

with tempfile.TemporaryDirectory(prefix='tilo-cef-dialogs-') as profile:
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
                raise AssertionError(f'{what}: never came; saw {seen[-20:]}') from None
            if predicate(event):
                return event

    def on(tab, name, **fields):
        return lambda e: (e['event'] == name and e.get('tab', 1) == tab
                          and all(e.get(k) == v for k, v in fields.items()))

    def question(what, tab=1, **fields):
        event = until(what, on(tab, 'js_dialog', **fields))
        assert event['id'] > 0, event
        return event

    def answer(event, accept, text=None, tab=1):
        """Answer `event` and see the page stop waiting on it."""
        reply = dict(op='js_dialog_reply', tab=tab, id=event['id'], accept=accept)
        if text is not None:
            reply['text'] = text
        send(**reply)
        until(f'question {event["id"]} closing', on(tab, 'js_dialog_closed', id=event['id']))

    def arm(tab, n):
        """Click the guarded page until it takes the click: a click before
        the page first paints lands nowhere. Each click is a fresh gesture,
        which is what lets a page ask to be kept open."""
        deadline = time.monotonic() + HANG_BOUND
        clicked = lambda e: (on(tab, 'title')(e)
                             and e['title'].startswith(f'armed-{n}-'))
        while time.monotonic() < deadline:
            for down in (True, False):
                send(op='mouse_button', tab=tab, button='left', down=down, x=100.0, y=50.0)
            try:
                until('the click arming the page', clicked, timeout=2)
                return
            except AssertionError:
                continue
        raise AssertionError(f'the page never took a click; saw {seen[-20:]}')

    try:
        ready = until('ready', lambda e: e['event'] == 'ready')
        assert ready.get('js_dialogs') is True, ready
        send(op='resize', width=800, height=400, scale_factor=1.0)
        send(op='visibility', visible=True)

        # Nobody asked for questions: each is declined at once.
        send(op='navigate', url=BASE + '/declined')
        until('the declined questions', on(1, 'title', title='declined [false,null]'))
        assert not [e for e in seen if e['event'] == 'js_dialog'], seen

        send(op='js_dialogs', enabled=True)

        # Each question, in turn, with what the page asked.
        send(op='navigate', url=BASE + '/ask')
        sure = question('the confirm', kind='confirm', message='Delete the draft?',
                        origin=BASE, default_text='')
        # An answer to another question is no answer to this one.
        send(op='js_dialog_reply', id=sure['id'] + 1000, accept=False)
        answer(sure, True)
        told = question('the alert', kind='alert', message='Draft deleted')
        assert told['id'] != sure['id'], (sure, told)
        answer(told, True)
        name = question('the prompt', kind='prompt', message='Your name?',
                        default_text='Ada')
        answer(name, True, 'Grace Hopper')
        again = question('the second confirm', kind='confirm', message='Delete another?')
        answer(again, False)
        until('the page got the answers',
              on(1, 'title', title='answered [true,"Grace Hopper",false]'))

        # Leaving a guarded page asks; staying keeps it.
        send(op='navigate', url=BASE + '/guard')
        until('the guarded page', on(1, 'title', title='guard-1'))
        arm(1, 1)
        send(op='scoped_navigation', request_id=1, command='navigate', url=BASE + '/after')
        leave = question('leave this page?', kind='beforeunload', reload=False, origin=BASE)
        start = len(seen)
        answer(leave, False)
        stayed = until('the navigation that asked', on(1, 'navigation_outcome', request_id=1))
        assert stayed.get('error') == 'The page asked to stay', stayed
        assert not [e for e in seen[start:] if on(1, 'uri', url=BASE + '/after')(e)], seen[start:]

        # Asked again (after another click on the page), leaving goes.
        arm(1, 1)
        send(op='scoped_navigation', request_id=2, command='navigate', url=BASE + '/after')
        leave = question('leave this page? again', kind='beforeunload', reload=False)
        answer(leave, True)
        went = until('the navigation that asked again', on(1, 'navigation_outcome', request_id=2))
        assert 'error' not in went and went['url'] == BASE + '/after', went

        # A reload asks as one, and goes when answered.
        send(op='navigate', url=BASE + '/guard')
        until('the guarded page again', on(1, 'title', title='guard-2'))
        arm(1, 2)
        send(op='reload')
        reload = question('reload this page?', kind='beforeunload', reload=True)
        answer(reload, True)
        until('the guarded page reloaded', on(1, 'title', title='guard-3'))

        # Going elsewhere lets an open question go.
        send(op='navigate', url=BASE + '/waiting')
        waiting = question('the waiting confirm', kind='confirm', message='Keep waiting?')
        send(op='navigate', url=BASE + '/elsewhere')
        until('the question let go', on(1, 'js_dialog_closed', id=waiting['id']))
        until('the page elsewhere', on(1, 'title', title='elsewhere'))

        # Another tab asks of its own: its questions name it.
        send(op='create_tab', tab=2)
        until('a second tab', on(2, 'tab_ready'))
        send(op='js_dialogs', tab=2, enabled=True)
        send(op='navigate', tab=2, url=BASE + '/waiting')
        waiting = question('the second tab asking', tab=2, kind='confirm')
        answer(waiting, True, tab=2)
        until('the second tab went on', on(2, 'title', title='went on'))

        send(op='shutdown')
        until('closed', lambda e: e['event'] == 'closed' and e.get('tab', 1) == 1)
        assert host.wait(timeout=HANG_BOUND) == 0
        print('PASS: a page asks the person, waits for the answer and gets it')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        server.shutdown()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
