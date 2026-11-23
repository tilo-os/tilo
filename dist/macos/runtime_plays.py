#!/usr/bin/env python3
"""Prove a tilolang runtime plays video: a generated clip, opened by a tilolang app.

    runtime_plays.py --tilolang BIN --runtime DIR --work DIR

`--runtime` is a runtime bundle's tree (`bin/tilolang-app`, and on a Mac the GStreamer it
carries under `lib/gstreamer`, found as the daemon finds it). The clip is made with this
machine's `gst-launch-1.0` (test pattern and tone, H.264 and AAC in MP4); the app is the test
package `apps/tilolang/tests/videoplays`, built with `--tilolang`. The runtime runs it headless:
the clip is named through the app's `play` action, playback must report moving on through the
app's `reached` action, and the window's picture must show the decoded test pattern.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import zlib

REPO = Path(__file__).resolve().parents[2]
PACKAGE = REPO / 'apps/tilolang/tests/videoplays'
PACKAGE_ID = 'so.tilo.lang.videoplays'
# The test pattern's yellow bar, which nothing in the window draws but the decoded clip.
YELLOW = (192, 192, 0)


def run(*command, **kwargs):
    print('+', ' '.join(map(str, command)), file=sys.stderr, flush=True)
    return subprocess.run(command, check=True, **kwargs)


def encoder(*names):
    """The first of `names` this machine's GStreamer has."""
    for name in names:
        if subprocess.run(['gst-inspect-1.0', '--exists', name]).returncode == 0:
            return name
    raise SystemExit(f'none of {", ".join(names)} is installed')


def clip(path):
    """Two minutes of the SMPTE pattern and a tone: still playing when the window is captured."""
    video = encoder('x264enc', 'vtenc_h264', 'openh264enc')
    audio = encoder('avenc_aac', 'fdkaacenc', 'voaacenc', 'faac')
    run('gst-launch-1.0', '-q', 'mp4mux', 'name=mux', '!', 'filesink', f'location={path}',
        'videotestsrc', 'num-buffers=3600', 'pattern=smpte', '!',
        'video/x-raw,width=320,height=240,framerate=30/1', '!', 'videoconvert', '!', video,
        '!', 'h264parse', '!', 'mux.',
        'audiotestsrc', 'num-buffers=5200', '!', 'audioconvert', '!', audio, '!', 'aacparse',
        '!', 'mux.')
    if path.stat().st_size == 0:
        raise SystemExit(f'{path} is empty')


def gstreamer_env(runtime):
    """What the daemon sets for a runtime that carries its GStreamer (apps/macos.rs)."""
    root = runtime / 'lib/gstreamer'
    if not root.is_dir():
        return {}
    plugins, scanner = root / 'PlugIns', root / 'Helpers/gst-plugin-scanner'
    fonts = root / 'Resources/fontconfig'
    return {
        'GST_PLUGIN_SYSTEM_PATH_1_0': plugins, 'GST_PLUGIN_SYSTEM_PATH': plugins,
        'GST_PLUGIN_PATH_1_0': '', 'GST_PLUGIN_PATH': '',
        'GST_PLUGIN_SCANNER_1_0': scanner, 'GST_PLUGIN_SCANNER': scanner,
        'FONTCONFIG_FILE': fonts / 'fonts.conf', 'FONTCONFIG_PATH': fonts,
        'PANGOCAIRO_BACKEND': 'fontconfig', 'LIBTHAI_DICTDIR': root / 'Resources/libthai',
    }


def drive(app, env, spec):
    """Whether the runtime drove `spec` to its end; what it writes is written either way."""
    print('+', app, '--headless-drive', json.dumps(spec), file=sys.stderr, flush=True)
    return subprocess.run([app, '--headless-drive', json.dumps(spec)], env=env,
                          stdin=subprocess.DEVNULL).returncode == 0


def pixels(path):
    """The rows of an 8-bit RGB or RGBA PNG, as (r, g, b) tuples."""
    data = path.read_bytes()
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise SystemExit(f'{path} is not a PNG')
    at, idat, header = 8, b'', None
    while at < len(data):
        length, kind = struct.unpack('>I4s', data[at:at + 8])
        body = data[at + 8:at + 8 + length]
        if kind == b'IHDR':
            header = struct.unpack('>IIBBBBB', body)
        elif kind == b'IDAT':
            idat += body
        at += 12 + length
    width, height, depth, colour, _, _, interlace = header
    if depth != 8 or colour not in (2, 6) or interlace:
        raise SystemExit(f'{path}: unexpected PNG format {header}')
    step = 3 if colour == 2 else 4
    stride = width * step
    raw, rows, previous = zlib.decompress(idat), [], bytearray(stride)
    for y in range(height):
        kind, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for x in range(stride):
            left = line[x - step] if x >= step else 0
            up, corner = previous[x], previous[x - step] if x >= step else 0
            if kind == 1:
                line[x] = (line[x] + left) & 255
            elif kind == 2:
                line[x] = (line[x] + up) & 255
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 255
            elif kind == 4:
                guess = left + up - corner
                near = min((abs(guess - left), left), (abs(guess - up), up), (abs(guess - corner), corner),
                           key=lambda pair: pair[0])
                line[x] = (line[x] + near[1]) & 255
        rows.append([tuple(line[x:x + 3]) for x in range(0, stride, step)])
        previous = line
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--tilolang', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    args = parser.parse_args()
    work = args.work.resolve()
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    runtime = args.runtime.resolve()
    app = runtime / 'bin/tilolang-app'

    clip(work / 'clip.mp4')
    run(args.tilolang, 'build', PACKAGE, '--out', work / 'built')
    payload, = (work / 'built').glob('*/payload')
    env = dict(os.environ, TILO_APP_MODULE=str(payload), TILO_PACKAGE_ID=PACKAGE_ID,
               XDG_STATE_HOME=str(work / 'state'), XDG_DATA_HOME=str(work / 'data'),
               XDG_CACHE_HOME=str(work / 'cache'))
    env.update({key: str(value) for key, value in gstreamer_env(runtime).items()})

    # Where the app's `play` is published, read from the app itself.
    if not drive(app, env, {'resources': str(work / 'resources.json')}):
        raise SystemExit('the runtime did not start the app')
    published = json.loads((work / 'resources.json').read_text())
    resource = next((entry['resource'] for entry in published
                     if any(action.get('action') == 'play' for action in entry.get('actions', []))), None)
    if resource is None:
        raise SystemExit(f'the app publishes no play action: {json.dumps(published)[:2000]}')

    # The line the view shows once playback moved on, by the id its anchor keeps (`as moved`).
    ids = dict(line.split(' = ', 1) for line in (PACKAGE / 'app.ids').read_text().splitlines()
               if ' = ' in line and not line.startswith('#'))
    moved = next(value.strip() for key, value in ids.items() if key.strip().endswith('moved'))
    shot = work / 'playing.png'
    went_on = drive(app, env, {
        'actions': [
            {'invoke': {'resource': resource, 'action': 'play',
                        'args': {'source': (work / 'clip.mp4').as_uri()}}},
            {'wait_for_named': {'target': moved, 'timeout_ms': 30000}},
        ],
        'capture': {'path': str(shot)},
        'targets': str(work / 'targets.json'),
    })
    shown = (work / 'targets.json').read_text()
    if not went_on:
        raise SystemExit(f'playback never reported moving on; the window shows: {shown}')
    rows = pixels(shot)
    yellow = sum(1 for row in rows for pixel in row
                 if all(abs(have - want) <= 40 for have, want in zip(pixel, YELLOW)))
    if yellow < 200:
        raise SystemExit(f'{shot} shows no decoded frame ({yellow} pattern pixels)')
    print(f'played: playback moved on, and the window shows {yellow} pixels of the decoded pattern')


if __name__ == '__main__':
    main()
