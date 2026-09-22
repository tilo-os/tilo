#!/usr/bin/env python3
"""Exercise a real host, HTTPS navigation, BGRA frames and orderly shutdown."""
import argparse
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('--url', default='https://podesta.ai')
args = parser.parse_args()

def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)

with tempfile.TemporaryDirectory(prefix='qos-cef-smoke-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
    incoming = queue.Queue()
    def reader():
        try:
            while True:
                length, = struct.unpack('<I', read_exact(host.stdout, 4))
                assert 1 <= length <= 64 * 1024 * 1024
                data = read_exact(host.stdout, length)
                if data[0] == 1:
                    incoming.put(json.loads(data[1:]))
                elif data[0] == 2:
                    width, height = struct.unpack_from('<II', data, 1)
                    assert 0 < width <= 8192 and 0 < height <= 8192
                    assert len(data) == 9 + width * height * 4
                    incoming.put({'event': 'frame', 'width': width, 'height': height, 'nonempty': len(set(data[9:])) > 2})
                else:
                    raise AssertionError('unexpected packet kind')
        except Exception as exc:
            incoming.put(exc)
    threading.Thread(target=reader, daemon=True).start()
    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()
    ready = loaded = painted = False
    events = []
    try:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            if isinstance(event, Exception):
                raise event
            if event['event'] != 'frame':
                events.append(event)
                print(json.dumps(event), flush=True)
            if event['event'] == 'ready':
                ready = True
                send({'op': 'resize', 'width': 1024, 'height': 768, 'scale_factor': 1.0})
                send({'op': 'navigate', 'url': args.url})
            elif event['event'] == 'loaded' and event['url'].startswith(args.url):
                assert event['status'] == 200, event
                loaded = True
            elif event['event'] == 'frame' and loaded and event['nonempty']:
                painted = True
            if ready and loaded and painted:
                break
        assert ready and loaded and painted, (ready, loaded, painted)
        send({'op': 'shutdown'})
        while True:
            event = incoming.get(timeout=10)
            if isinstance(event, Exception):
                raise event
            if event['event'] == 'closed':
                break
        assert host.wait(timeout=10) == 0
        print('PASS: ready, HTTPS 200, nonempty BGRA frame, closed, exit 0')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics:
            print(diagnostics)
