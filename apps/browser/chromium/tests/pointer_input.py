#!/usr/bin/env python3
"""The pointer on a real page behaves as it does in any browser.

- Over a link it is a hand, over text an I-beam: the host says the shape.
- A double click selects the word under it.
- Leaving an element, out of the page, ends its hover.
- A trackpad's movements of less than a pixel add up to a scroll.
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

PAGE = ('<!doctype html><html><body style="margin:0;font:20px sans-serif">'
        '<a id="link" href="#x" style="position:absolute;left:0;top:0;width:200px;height:50px;'
        'display:block">link</a>'
        '<p id="text" style="position:absolute;left:0;top:100px;margin:0">alpha bravo charlie</p>'
        '<div id="box" style="position:absolute;left:300px;top:0;width:200px;height:100px"></div>'
        '<div style="height:5000px"></div>'
        '<script>'
        'const say=s=>{document.title=s};'
        'document.getElementById("text").addEventListener("dblclick",'
        'e=>say("dblclick:"+e.detail+":"+getSelection().toString().trim()));'
        'document.getElementById("box").addEventListener("mouseenter",()=>say("entered"));'
        'document.getElementById("box").addEventListener("mouseleave",()=>say("left"));'
        'addEventListener("scroll",()=>say("scroll:"+Math.round(scrollY)));'
        'say("ready");'
        '</script></body></html>')
URL = 'data:text/html;base64,' + base64.b64encode(PAGE.encode()).decode()


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


with tempfile.TemporaryDirectory(prefix='tilo-cef-pointer-input-') as profile:
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

    seen = []

    def until(what, predicate):
        """The first event `predicate` accepts, every one before it kept."""
        deadline = time.monotonic() + HANG_BOUND
        while True:
            try:
                event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{what}: never came; saw {seen[-20:]}') from None
            if isinstance(event, Exception):
                raise event
            if event['event'] != 'frame':
                seen.append(event)
            if predicate(event):
                return event

    def title(expected):
        return until(f'title {expected!r}',
                     lambda e: e['event'] == 'title' and e['title'] == expected)

    def move(x, y):
        send({'op': 'mouse_move', 'x': float(x), 'y': float(y)})

    def click(x, y):
        send({'op': 'mouse_button', 'button': 'left', 'down': True, 'x': float(x), 'y': float(y)})
        send({'op': 'mouse_button', 'button': 'left', 'down': False, 'x': float(x), 'y': float(y)})

    try:
        until('ready', lambda e: e['event'] == 'ready')
        send({'op': 'resize', 'width': 800, 'height': 400, 'scale_factor': 1.0})
        send({'op': 'navigate', 'url': URL})
        title('ready')
        until('a painted page', lambda e: e['event'] == 'frame')

        # The shape follows the page under the pointer.
        move(100, 25)
        until('a hand over the link', lambda e: e['event'] == 'cursor' and e['cursor'] == 'pointer')
        move(30, 112)
        until('an I-beam over the text', lambda e: e['event'] == 'cursor' and e['cursor'] == 'text')

        # Two clicks in quick succession on a word are a double click on it.
        click(30, 112)
        click(30, 112)
        title('dblclick:2:alpha')

        # Into the box, then out of the page above it: the hover ends.
        move(400, 50)
        title('entered')
        move(400, -5)
        title('left')

        # Twenty movements of half a pixel are ten pixels of scroll.
        move(200, 300)
        for _ in range(20):
            send({'op': 'scroll', 'x': 200.0, 'y': 300.0, 'dx': 0.0, 'dy': -0.5})
        scrolled = until('a scroll from half pixels',
                         lambda e: e['event'] == 'title' and e['title'].startswith('scroll:'))
        assert int(scrolled['title'].split(':')[1]) > 0, scrolled

        send({'op': 'shutdown'})
        until('closed', lambda e: e['event'] == 'closed')
        assert host.wait(timeout=HANG_BOUND) == 0
        print('PASS: pointer shapes, double click, hover leaving the page, fractional scroll')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics[-20000:])
