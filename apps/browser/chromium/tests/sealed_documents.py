#!/usr/bin/env python3
"""A host started for sealed documents (a mail) never leaves the document.

The page carries a script, a meta refresh, a frame to other sites and a link.
The script never runs. Nothing navigates: the refresh and the frame go nowhere,
and the link the pointer follows comes back as an open_tab request for the
embedder, after the address under the pointer was reported. The selection is
read back as text for the embedder's clipboard.
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
args = parser.parse_args()

PAGE = ('<!doctype html><html><head><title>Sealed</title>'
        '<meta http-equiv="refresh" content="0; url=https://refresh.invalid/">'
        '<script>document.title = "script ran"</script>'
        '</head><body style="margin:0;font:16px sans-serif">'
        '<a href="https://open.invalid/story" style="display:block;height:120px">Open the story</a>'
        '<p id="text">Hello sealed world</p>'
        '<iframe src="https://frame.invalid/" width="10" height="10"></iframe>'
        '</body></html>')
URL = 'data:text/html;base64,' + base64.b64encode(PAGE.encode()).decode()
LINK = 'https://open.invalid/story'


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


with tempfile.TemporaryDirectory(prefix='tilo-cef-sealed-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, '--tilo-sealed-documents'],
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

    seen = []

    def until(predicate, timeout=HANG_BOUND, what='event'):
        deadline = time.monotonic() + timeout
        while True:
            try:
                event = incoming.get(timeout=max(.05, deadline - time.monotonic()))
            except queue.Empty:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}; saw {[e["event"] for e in seen][-40:]}') from None
            if isinstance(event, Exception):
                raise event
            if event['event'] != 'frame':
                seen.append(event)
                print(json.dumps(event)[:300], flush=True)
                # The document is never left.
                if event['event'] == 'uri':
                    assert event['url'].startswith('data:') or event['url'] == 'about:blank', event
                if event['event'] == 'open_tab':
                    assert event['url'] == LINK, event
            if predicate(event):
                return event
            if time.monotonic() >= deadline:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}')

    try:
        ready = until(lambda e: e['event'] == 'ready', what='ready')
        assert ready.get('sealed_documents') is True, ready
        assert ready.get('selection_text') is True, ready
        send({'op': 'resize', 'width': 800, 'height': 400, 'scale_factor': 1.0})
        send({'op': 'navigate', 'url': URL})
        # A frame of the document, which may come before it says it loaded:
        # the page is painted once, and nothing on it changes after.
        until(lambda e: e['event'] == 'uri' and e['url'] == URL, what='the document')
        painted = []

        def loaded(event):
            """Whether the document loaded; a frame on the way is kept."""
            if event['event'] == 'frame':
                painted.append(event)
            return event['event'] == 'loaded' and event['url'] == URL

        until(loaded, what='loaded')
        if not painted:
            until(lambda e: e['event'] == 'frame', what='a painted frame')
        # The refresh and the frame had their chance: nothing was opened.
        until(lambda e: False, timeout=2.0)
        assert not [e for e in seen if e['event'] == 'open_tab'], 'a refresh opened something'
        assert not [e for e in seen if e.get('title') == 'script ran'], 'a script ran'

        # The pointer over the link says where it goes.
        send({'op': 'mouse_move', 'x': 40.0, 'y': 40.0})
        status = until(lambda e: e['event'] == 'link_status' and e['url'], what='link status')
        assert status['url'] == LINK, status
        # Followed, it is the embedder's to open; the document stays.
        send({'op': 'mouse_button', 'button': 'left', 'down': True, 'x': 40.0, 'y': 40.0})
        send({'op': 'mouse_button', 'button': 'left', 'down': False, 'x': 40.0, 'y': 40.0})
        until(lambda e: e['event'] == 'open_tab', what='open_tab for the followed link')

        # Select the paragraph by dragging over it, and read it back.
        send({'op': 'focus', 'focused': True})
        send({'op': 'mouse_move', 'x': 2.0, 'y': 145.0})
        send({'op': 'mouse_button', 'button': 'left', 'down': True, 'x': 2.0, 'y': 145.0})
        send({'op': 'mouse_move', 'x': 600.0, 'y': 145.0})
        send({'op': 'mouse_button', 'button': 'left', 'down': False, 'x': 600.0, 'y': 145.0})
        text = ''
        for request in range(1, 40):
            send({'op': 'selection_text', 'request_id': request})
            reply = until(lambda e, r=request: e['event'] == 'selection_text' and e['request_id'] == r,
                          what='selection text')
            text = reply['text']
            if 'Hello sealed world' in text:
                break
            time.sleep(.1)
        assert 'Hello sealed world' in text, repr(text)
        assert LINK not in text

        send({'op': 'shutdown'})
        until(lambda e: e['event'] == 'closed', what='closed')
        assert host.wait(timeout=HANG_BOUND) == 0
        print('PASS: script, refresh and frame refused, link reported and handed out, selection read')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
