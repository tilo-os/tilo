#!/usr/bin/env python3
"""A real host reports a page's document size, only once asked, and its scroll.

Loads a local page 3000 CSS pixels tall into a 800x200 view. Nothing is reported
before the tab asks; once it asks, the host sends the document's height, and a
scroll to an anchor far down moves the reported scroll position.
"""
import argparse
import base64
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

    try:
        deadline = time.monotonic() + 60
        asked = scrolled = False
        sizes = []
        frames = 0
        scrolled_at = None
        asked_again = False
        while time.monotonic() < deadline:
            wait = deadline if scrolled_at is None or asked_again else scrolled_at + 5
            try:
                event = next_event(wait)
            except queue.Empty:
                if scrolled_at is not None and not asked_again:
                    # No paint since the scroll (none at all in a headless
                    # sandbox without a compositor): asking again reads now.
                    print(f'no report after the scroll; frames so far: {frames}', flush=True)
                    send({'op': 'document_size', 'tab': 1, 'enabled': True})
                    asked_again = True
                    continue
                raise
            name = event['event']
            if name == 'frame':
                frames += 1
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
                if len(sizes) == 1:
                    assert event['height'] >= 3000, event
                    assert 700 <= event['width'] <= 900, event
                    assert event['scroll_y'] == 0, event
                    # The page scrolls to an anchor far down it.
                    send({'op': 'navigate', 'url': URL + '#far'})
                    scrolled_at = time.monotonic()
                elif event['scroll_y'] > 0:
                    scrolled = True
                    break
        assert sizes, 'no document size reported'
        assert scrolled, f'a scroll was not reported: {sizes}'
        send({'op': 'shutdown'})
        while next_event(time.monotonic() + 10)['event'] != 'closed':
            pass
        assert host.wait(timeout=10) == 0
        print(f'PASS: off until asked, height of the document, scroll reported ({frames} frames)')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
