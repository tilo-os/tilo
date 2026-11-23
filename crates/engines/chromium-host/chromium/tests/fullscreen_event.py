#!/usr/bin/env python3
"""A page's requestFullscreen() (on a user gesture) reaches the app as a
`fullscreen` event with fullscreen=true, and exit_fullscreen yields false.

Runs a real host in off-screen rendering mode against a data: page whose
button enters element fullscreen.
"""
import argparse
import json
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time
import urllib.parse

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('--seconds', type=int, default=60)
args = parser.parse_args()

PAGE = """<html><body style="margin:0"><div id=d style="width:100vw;height:100vh;background:#c00">
<button style="width:100%;height:100%" id=b>fs</button></div><script>
b.onclick=()=>{d.requestFullscreen().then(()=>document.title='fs-ok',e=>document.title='fs-err:'+e)};
</script></body></html>"""
URL = 'data:text/html,' + urllib.parse.quote(PAGE)

def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)

with tempfile.TemporaryDirectory(prefix='tilo-cef-fs-') as profile, tempfile.TemporaryFile() as log:
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, '--ozone-platform=headless', '--disable-gpu'],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
    incoming = queue.Queue()
    def reader():
        try:
            while True:
                length, = struct.unpack('<I', read_exact(host.stdout, 4))
                data = read_exact(host.stdout, length)
                if data[0] == 1:
                    incoming.put(json.loads(data[1:]))
        except Exception as exc:
            incoming.put(exc)
    threading.Thread(target=reader, daemon=True).start()
    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()
    clicked = exited = False
    try:
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            if isinstance(event, Exception):
                raise event
            name = event['event']
            if name not in ('frame', 'audio'):
                print('event', name, {k: v for k, v in event.items() if k != 'event'}, flush=True)
            if name == 'ready':
                send({'op': 'resize', 'width': 400, 'height': 300, 'scale_factor': 1.0})
                send({'op': 'navigate', 'url': URL})
            elif name == 'loaded' and event['url'].startswith('data:') and not clicked:
                clicked = True
                send({'op': 'mouse_move', 'x': 50, 'y': 50})
                send({'op': 'mouse_button', 'x': 50, 'y': 50, 'button': 'left', 'down': True})
                send({'op': 'mouse_button', 'x': 50, 'y': 50, 'button': 'left', 'down': False})
            elif name == 'fullscreen' and event['fullscreen'] and not exited:
                exited = True
                send({'op': 'exit_fullscreen'})
            elif name == 'fullscreen' and not event['fullscreen'] and exited:
                print('PASS: fullscreen entered and exited')
                break
        else:
            raise AssertionError('no fullscreen event (entered=%s)' % exited)
    finally:
        host.kill()
        host.wait()
        log.seek(0)
        print(log.read().decode(errors='replace')[-1500:])
