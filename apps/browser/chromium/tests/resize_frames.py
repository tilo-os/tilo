#!/usr/bin/env python3
"""Every frame at a view's new size shows the page laid out at that size.

An embedder that sizes a view to its document (a mail reader) measures the
page in a sliver of a view, then grows the view to the document's height. A
frame published at the grown size before the page repainted at it would show
the page's background where its content belongs: a blank flash the embedder
cannot tell from the page. The page here has a coloured block well below the
sliver; every frame at the grown size must show it.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time

HANG_BOUND = float(os.environ.get('TILO_TEST_HANG_BOUND_SECS', '120'))

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('host_args', nargs='*')
args = parser.parse_args()

PAGE = ('<!doctype html><html><head><style>html,body{margin:0;background:#fff}'
        '#block{position:absolute;top:150px;left:0;width:100%;height:100px;background:#d02020}'
        '</style></head><body><p>Hello</p><div id="block"></div></body></html>')
SCALE = 2.0
WIDTH = 600
SLIVER = 4
GROWN = 400
LAST = 618
OPENS = 8


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


def block_shown(pixels, width, y):
    """Whether the row at device pixel `y` is the block's red, in the middle (BGRA)."""
    at = (y * width + width // 2) * 4
    b, g, r = pixels[at], pixels[at + 1], pixels[at + 2]
    return r > 160 and g < 90 and b < 90


with tempfile.TemporaryDirectory(prefix='tilo-cef-resize-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, *args.host_args],
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
                    offset = 5 if data[0] == 3 else 1
                    width, height = struct.unpack_from('<II', data, offset)
                    incoming.put({'event': 'frame', 'width': width, 'height': height,
                                  'pixels': data[offset + 8:]})
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    def until(predicate, timeout=HANG_BOUND, what='event'):
        deadline = time.monotonic() + timeout
        while True:
            try:
                event = incoming.get(timeout=max(.05, deadline - time.monotonic()))
            except queue.Empty:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}') from None
            if isinstance(event, Exception):
                raise event
            if predicate(event):
                return event
            if time.monotonic() >= deadline:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}')

    try:
        until(lambda e: e['event'] == 'ready', what='ready')
        send({'op': 'document_size', 'tab': 1, 'enabled': True})
        grown = (int(WIDTH * SCALE), int(GROWN * SCALE))
        shown = 0
        # The way a mail reader opens a message: the view still has the last
        # message's size when the page is sent, shrinks to a sliver to measure
        # it, and grows to the document's height the moment it has loaded. The
        # renderer is busiest then, so a paint before it drew at the new size
        # is likeliest; each open is one more chance to catch one.
        for opened in range(OPENS):
            send({'op': 'resize', 'width': grown[0], 'height': int(LAST * SCALE),
                  'scale_factor': SCALE})
            url = 'data:text/html;base64,' + base64.b64encode(
                (PAGE + f'<!-- open {opened} -->').encode()).decode()
            send({'op': 'navigate', 'url': url})
            send({'op': 'resize', 'width': grown[0], 'height': int(SLIVER * SCALE),
                  'scale_factor': SCALE})
            until(lambda e, url=url: e['event'] == 'loaded' and e.get('url') == url, what='loaded')
            send({'op': 'resize', 'width': grown[0], 'height': grown[1], 'scale_factor': SCALE})
            frames = []

            def at_grown_size(event, frames=frames):
                if event['event'] == 'frame' and (event['width'], event['height']) == grown:
                    frames.append(event)
                return False

            until(at_grown_size, timeout=1.5)
            assert frames, f'open {opened}: no frame at the grown size'
            middle = int(200 * SCALE)
            blank = [index for index, frame in enumerate(frames)
                     if not block_shown(frame['pixels'], frame['width'], middle)
                     or frame['pixels'][-1] == 0]
            assert not blank, (f'open {opened}: frames {blank} of {len(frames)} at the grown size '
                               'show no page there')
            shown += len(frames)
        send({'op': 'shutdown'})
        until(lambda e: e['event'] == 'closed', what='closed')
        assert host.wait(timeout=HANG_BOUND) == 0
        print(f'PASS: {OPENS} opens, {shown} frame(s) at the grown size, each showing the page')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics and host.returncode:
            print(diagnostics[-20000:])
