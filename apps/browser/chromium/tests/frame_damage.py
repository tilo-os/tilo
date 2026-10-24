#!/usr/bin/env python3
"""A blinking caret leaves the host as the few pixels it changes.

A page whose only change is its caret blinking repaints a few hundred pixels
of a frame that, at 2560x1600, is sixteen megabytes. With `--tilo-frame-damage`
the host sends each paint after the first as a damage packet (kind 4): the
rects Chromium repainted and their pixels, which the app applies to the frame
it has. Without the switch every paint is the whole frame, as before.

Run as `frame_damage.py HOST`. It shows the same focused text field to a host
without the switch and to one with it, and fails unless the caret's blinks
arrive as small rects near the field, and the frame rebuilt from them is the
frame the host sends whole when asked (`resend_frame`). It prints the bytes
per blink each way.
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

HANG_BOUND = float(os.environ.get('TILO_TEST_HANG_BOUND_SECS', '120'))
WIDTH, HEIGHT, SCALE = 2560, 1600, 2.0
BLINK_SECS = 3.0
# The field, in CSS pixels; the caret blinks inside it.
FIELD = (40, 40, 400, 60)

CARET = ('''<!doctype html><style>html,body{margin:0;background:#ffffff}
textarea{position:absolute;left:%dpx;top:%dpx;width:%dpx;height:%dpx;border:0;outline:0;
resize:none;font:20px sans-serif;background:#ffffff}</style>
<textarea autofocus></textarea>
<script>document.querySelector('textarea').focus()</script>''' % FIELD).encode()

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('host_args', nargs='*')
args = parser.parse_args()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = CARET if self.path == '/caret' else b''
        self.send_response(200 if body else 404)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'


def blink(flags):
    """Shows the caret page to a host started with `flags`; returns the frame
    packets of tab 1 while the caret blinked as (kind, bytes, rects), and,
    once the page is still, the frame rebuilt from every packet and the frame
    the host sends whole when asked."""
    with tempfile.TemporaryDirectory(prefix='tilo-frame-damage-') as profile:
        log = tempfile.TemporaryFile()
        host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, *flags,
                                 *args.host_args],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                bufsize=0)
        events = queue.Queue()
        packets = []
        frame = {'pixels': None, 'size': None}
        resent = queue.Queue()
        lock = threading.Lock()

        def reader():
            stream = host.stdout
            buffer = bytearray(64 * 1024 * 1024)
            view = memoryview(buffer)

            def exact(length):
                got = 0
                while got < length:
                    n = stream.readinto(view[got:length])
                    if not n:
                        raise EOFError('host closed IPC')
                    got += n
                return view[:length]

            try:
                while True:
                    length, = struct.unpack('<I', exact(4))
                    assert 1 <= length <= 64 * 1024 * 1024
                    data = exact(length)
                    kind = data[0]
                    if kind == 1:
                        events.put(json.loads(bytes(data[1:])))
                        continue
                    if kind == 2:
                        width, height = struct.unpack_from('<II', data, 1)
                        assert len(data) == 9 + width * height * 4, 'frame size'
                        with lock:
                            if resent_wanted.is_set():
                                # The frame rebuilt so far, and the frame whole.
                                resent.put((bytes(frame['pixels']), bytes(data[9:])))
                            frame['pixels'] = bytearray(data[9:])
                            frame['size'] = (width, height)
                            packets.append((2, length + 4, None))
                        continue
                    assert kind == 4, kind
                    tab, width, height, count = struct.unpack_from('<IIII', data, 1)
                    assert tab == 1 and 1 <= count <= 16, (tab, count)
                    rects = [struct.unpack_from('<IIII', data, 17 + 16 * i) for i in range(count)]
                    at = 17 + 16 * count
                    with lock:
                        assert frame['size'] == (width, height), 'damage before its frame'
                        pixels = frame['pixels']
                        for x, y, w, h in rects:
                            assert w and h and x + w <= width and y + h <= height, (x, y, w, h)
                            for row in range(y, y + h):
                                start = (row * width + x) * 4
                                pixels[start:start + w * 4] = data[at:at + w * 4]
                                at += w * 4
                        assert at == len(data), 'damage pixels match their rects'
                        packets.append((4, length + 4, rects))
            except Exception as exc:  # noqa: BLE001 - handed to the main thread
                events.put(exc)

        resent_wanted = threading.Event()
        threading.Thread(target=reader, daemon=True).start()

        def send(**command):
            packet = b'\x01' + json.dumps(command).encode()
            host.stdin.write(struct.pack('<I', len(packet)) + packet)
            host.stdin.flush()

        def until(predicate, what):
            deadline = time.monotonic() + HANG_BOUND
            while True:
                try:
                    event = events.get(timeout=max(.05, deadline - time.monotonic()))
                except queue.Empty:
                    raise AssertionError(f'no {what}') from None
                if isinstance(event, Exception):
                    raise event
                if predicate(event):
                    return event

        try:
            until(lambda e: e['event'] == 'ready', 'ready')
            send(op='resize', width=WIDTH, height=HEIGHT, scale_factor=SCALE)
            send(op='navigate', url=base + '/caret')
            until(lambda e: e['event'] == 'loaded' and e.get('url', '').endswith('/caret'),
                  'the caret page loaded')
            # The caret blinks only in a focused view.
            send(op='focus', focused=True)
            time.sleep(1.0)
            with lock:
                settled = len(packets)
            time.sleep(BLINK_SECS)
            with lock:
                blinks = packets[settled:]
            # Without focus the caret stops; once nothing paints, the frame
            # rebuilt from every packet must be the frame the host has.
            send(op='focus', focused=False)
            time.sleep(1.0)
            resent_wanted.set()
            send(op='resend_frame', tab=1)
            try:
                rebuilt, whole = resent.get(timeout=HANG_BOUND)
            except queue.Empty:
                raise AssertionError('the host never sent the frame whole') from None
            send(op='shutdown')
            assert host.wait(timeout=HANG_BOUND) == 0
            return blinks, rebuilt, whole
        except BaseException:
            log.seek(0)
            print(log.read().decode(errors='replace')[-4000:])
            raise
        finally:
            if host.poll() is None:
                host.kill()
                host.wait()


try:
    whole_frames, _, _ = blink([])
    assert len(whole_frames) >= 3, f'the caret blinked {len(whole_frames)} times in {BLINK_SECS} s'
    assert all(kind == 2 for kind, _, _ in whole_frames), 'without the switch frames go whole'
    before = sum(size for _, size, _ in whole_frames) / len(whole_frames)

    damaged, rebuilt, resent = blink(['--tilo-frame-damage'])
    assert len(damaged) >= 3, f'the caret blinked {len(damaged)} times in {BLINK_SECS} s'
    kinds = [kind for kind, _, _ in damaged]
    assert all(kind == 4 for kind in kinds), f'blinks went whole: kinds {kinds}'
    left, top, width, height = (round(v * SCALE) for v in FIELD)
    for _, size, rects in damaged:
        assert size < WIDTH * HEIGHT * 4 // 100, f'a blink took {size} bytes'
        for x, y, w, h in rects:
            assert left <= x and x + w <= left + width and top <= y and y + h <= top + height, (
                f'a blink repainted {(x, y, w, h)}, outside the field {(left, top, width, height)}')
    assert rebuilt == resent, 'the frame rebuilt from rects differs from the frame whole'
    after_bytes = sum(size for _, size, _ in damaged) / len(damaged)
    print(f'perf chromium frame damage caret blink at {WIDTH}x{HEIGHT}: '
          f'{before:.0f} bytes per blink whole, {after_bytes:.0f} bytes as rects '
          f'({len(whole_frames)} and {len(damaged)} blinks)', flush=True)
finally:
    server.shutdown()
print('frame damage: ok')
