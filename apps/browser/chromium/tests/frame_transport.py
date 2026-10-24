#!/usr/bin/env python3
"""A page's frames on their way out of the engine: what they cost the host, and
that no tab's frame is lost to another tab's.

Every paint of a shown page leaves the host as one packet of BGRA pixels, at
2560x1600 sixteen megabytes. The host copies the pixels once, into the packet
it sends and keeps (a popup is composited onto a copy), and the writer keeps
each tab's newest unsent packet, so a page that paints every frame cannot
replace another tab's last frame before it is sent: that tab would show a
stale page until it next painted, which a still page never does.

Run as `frame_transport.py HOST`. It prints what the host's own process spent
per second and per delivered frame while one page animated, for the first tab
and for a routed one, and fails when a still tab's change never arrived or the
host spent more than BUDGET_MS of its own CPU per delivered frame.
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
MEASURE_SECS = 3.0
# The host's own CPU per delivered frame (its process alone, not the
# renderer's): Chromium's share of a paint plus the host's copy and write.
# The x86 builder measured 7.2 ms for the first tab and 7.3 ms for a routed
# one; copying each paint three or four times had cost 16.9 and 20.1 ms there,
# and the routed tab no longer kept up with the page's 60 frames a second.
BUDGET_MS = float(os.environ.get('TILO_FRAME_TRANSPORT_BUDGET_MS', '12'))

ANIMATED = b'''<!doctype html><style>html,body{margin:0;height:100%}</style><script>
let n=0;function tick(){document.body.style.background=n++%2?'#204080':'#802040';requestAnimationFrame(tick)}
requestAnimationFrame(tick)</script>'''
STILL = b'<!doctype html><style>html,body{margin:0;height:100%;background:#ffffff}</style>'
# One colour at a time, long enough apart that only a lost frame misses one.
COLORS = ('#d02020', '#20d020', '#2020d0', '#d0d020', '#20d0d0', '#d020d0', '#808080', '#000000')
CHANGING = ('''<!doctype html><style>html,body{margin:0;height:100%}</style><script>
const colors=COLORS;let i=0;document.documentElement.style.background=colors[0];
function next(){if(++i<colors.length){document.documentElement.style.background=colors[i];setTimeout(next,400)}
else setTimeout(()=>{document.title='done'},400)}
setTimeout(next,400)</script>'''.replace('COLORS', json.dumps(list(COLORS)))).encode()

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('host_args', nargs='*')
args = parser.parse_args()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = {'/animated': ANIMATED, '/still': STILL, '/changing': CHANGING}.get(self.path, b'')
        self.send_response(200 if body else 404)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def cpu_seconds(pid):
    """User and system CPU the process has spent, in seconds."""
    try:
        with open(f'/proc/{pid}/stat') as stat:
            fields = stat.read().rsplit(')', 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')
    except FileNotFoundError:
        # macOS, for local runs: [[dd-]hh:]mm:ss.ss
        text = subprocess.run(['ps', '-o', 'time=', '-p', str(pid)], capture_output=True,
                              text=True, check=True).stdout.strip()
        days, _, clock = text.rpartition('-')
        seconds = 0.0
        for part in clock.split(':'):
            seconds = seconds * 60 + float(part)
        return seconds + (int(days) * 86400 if days else 0)


def rgb(color):
    return tuple(int(color[i:i + 2], 16) for i in (1, 3, 5))


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}'

with tempfile.TemporaryDirectory(prefix='tilo-frame-transport-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, *args.host_args],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
    events = queue.Queue()
    frames = {}  # tab -> [count, latest centre (r, g, b), centres in order of change]
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
                assert kind in (2, 3), kind
                tab = struct.unpack_from('<I', data, 1)[0] if kind == 3 else 1
                offset = 5 if kind == 3 else 1
                width, height = struct.unpack_from('<II', data, offset)
                assert len(data) == offset + 8 + width * height * 4, 'frame size'
                at = offset + 8 + ((height // 2) * width + width // 2) * 4
                centre = (data[at + 2], data[at + 1], data[at])
                with lock:
                    seen = frames.setdefault(tab, [0, None, []])
                    seen[0] += 1
                    seen[1] = centre
                    if not seen[2] or seen[2][-1] != centre:
                        seen[2].append(centre)
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            events.put(exc)

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

    def navigate(tab, path):
        send(op='navigate', tab=tab, url=base + path)
        until(lambda e: e['event'] == 'loaded' and e.get('tab') == tab
              and e.get('url', '').endswith(path), f'tab {tab} loaded {path}')

    def count(tab):
        with lock:
            return frames.get(tab, [0])[0]

    def measure(tab, label):
        time.sleep(.5)
        before_frames, before_cpu, started = count(tab), cpu_seconds(host.pid), time.monotonic()
        time.sleep(MEASURE_SECS)
        delivered = count(tab) - before_frames
        elapsed = time.monotonic() - started
        spent = cpu_seconds(host.pid) - before_cpu
        assert delivered > 0, f'{label}: no frame arrived while the page animated'
        per_frame = spent * 1000 / delivered
        print(f'perf chromium frame transport {label}: {delivered / elapsed:.1f} frames/s at '
              f'{WIDTH}x{HEIGHT}, host process {spent * 1000 / elapsed:.0f} ms CPU/s, '
              f'{per_frame:.2f} ms per frame (budget {BUDGET_MS} ms)', flush=True)
        return per_frame

    try:
        until(lambda e: e['event'] == 'ready', 'ready')
        send(op='resize', width=WIDTH, height=HEIGHT, scale_factor=SCALE)
        send(op='create_tab', tab=2)
        until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == 2, 'tab 2 ready')
        send(op='resize', tab=2, width=WIDTH, height=HEIGHT, scale_factor=SCALE)
        navigate(2, '/still')
        navigate(1, '/animated')
        first = measure(1, 'first tab')

        # A still tab changes eight times beside a page painting every frame:
        # each change must arrive.
        navigate(2, '/changing')
        until(lambda e: e['event'] == 'title' and e.get('tab') == 2 and e.get('title') == 'done',
              'the changing tab to finish')
        time.sleep(.5)
        with lock:
            changes = list(frames[2][2])
        wanted = [rgb(color) for color in COLORS]
        arrived = [color for color in wanted if color in changes]
        assert arrived == wanted, (f'a still tab lost frames to a busy one: of {wanted} only '
                                   f'{arrived} arrived (centres seen {changes})')
        assert changes[-1] == wanted[-1], f'the still tab ends on {changes[-1]}, not {wanted[-1]}'

        # The same animation through a routed tab, with the first tab hidden.
        send(op='visibility', visible=False)
        navigate(2, '/animated')
        routed = measure(2, 'routed tab')
        for label, per_frame in (('first tab', first), ('routed tab', routed)):
            assert per_frame <= BUDGET_MS, (f'{label}: the host spent {per_frame:.2f} ms per '
                                            f'frame, over the {BUDGET_MS} ms budget')
        send(op='shutdown')
        assert host.wait(timeout=HANG_BOUND) == 0
    except BaseException:
        log.seek(0)
        print(log.read().decode(errors='replace')[-4000:])
        raise
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        server.shutdown()
print('frame transport: ok')
