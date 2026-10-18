#!/usr/bin/env python3
"""A real host keeps an embedded site from storing anything on another site.

Serves a page on 127.0.0.1 that embeds a frame from localhost, a different
site. The page itself sets a cookie; the embedded frame asks whether it has
storage access and tries to set a cookie of its own. The page puts both
answers in its title, which the host reports: the page keeps its cookie, the
embedded site has no storage access and its cookie is refused.
"""
import argparse
import http.server
import json
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
args = parser.parse_args()

TOP = '''<!doctype html><title>waiting</title><body>
<script>
document.cookie = 'first=1; SameSite=Lax';
const first = document.cookie.includes('first=1');
addEventListener('message', (event) => {
  document.title = 'first=' + first + ' ' + event.data;
});
</script>
<iframe src="http://localhost:{port}/frame"></iframe>
</body>'''

FRAME = '''<!doctype html><body><script>
document.cookie = 'third=1; SameSite=None; Secure';
const third = document.cookie.includes('third=1');
document.hasStorageAccess().then(
  (access) => parent.postMessage('third=' + third + ' access=' + access, '*'),
  () => parent.postMessage('third=' + third + ' access=error', '*'));
</script></body>'''


class Pages(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 - http.server's name
        body = (FRAME if self.path == '/frame'
                else TOP.replace('{port}', str(self.server.server_port))).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Pages)
threading.Thread(target=server.serve_forever, daemon=True).start()
URL = f'http://127.0.0.1:{server.server_port}/top'

with tempfile.TemporaryDirectory(prefix='tilo-cef-privacy-') as profile:
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
                incoming.put(json.loads(data[1:]) if data[0] == 1 else {'event': 'frame'})
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    def next_event(deadline):
        event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
        if isinstance(event, Exception):
            raise event
        return event

    try:
        deadline = time.monotonic() + 90
        answer = None
        while time.monotonic() < deadline and answer is None:
            try:
                event = next_event(deadline)
            except queue.Empty:
                break
            name = event['event']
            if name == 'frame':
                continue
            print(json.dumps(event)[:300], flush=True)
            if name == 'ready':
                send({'op': 'resize', 'width': 800, 'height': 400, 'scale_factor': 1.0})
                send({'op': 'navigate', 'url': URL})
            elif name == 'title' and event.get('title', '').startswith('first='):
                answer = event['title']
        assert answer is not None, 'the page never reported its answers'
        assert 'first=true' in answer, f'the page keeps its own cookie: {answer}'
        assert 'access=false' in answer, f'the embedded site has no storage access: {answer}'
        assert 'third=false' in answer, f'the embedded site cannot set a cookie: {answer}'
        send({'op': 'shutdown'})
        while next_event(time.monotonic() + 10)['event'] != 'closed':
            pass
        assert host.wait(timeout=10) == 0
        print(f'PASS: {answer}')
    finally:
        server.shutdown()
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
