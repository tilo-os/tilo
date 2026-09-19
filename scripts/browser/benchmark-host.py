#!/usr/bin/env python3
"""Measure real helper startup and CPU-frame delivery against a local animation."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import queue
import statistics
import struct
import subprocess
import tempfile
import threading
import time


class Page(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'''<!doctype html><title>qOS frame benchmark</title><style>
        body{margin:0;background:#f5f5f5}.box{width:300px;height:300px;background:#1870fa;
        animation:move 1s linear infinite alternate}@keyframes move{to{transform:translateX(600px) rotate(180deg)}}
        </style><div class="box"></div>'''
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
        part = stream.read(length - len(data))
        if not part:
            raise EOFError('helper closed')
        data.extend(part)
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host', type=Path)
    parser.add_argument('--seconds', type=float, default=5)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    events = queue.Queue()
    with tempfile.TemporaryDirectory(prefix='qos-benchmark-') as profile, tempfile.TemporaryFile() as log:
        start = time.monotonic()
        child = subprocess.Popen([str(args.host.resolve()), '--profile='+profile], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
        def reader():
            try:
                while True:
                    length, = struct.unpack('<I', exact(child.stdout, 4))
                    if not 1 <= length <= 64*1024*1024:
                        raise ValueError('invalid packet size')
                    payload = exact(child.stdout, length)
                    if payload[0] == 1:
                        events.put(('event', time.monotonic(), json.loads(payload[1:])))
                    elif payload[0] == 2:
                        width, height = struct.unpack_from('<II', payload, 1)
                        if length != 9+width*height*4:
                            raise ValueError('invalid frame size')
                        events.put(('frame', time.monotonic(), (width, height, length)))
            except Exception as error:
                events.put(('error', time.monotonic(), str(error)))
        threading.Thread(target=reader, daemon=True).start()
        def send(value):
            payload = b'\x01'+json.dumps(value).encode()
            child.stdin.write(struct.pack('<I', len(payload))+payload)
            child.stdin.flush()
        ready = loaded = None
        frames = []
        try:
            deadline = start+30
            while True:
                kind, stamp, data = events.get(timeout=max(.1, deadline-time.monotonic()))
                if kind == 'error':
                    raise RuntimeError(data)
                if kind == 'event' and data.get('event') == 'ready':
                    ready = stamp
                    send({'op':'resize','width':1280,'height':800,'scale_factor':1})
                    send({'op':'navigate','url':f'http://127.0.0.1:{server.server_port}/'})
                if kind == 'event' and data.get('event') == 'loaded' and data.get('status') == 200:
                    loaded = stamp
                    deadline = stamp+args.seconds+10
                if kind == 'frame' and loaded is not None and data[:2] == (1280,800):
                    frames.append((stamp, data[2]))
                    if stamp-loaded >= args.seconds:
                        break
                if time.monotonic() > deadline:
                    raise TimeoutError('frame benchmark timed out')
            intervals = [(b[0]-a[0])*1000 for a,b in zip(frames, frames[1:])]
            duration = frames[-1][0]-frames[0][0]
            result = {
                'backend':'CEF CPU BGRA pipe, standalone helper, no filter lists',
                'viewport':[1280,800], 'startup_ready_ms':round((ready-start)*1000,2),
                'local_load_ms_after_ready':round((loaded-ready)*1000,2),
                'frames':len(frames), 'sample_seconds':round(duration,3),
                'observed_fps':round((len(frames)-1)/duration,2),
                'median_frame_interval_ms':round(statistics.median(intervals),2),
                'pipe_mib_per_second':round(sum(size for _,size in frames[1:])/duration/1024**2,2),
                'note':'Host-only delivery measurement; excludes Rust conversion/UI rendering and is not page-load or end-to-end FPS.'
            }
            print(json.dumps(result, indent=2))
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(result, indent=2)+'\n')
            send({'op':'shutdown'})
            child.stdin.close()
            child.wait(timeout=35)
            if child.returncode:
                raise RuntimeError(f'helper exited {child.returncode}')
        finally:
            if child.poll() is None:
                child.kill();child.wait()
            server.shutdown()


if __name__ == '__main__':
    main()
