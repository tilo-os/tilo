#!/usr/bin/env python3
"""Measure native key dispatch to a causally marked complete Chromium CPU frame.

This excludes Rust/Vello and display presentation. It is not physical keyboard
latency or a comparison with a native Chromium window. No page script dispatches
input: the fixture only changes its marker after a native input event.
"""
import argparse
import hashlib
import json
import math
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Page(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'''<!doctype html><title>input-ready</title><style>
        body{margin:0}#marker{position:fixed;left:0;top:0;width:8px;height:8px;
        background:rgb(0,255,64)}input{margin:40px;width:500px;font-size:24px}
        </style><div id="marker"></div><input autofocus id="field"><script>
        field.addEventListener('input',()=>{
          marker.style.backgroundColor=`rgb(${field.value.length},255,64)`;
        });</script>'''
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def exact(stream, length):
    data = bytearray()
    while len(data) < length:
        chunk = stream.read(length - len(data))
        if not chunk:
            raise EOFError('Chromium host closed before the measurement completed')
        data.extend(chunk)
    return data


def percentile(samples, rank):
    if not samples or not all(math.isfinite(value) and value >= 0 for value in samples):
        raise ValueError('measurement has no valid latency samples')
    return sorted(samples)[max(0, math.ceil(len(samples) * rank / 100) - 1)]


def frame_marker(payload):
    if len(payload) < 13 or payload[0] != 2:
        raise ValueError('invalid primary frame packet')
    width, height = struct.unpack_from('<II', payload, 1)
    if not 1 <= width <= 8192 or not 1 <= height <= 8192 or len(payload) != 9 + width * height * 4:
        raise ValueError('invalid frame size')
    pixel = payload[9:13]
    marker = pixel[2] if pixel[0:2] == bytes([64, 255]) and pixel[3] == 255 else None
    return {'event': 'frame', 'size': (width, height), 'marker': marker}


def parse_memory_pressure(text):
    for line in text.splitlines():
        if line.startswith('full '):
            values = dict(field.split('=', 1) for field in line.split()[1:])
            value = float(values['avg10'])
            if math.isfinite(value) and 0 <= value <= 100:
                return value
    raise ValueError('cannot observe memory pressure')


def memory_pressure():
    return parse_memory_pressure(Path('/proc/pressure/memory').read_text()) if sys.platform == 'linux' else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host', type=Path)
    parser.add_argument('--width', type=int, default=1280)
    parser.add_argument('--height', type=int, default=800)
    parser.add_argument('--keys', type=int, default=80)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--max-load-per-cpu', type=float, default=1.0)
    args = parser.parse_args()
    if not 1 <= args.keys <= 200:
        parser.error('--keys must be between 1 and 200')
    if not 600 <= args.width <= 3840 or not 200 <= args.height <= 2160:
        parser.error('viewport must fit the input field and be at most 3840x2160')
    if args.output.exists():
        parser.error('output already exists; keep independent measurement results')
    if not math.isfinite(args.max_load_per_cpu) or args.max_load_per_cpu <= 0:
        parser.error('--max-load-per-cpu must be finite and positive')
    cpus = len(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else os.cpu_count()
    load_before = os.getloadavg()[0] / cpus
    if load_before > args.max_load_per_cpu:
        raise RuntimeError(f'host too busy for a valid measurement: load per CPU {load_before:.2f}')
    pressure_before = memory_pressure()
    if pressure_before is not None and pressure_before > 1:
        raise RuntimeError(f'memory pressure invalidates measurement: full avg10 {pressure_before:.2f}%')
    host_sha256 = hashlib.sha256(args.host.read_bytes()).hexdigest()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    incoming = queue.Queue()
    with tempfile.TemporaryDirectory(prefix='tilo-input-benchmark-') as profile, tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)

        def read():
            try:
                while True:
                    length, = struct.unpack('<I', exact(process.stdout, 4))
                    if not 1 <= length <= 64 * 1024 * 1024:
                        raise ValueError('invalid packet size')
                    payload = exact(process.stdout, length)
                    stamp = time.monotonic()
                    if payload[0] == 1:
                        incoming.put((stamp, json.loads(payload[1:])))
                    elif payload[0] == 2:
                        incoming.put((stamp, frame_marker(payload)))
                    else:
                        raise ValueError('unexpected packet kind in single-tab benchmark')
            except Exception as error:
                incoming.put((time.monotonic(), error))

        threading.Thread(target=read, daemon=True).start()

        def send(**command):
            payload = b'\x01' + json.dumps(command).encode()
            process.stdin.write(struct.pack('<I', len(payload)) + payload)
            process.stdin.flush()

        def until(predicate, timeout=20):
            deadline = time.monotonic() + timeout
            while True:
                stamp, event = incoming.get(timeout=max(0.001, deadline - time.monotonic()))
                if isinstance(event, Exception):
                    raise event
                if predicate(event):
                    return stamp
                if time.monotonic() >= deadline:
                    raise TimeoutError('native input did not produce its marked frame')

        try:
            until(lambda e: e['event'] == 'ready')
            send(op='resize', width=args.width, height=args.height, scale_factor=1)
            send(op='navigate', url=f'http://127.0.0.1:{server.server_port}/')
            until(lambda e: e['event'] == 'frame' and e['size'] == (args.width, args.height) and e['marker'] == 0)
            send(op='focus', focused=True)
            # Native pointer input establishes focus as a real click would.
            send(op='mouse_button', button='left', x=100, y=55, down=True)
            send(op='mouse_button', button='left', x=100, y=55, down=False)
            samples = []
            for count in range(1, args.keys + 1):
                start = time.monotonic()
                for down in (True, False):
                    send(op='key', key='x', down=down, ctrl=False, alt=False, shift=False, meta=False)
                stamp = until(lambda e: e['event'] == 'frame' and e['size'] == (args.width, args.height)
                              and e['marker'] == count, timeout=5)
                samples.append((stamp - start) * 1000)
            load_after = os.getloadavg()[0] / cpus
            if load_after > args.max_load_per_cpu:
                raise RuntimeError(f'host became too busy for a valid measurement: load per CPU {load_after:.2f}')
            pressure_after = memory_pressure()
            if pressure_after is not None and pressure_after > 1:
                raise RuntimeError(f'memory pressure invalidates measurement: full avg10 {pressure_after:.2f}%')
            result = {
                'memory_full_avg10_before': pressure_before,
                'memory_full_avg10_after': pressure_after,
                'host_sha256': host_sha256,
                'load_per_cpu_before': load_before,
                'load_per_cpu_after': load_after,
                'measurement': 'native-key-command-to-complete-CPU-frame',
                'viewport': [args.width, args.height],
                'keys': len(samples),
                'p50_ms': percentile(samples, 50),
                'p95_ms': percentile(samples, 95),
                'p99_ms': percentile(samples, 99),
                'max_ms': max(samples),
                'samples_ms': samples,
                'note': 'Sequential native keys, one acknowledged pixel marker per key; excludes Rust/Vello and display presentation.',
            }
            send(op='shutdown')
            process.wait(timeout=35)
            if process.returncode != 0:
                raise RuntimeError(f'host exited {process.returncode}')
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps({k: v for k, v in result.items() if k != 'samples_ms'}, indent=2))
        except Exception:
            log.seek(0)
            print(log.read().decode(errors='replace')[-4000:])
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            server.shutdown()


if __name__ == '__main__':
    main()
