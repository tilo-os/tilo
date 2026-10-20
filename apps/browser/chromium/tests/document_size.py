#!/usr/bin/env python3
"""A real host reports a page's document size, only once asked, and its scroll.

Loads a local page 3000 CSS pixels tall into a 800x200 view. Nothing is reported
before the tab asks; once it asks, the host sends the document's height, and a
scroll to an anchor far down moves the reported scroll position, and so does
the wheel once the page is painted there.
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

# How long a wait on the host may take before it counts as hung: the
# shared hang bound (tools/tests/hang.py), so a loaded runner is not a failure.
HANG_BOUND = float(os.environ.get('TILO_TEST_HANG_BOUND_SECS', '120'))

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
args = parser.parse_args()

PAGE = ('<!doctype html><html><body style="margin:0">'
        '<div style="height:2000px;background:linear-gradient(#fff,#048)">tall</div>'
        '<div id="far" style="height:1000px">far</div>'
        '</body></html>')
URL = 'data:text/html;base64,' + base64.b64encode(PAGE.encode()).decode()


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


with tempfile.TemporaryDirectory(prefix='tilo-cef-document-size-') as profile:
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

    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    def next_event(deadline):
        event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
        if isinstance(event, Exception):
            raise event
        return event

    def wheel():
        """Wheel up three notches over the page, as the pointer does."""
        send({'op': 'mouse_move', 'x': 400.0, 'y': 100.0})
        for _ in range(3):
            send({'op': 'scroll', 'x': 400.0, 'y': 100.0, 'dx': 0.0, 'dy': 120.0})

    try:
        deadline = time.monotonic() + HANG_BOUND
        asked = False
        sizes = []
        frames = 0
        phase = 'load'
        frames_before_anchor = 0
        while time.monotonic() < deadline:
            try:
                event = next_event(deadline)
            except queue.Empty:
                raise AssertionError(f'{phase}: nothing reported; {frames} frames, {sizes}') from None
            name = event['event']
            if name == 'frame':
                frames += 1
                if phase == 'painted':
                    # The scrolled page is on screen, so the renderer can
                    # tell what is under the pointer: a wheel event that
                    # comes before the page's first frame finds nothing to
                    # scroll and is dropped, in any Chromium.
                    phase = 'wheel'
                    wheel()
                continue
            print(json.dumps(event)[:300], flush=True)
            if name == 'ready':
                assert event.get('document_size') is True, 'the host says it measures'
                send({'op': 'resize', 'width': 800, 'height': 200, 'scale_factor': 1.0})
                send({'op': 'navigate', 'url': URL})
            elif name == 'loaded' and event['url'] == URL and not asked:
                # Loaded, and nothing reported: it is off until asked.
                assert not sizes, sizes
                send({'op': 'document_size', 'tab': 1, 'enabled': True})
                asked = True
            elif name == 'document_size':
                assert asked, 'reported before it was asked for'
                sizes.append(event)
                if phase == 'load':
                    assert event['height'] >= 3000, event
                    assert 700 <= event['width'] <= 900, event
                    assert event['scroll_y'] == 0, event
                    # The page scrolls to an anchor far down it.
                    phase = 'anchor'
                    frames_before_anchor = frames
                    send({'op': 'navigate', 'url': URL + '#far'})
                elif phase == 'anchor' and event['scroll_y'] >= 1900:
                    # Then, once it is painted there, the wheel scrolls it
                    # back up. The report follows the paint that showed it.
                    if frames > frames_before_anchor:
                        phase = 'wheel'
                        wheel()
                    else:
                        phase = 'painted'
                elif phase == 'wheel' and event['scroll_y'] < sizes[-2]['scroll_y']:
                    phase = 'done'
                    break
        assert phase == 'done', f'{phase}: {sizes}'
        send({'op': 'shutdown'})
        while next_event(time.monotonic() + HANG_BOUND)['event'] != 'closed':
            pass
        assert host.wait(timeout=HANG_BOUND) == 0
        print(f'PASS: off until asked, height, anchor and wheel scroll reported ({frames} frames)')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
