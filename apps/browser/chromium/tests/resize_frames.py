#!/usr/bin/env python3
"""An embedder can tell a frame of the page at the view's new size from one
the compositor painted before the page drew there.

A mail reader sizes its view to the document: it measures the page in a sliver
of a view, then grows the view to the document's height the moment it has
loaded. Offscreen Chromium on Linux does not wait for the renderer when a view
is resized (DelegatedFrameHost uses no resize deadline there): it paints the
new size at once with the page's old, smaller frame and the page's background
colour in the rest, a blank sheet where the document belongs. The page cannot
be made to arrive sooner, so the mail marks its end (apps/mail/src/html_mail.rs,
END_MARK): a fixed square in the view's bottom-right corner, blended to differ
from what is under it by one level, so it is invisible yet unlike the colour
beside it on its row. Only the page's own frame at the new size has it there.
This test holds Chromium to that, on a light page and on a dark one: every open
yields a marked frame, and every marked frame shows the page, whatever the
page's own style says to remove the mark.
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
parser.add_argument('host_args', nargs='*')
args = parser.parse_args()

END_MARK = '@layer{html::after{all:initial!important;content:""!important;display:block!important;position:fixed!important;right:0!important;bottom:0!important;width:2px!important;height:2px!important;margin:0!important;background:rgb(1,1,1)!important;mix-blend-mode:difference!important;pointer-events:none!important;z-index:2147483647!important}html{display:block!important;visibility:visible!important;opacity:1!important;transform:none!important;translate:none!important;scale:none!important;rotate:none!important;perspective:none!important;filter:none!important;backdrop-filter:none!important;contain:none!important;container-type:normal!important;content-visibility:visible!important;will-change:auto!important;clip-path:none!important;mask:none!important;zoom:1!important;isolation:auto!important;mix-blend-mode:normal!important}}'
PAGE = ('<!doctype html><html><head><style>html,body{margin:0;background:BACKGROUND}'
        '#block{position:absolute;top:150px;left:0;width:100%;height:100px;background:#d02020}'
        + END_MARK + '</style></head><body><p>Hello</p><div id="block"></div>'
        '<style>HOSTILE</style></body></html>')
# What a mail's own style may say to remove the mark, and whether the page
# still shows its block.
HOSTILE = (('', True),
           ('html::after{display:none!important;content:none!important}', True),
           ('*{display:none!important}*::after{display:none!important;content:none!important}',
            False),
           ('html{transform:scale(.5)!important;opacity:0!important;filter:blur(9px)!important}'
            'html::after{position:static!important;mix-blend-mode:normal!important;'
            'background:transparent!important;transition:all 99s!important}', True))
SCALE = 2.0
WIDTH = 600
SLIVER = 4
GROWN = 400
LAST = 618
OPENS = 20


def read_exact(stream, length):
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError('host closed IPC')
        chunks.extend(chunk)
    return bytes(chunks)


def block_shown(pixels, width, y):
    """Whether the row at device pixel `y` is the block's red, in the middle (BGRA)."""
    at = (y * width + width // 2) * 4
    b, g, r = pixels[at], pixels[at + 1], pixels[at + 2]
    return r > 160 and g < 90 and b < 90


# A light mail's sheet and the reader's dark ground.
BACKGROUNDS = ('#ffffff', '#121417')
# How far left of the corner the colour beside the mark is read, as the reader
# reads it (apps/mail/src/reader/html.rs, is_page).
BESIDE = 16


def marked(frame):
    """Whether the page's end mark is in the frame's bottom-right corner (BGRA):
    one level off the colour beside it on its row."""
    width, height, pixels = frame['width'], frame['height'], frame['pixels']
    row = (height - 2) * width
    at = (row + width - 2) * 4
    beside = (row + width - 2 - BESIDE) * 4
    return pixels[at + 3] != 0 and pixels[at:at + 4] != pixels[beside:beside + 4]


def describe(frame):
    """A blank frame's middle column, top to bottom, as BGRA at a few rows."""
    width, height, pixels = frame['width'], frame['height'], frame['pixels']
    rows = sorted({0, 4, int(SLIVER * SCALE), int(200 * SCALE), height // 2, height - 1})
    return ', '.join(f'y{y}={pixels[(y * width + width // 2) * 4:(y * width + width // 2) * 4 + 4].hex()}'
                     for y in rows if y < height)


with tempfile.TemporaryDirectory(prefix='tilo-cef-resize-') as profile:
    log = tempfile.TemporaryFile()
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile,
                             '--tilo-sealed-documents', *args.host_args],
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
                    offset = 5 if data[0] == 3 else 1
                    width, height = struct.unpack_from('<II', data, offset)
                    incoming.put({'event': 'frame', 'width': width, 'height': height,
                                  'pixels': data[offset + 8:]})
        except Exception as exc:  # noqa: BLE001 - handed to the main thread
            incoming.put(exc)

    threading.Thread(target=reader, daemon=True).start()

    def send(command):
        packet = b'\x01' + json.dumps(command).encode()
        host.stdin.write(struct.pack('<I', len(packet)) + packet)
        host.stdin.flush()

    def until(predicate, timeout=HANG_BOUND, what='event'):
        deadline = time.monotonic() + timeout
        while True:
            try:
                event = incoming.get(timeout=max(.05, deadline - time.monotonic()))
            except queue.Empty:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}') from None
            if isinstance(event, Exception):
                raise event
            if predicate(event):
                return event
            if time.monotonic() >= deadline:
                if timeout < HANG_BOUND:
                    return None
                raise AssertionError(f'no {what}')

    try:
        until(lambda e: e['event'] == 'ready', what='ready')
        send({'op': 'document_size', 'tab': 1, 'enabled': True})
        grown = (int(WIDTH * SCALE), int(GROWN * SCALE))
        shown = 0
        unmarked = 0
        # The way a mail reader opens a message: the view still has the last
        # message's size when the page is sent, shrinks to a sliver to measure
        # it, and grows to the document's height the moment it has loaded. The
        # renderer is busiest then, so a paint before it drew at the new size
        # is likeliest; each open is one more chance to catch one.
        for opened in range(OPENS):
            send({'op': 'resize', 'width': grown[0], 'height': int(LAST * SCALE),
                  'scale_factor': SCALE})
            hostile, shows_block = HOSTILE[opened % len(HOSTILE)]
            background = BACKGROUNDS[opened // len(HOSTILE) % len(BACKGROUNDS)]
            page = PAGE.replace('BACKGROUND', background).replace('HOSTILE', hostile)
            url = 'data:text/html;base64,' + base64.b64encode(
                (page + f'<!-- open {opened} -->').encode()).decode()
            send({'op': 'navigate', 'url': url})
            send({'op': 'resize', 'width': grown[0], 'height': int(SLIVER * SCALE),
                  'scale_factor': SCALE})
            until(lambda e, url=url: e['event'] == 'loaded' and e.get('url') == url, what='loaded')
            send({'op': 'resize', 'width': grown[0], 'height': grown[1], 'scale_factor': SCALE})
            frames = []

            def page_at_grown_size(event, frames=frames):
                if event['event'] == 'frame' and (event['width'], event['height']) == grown:
                    frames.append(event)
                    return marked(event)
                return False

            def and_after(event, frames=frames):
                page_at_grown_size(event, frames)
                return False

            # The page's own frame comes once the renderer has drawn at the
            # new size, however busy the machine; then the frames after it.
            until(page_at_grown_size, what=f'open {opened}: a frame of the page at the grown size')
            until(and_after, timeout=0.3)
            painted = [frame for frame in frames if marked(frame)]
            assert painted, f'open {opened}: no frame of the page at the grown size'
            middle = int(200 * SCALE)
            blank = [index for index, frame in enumerate(painted)
                     if shows_block and not block_shown(frame['pixels'], frame['width'], middle)]
            assert not blank, (f'open {opened}: marked frames {blank} of {len(painted)} at the '
                               'grown size show no page there: ' + '; '.join(
                                   describe(painted[index]) for index in blank))
            unmarked += len(frames) - len(painted)
            shown += len(painted)
        send({'op': 'shutdown'})
        until(lambda e: e['event'] == 'closed', what='closed')
        assert host.wait(timeout=HANG_BOUND) == 0
        print(f'PASS: {OPENS} opens, {shown} marked frame(s) at the grown size, each showing the '
              f'page; {unmarked} unmarked')
    finally:
        if host.poll() is None:
            host.kill()
            host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
        if diagnostics and host.returncode:
            print(diagnostics[-20000:])
