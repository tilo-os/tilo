#!/usr/bin/env python3
"""An input method's composition, the focused field's place and physical key codes.

The protocol's ime_composition / ime_commit / ime_cancel ops must reach a page
field the way a native field takes an input method: compositionstart, one
compositionupdate per preedit, compositionend with the committed text, and
the text in the field. The host reports the focused field (text_field) and
the text being composed (ime_composition_bounds) so the window places the
input method's candidates where the person types. A key's DOM code decides
its place: German z on the US Y key, and Ctrl+Ф on a Russian layout selects
all as Ctrl+A does.
"""
import http.server
import json
import os
import struct
import subprocess
import sys
import tempfile
import threading
import time

PAGE = b"""<!doctype html><meta charset=utf-8>
<style>body{margin:0} input{position:absolute;box-sizing:border-box;width:200px;height:30px;font:16px sans-serif}</style>
<input id=field autofocus style="left:40px;top:60px">
<input id=secret type=password style="left:40px;top:160px">
<div id=plain style="position:absolute;left:400px;top:300px;width:100px;height:100px"></div>
<script>
const state={log:[],keys:[]};
const show=()=>{state.value=field.value;state.start=field.selectionStart;state.end=field.selectionEnd;
  document.title=JSON.stringify(state)};
for(const type of ['compositionstart','compositionupdate','compositionend'])
  field.addEventListener(type,e=>{state.log.push(type+':'+e.data);show()});
field.addEventListener('input',e=>{state.log.push('input:'+e.inputType+':'+(e.data??''));show()});
addEventListener('keydown',e=>{state.keys.push(e.key+'|'+e.code+'|'+e.keyCode);show()},true);
document.addEventListener('selectionchange',show);
show();
</script>"""


class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

    def log_message(self, *args):
        if os.environ.get('TILO_IME_DEBUG'):
            print(args, flush=True)


def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    extra = ['--use-mock-keychain'] if sys.platform == 'darwin' else []
    with tempfile.TemporaryDirectory(prefix='tilo-ime-') as profile:
        host = subprocess.Popen(
            [sys.argv[1], '--profile=' + profile, *extra],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=None if os.environ.get('TILO_IME_DEBUG') else subprocess.DEVNULL)
        # Every event, kept: a wait looks from a mark, so an event that came
        # before its wait began (a field focused as the page loaded) counts.
        events = []
        arrived = threading.Condition()

        def exact(n):
            data = b''
            while len(data) < n:
                part = host.stdout.read(n - len(data))
                if not part:
                    raise EOFError('host closed its output')
                data += part
            return data

        def read():
            try:
                while True:
                    n, = struct.unpack('<I', exact(4))
                    assert 0 < n <= 64 * 1024 * 1024
                    data = exact(n)
                    if data[0] == 1:
                        event = json.loads(data[1:])
                        if os.environ.get('TILO_IME_DEBUG'):
                            print(round(time.monotonic(), 3), event, flush=True)
                        with arrived:
                            events.append(event)
                            arrived.notify_all()
            except Exception as error:  # surfaced to the waiting test
                with arrived:
                    events.append(error)
                    arrived.notify_all()
        threading.Thread(target=read, daemon=True).start()

        def send(**command):
            body = b'\1' + json.dumps(command).encode()
            host.stdin.write(struct.pack('<I', len(body)) + body)
            host.stdin.flush()

        def mark():
            with arrived:
                return len(events)

        def until(predicate, what, since, timeout=30):
            end = time.monotonic() + timeout
            at = since
            with arrived:
                while True:
                    while at < len(events):
                        event = events[at]
                        at += 1
                        if isinstance(event, Exception):
                            raise event
                        if predicate(event):
                            return event
                    left = end - time.monotonic()
                    if left <= 0:
                        since_mark = [e.get('event') for e in events[since:] if isinstance(e, dict)
                                      and e.get('event') not in ('frame', 'title')]
                        raise AssertionError(f'timed out waiting for {what}; meanwhile: {since_mark[-20:]}')
                    arrived.wait(left)

        page = {}

        def title(predicate, what, since, timeout=10):
            def seen(event):
                if event['event'] != 'title' or not event['title'].startswith('{'):
                    return False
                page.clear()
                page.update(json.loads(event['title']))
                return predicate(page)
            return until(seen, what, since, timeout)

        def click(x, y):
            for down in (True, False):
                send(op='mouse_button', button='left', down=down, x=x, y=y)

        def key(name, code, ctrl=False, meta=False):
            for down in (True, False):
                send(op='key', key=name, code=code, down=down, ctrl=ctrl, shift=False, alt=False, meta=meta)

        try:
            ready = until(lambda e: e['event'] == 'ready', 'ready', 0, 60)
            assert ready.get('ime_composition') is True, ready
            send(op='resize', width=800, height=600, scale_factor=1.0)
            send(op='focus', focused=True)
            loading = mark()
            send(op='navigate', url='http://127.0.0.1:%d/ime' % server.server_port)
            title(lambda p: p.get('value') == '', 'the page', loading)
            click(60, 75)

            # The focused field, where the page laid it out.
            field = until(lambda e: e['event'] == 'text_field' and e.get('editable')
                          and e.get('purpose') == 'text' and 'x' in e, 'the field report', loading)
            box = (field['x'], field['y'], field['width'], field['height'])
            assert all(abs(a - b) <= 2 for a, b in zip(box, (40, 60, 200, 30))), field
            assert field['multiline'] is False, field

            # A composition: three preedits, then its commit. CEF starts
            # reporting the composition's box once a composition began, so
            # the box arrives from the preedits that follow the first.
            at = mark()
            send(op='ime_composition', text='に', selection_start=1, selection_end=1)
            title(lambda p: 'compositionupdate:に' in p['log'], 'the first preedit', at)
            at = mark()
            send(op='ime_composition', text='にほ', selection_start=2, selection_end=2)
            bounds = until(lambda e: e['event'] == 'ime_composition_bounds' and 'x' in e,
                           'the composition\'s box', at)
            assert 40 <= bounds['x'] < 240 and 60 <= bounds['y'] < 90, bounds
            assert bounds['height'] > 0 and bounds['y'] + bounds['height'] <= 92, bounds
            at = mark()
            send(op='ime_composition', text='日本語の', selection_start=4, selection_end=4)
            # The candidates follow the text: the box grows with it.
            wider = until(lambda e: e['event'] == 'ime_composition_bounds' and 'x' in e
                          and e['width'] > bounds['width'], 'the composition\'s wider box', at)
            assert wider['x'] == bounds['x'], (bounds, wider)
            title(lambda p: 'compositionupdate:日本語の' in p['log'], 'the third preedit', at)
            at = mark()
            send(op='ime_commit', text='日本語')
            title(lambda p: p['value'] == '日本語' and 'compositionend:日本語' in p['log'],
                  'the commit', at)
            log = [entry for entry in page['log'] if entry.startswith('composition')]
            assert log == ['compositionstart:', 'compositionupdate:に', 'compositionupdate:にほ',
                           'compositionupdate:日本語の', 'compositionupdate:日本語',
                           'compositionend:日本語'], page['log']
            assert page['start'] == page['end'] == 3, page
            until(lambda e: e['event'] == 'ime_composition_bounds' and 'x' not in e,
                  'the composition\'s end', at)

            # A withdrawn composition leaves the field as it was.
            at = mark()
            send(op='ime_composition', text='x', selection_start=1, selection_end=1)
            title(lambda p: p['value'] == '日本語x', 'a second composition', at)
            at = mark()
            send(op='ime_cancel')
            title(lambda p: p['value'] == '日本語' and p['log'][-1] != 'compositionupdate:x',
                  'the withdrawn composition', at)

            # Text an input method commits with nothing composed goes in at the caret.
            at = mark()
            send(op='ime_commit', text='!')
            title(lambda p: p['value'] == '日本語!', 'a bare commit', at)

            # German: z is on the US Y key; the page hears z there.
            at = mark()
            key('z', 'KeyY')
            title(lambda p: p['value'] == '日本語!z', 'German z', at)
            assert any(entry.endswith('|KeyY|90') for entry in page['keys']), page['keys']
            # Russian: Ctrl+Ф (Command on macOS) is the page's Select All.
            mac = sys.platform == 'darwin'
            at = mark()
            key('ф', 'KeyA', ctrl=not mac, meta=mac)
            title(lambda p: p['start'] == 0 and p['end'] == len('日本語!z'),
                  'Select All on a Russian layout', at)
            if not mac:
                assert any(entry.endswith('|KeyA|65') for entry in page['keys']), page['keys']

            # A password field is reported as one, and focus leaving a field says so.
            at = mark()
            click(60, 175)
            until(lambda e: e['event'] == 'text_field' and e.get('purpose') == 'password',
                  'the password field', at)
            at = mark()
            click(450, 350)
            until(lambda e: e['event'] == 'text_field' and e.get('editable') is False,
                  'focus leaving the fields', at)

            send(op='shutdown')
            assert host.wait(timeout=30) == 0
            print('PASS: compositions, field and composition boxes, and physical key codes reach the page')
        finally:
            if host.poll() is None:
                host.kill()
                host.wait()
    server.shutdown()


if __name__ == '__main__':
    main()
