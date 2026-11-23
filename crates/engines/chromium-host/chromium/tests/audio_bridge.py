#!/usr/bin/env python3
"""A page's sound reaches the app as PCM events when the host runs with
--tilo-audio-bridge, and the host opens no audio device of its own.

Runs a real host against a data: page that plays a 440 Hz tone after a click
(Chromium's autoplay policy wants a gesture), and checks the audio_start /
audio / audio_stop events: stereo 48 kHz, whole little-endian s16 frames, and
a tone that is actually in the samples.
"""
import argparse
import base64
import json
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('--host-arg', action='append', default=[], help='extra switch for the host (diagnosis)')
parser.add_argument('--no-bridge', action='store_true', help='diagnosis: let Chromium use its own audio')
parser.add_argument('--seconds', type=int, default=90)
args = parser.parse_args()

PAGE = """<html><body style="margin:0"><button style="width:100vw;height:100vh" id=b>play</button><script>
b.onclick=async()=>{const c=new AudioContext();await c.resume();const o=c.createOscillator();o.frequency.value=440;
const g=c.createGain();g.gain.value=.5;o.connect(g).connect(c.destination);o.start();setTimeout(()=>{o.stop();document.title='stopped'},1500);document.title='playing'};
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

with tempfile.TemporaryDirectory(prefix='tilo-cef-audio-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile] + ([] if args.no_bridge else ['--tilo-audio-bridge']) + [
                             '--ozone-platform=headless', '--disable-gpu'] + args.host_arg,
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
    started = None
    pcm = bytearray()
    stopped = False
    clicked = False
    try:
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline and not stopped:
            event = incoming.get(timeout=max(.1, deadline - time.monotonic()))
            if isinstance(event, Exception):
                raise event
            name = event['event']
            if name not in ('audio', 'frame'):
                print('event', name, flush=True)
            if name == 'ready':
                send({'op': 'resize', 'width': 400, 'height': 300, 'scale_factor': 1.0})
                send({'op': 'navigate', 'url': URL})
            elif name == 'loaded' and event['url'].startswith('data:') and not clicked:
                clicked = True
                for down in (None, True, False):
                    if down is None:
                        send({'op': 'mouse_move', 'x': 50, 'y': 50})
                    else:
                        send({'op': 'mouse_button', 'x': 50, 'y': 50, 'button': 'left', 'down': down})
            elif name == 'audio_start':
                assert started is None, 'one stream for one page'
                started = event
            elif name == 'audio':
                assert started, 'PCM before audio_start'
                pcm += base64.b64decode(event['pcm'])
            elif name == 'audio_stop' and started:
                stopped = True
        assert started, 'no audio_start: the page never reached the bridge'
        assert (started['rate'], started['channels']) == (48000, 2), started
        assert stopped, 'no audio_stop'
        assert len(pcm) % 4 == 0 and len(pcm) >= 48000 * 4 // 2, len(pcm)
        samples = struct.unpack('<%dh' % (len(pcm) // 2), bytes(pcm))
        left = samples[0::2]
        assert max(left) > 8000, 'the tone is in the samples'
        loud = [i for i, v in enumerate(left) if abs(v) > 100]
        tone = left[loud[0]:loud[-1] + 1]
        crossings = sum(1 for a, b in zip(tone, tone[1:]) if a < 0 <= b)
        seconds = len(tone) / 48000
        assert seconds > 1.0 and 400 < crossings / seconds < 480, (seconds, crossings / seconds)
        print('PASS: audio_start 48000/2, %.2fs of tone, ~%.0f Hz tone, audio_stop' % (seconds, crossings / seconds))
        send({'op': 'shutdown'})
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        crashes = diagnostics.count('stack smashing')
        probed = 'ALSA' in diagnostics or 'pulse' in diagnostics.lower()
        failing = sys.exc_info()[0] is not None
        if failing or probed or crashes:
            print(diagnostics[-3000:])
        if not failing and (probed or crashes):
            raise SystemExit('the host probed an audio device or a subprocess crashed (%d)' % crashes)
