#!/usr/bin/env python3
"""Real CEF regressions for key translation and document script boundaries."""
import http.server
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest

# How long a wait on the host may take before it counts as hung: the
# shared hang bound (tools/tests/hang.py), so a loaded runner is not a failure.
HANG_BOUND = float(os.environ.get('TILO_TEST_HANG_BOUND_SECS', '120'))

HOST = Path(sys.argv.pop(1)).resolve()
COUNTER = "(()=>{window.reviewCount=(window.reviewCount||0)+1;const publish=()=>document.title='script-count-'+window.reviewCount;if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',publish,{once:true});else publish()})()"
PAGE = b'''<!doctype html><title>fixture</title><input id="first" autofocus><input id="second">
<script>
let keys=[];
document.addEventListener('keydown',e=>keys.push({key:e.key,code:e.code,keyCode:e.keyCode}));
document.addEventListener('keyup',()=>document.title=JSON.stringify({keys,value:first.value,focused:document.activeElement.id}));
</script>'''

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.end_headers()
        self.wfile.write(PAGE)
    def log_message(self, *args):
        pass

# What Chromium writes on every test machine, which says nothing of a failure.
NOISE = ('dbus/bus.cc', 'dbus/object_proxy.cc', 'Failed to adjust OOM score')


class HostTimeout(TimeoutError, queue.Empty):
    """No awaited event came in time: a TimeoutError, and the queue.Empty
    callers caught before the wait said what the host did meanwhile."""


class Host:
    def __init__(self, args=(), profile=None):
        # A profile given is the caller's, kept when the host closes: a later
        # host may open it again.
        self.profile = tempfile.TemporaryDirectory(prefix='tilo-regressions-') if profile is None else types.SimpleNamespace(name=str(profile), cleanup=lambda: None)
        self.log = tempfile.TemporaryFile()
        self.p = subprocess.Popen([str(HOST), '--profile='+self.profile.name, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log)
        self.events = queue.Queue()
        self.frames = queue.Queue()
        self.seen = []
        threading.Thread(target=self.read, daemon=True).start()
        self.until(lambda e: e['event']=='ready')
    def read(self):
        def exact(n):
            data = bytearray()
            while len(data)<n:
                chunk = self.p.stdout.read(n-len(data))
                if not chunk:
                    raise EOFError('host IPC ended')
                data.extend(chunk)
            return data
        try:
            while True:
                n, = struct.unpack('<I', exact(4))
                assert 1<=n<=64*1024*1024
                payload = exact(n)
                if payload[0]==1:
                    self.events.put(json.loads(payload[1:]))
                elif payload[0]==2:
                    self.frames.put(struct.unpack_from('<II',payload,1))
        except Exception as exc:
            self.events.put(exc)
    def send(self, **command):
        payload = b'\x01'+json.dumps(command).encode()
        self.p.stdin.write(struct.pack('<I',len(payload))+payload)
        self.p.stdin.flush()
    def until(self, predicate, timeout=HANG_BOUND):
        deadline = time.monotonic()+timeout
        while True:
            if time.monotonic() >= deadline:
                raise HostTimeout(self.report('no matching native event'))
            try:
                event = self.events.get(timeout=max(.01,deadline-time.monotonic()))
            except queue.Empty:
                raise HostTimeout(self.report('no matching native event')) from None
            if isinstance(event, Exception):
                raise self.ended(event)
            self.seen.append(event)
            if predicate(event):
                return event
    def settle(self, seconds=.4):
        deadline = time.monotonic()+seconds
        while time.monotonic()<deadline:
            try:
                event = self.events.get(timeout=max(.01,deadline-time.monotonic()))
            except queue.Empty:
                return
            if isinstance(event, Exception):
                raise self.ended(event)
            self.seen.append(event)
    def ended(self, error):
        """The reader's `error`, with how the host ended when its IPC did:
        a host gone mid-test says why on its stderr, or by its exit status."""
        if not isinstance(error, EOFError):
            return error
        try:
            code = self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            code = 'nothing yet'
        return EOFError(self.report(f'{error}; the host exited with {code}'))
    def report(self, what):
        return f'{what}; latest events: {self.seen[-12:]!r}\nthe host said:\n{self.diagnostics()}'
    def diagnostics(self, limit=6000):
        """The end of what the host wrote to stderr, without Chromium's
        complaints about the session bus and OOM scores a test machine has
        none of. Read where it is, never moving the offset the host writes at."""
        try:
            size = os.fstat(self.log.fileno()).st_size
            text = os.pread(self.log.fileno(), size, 0).decode(errors='replace')
        except (OSError, ValueError):
            return ''
        lines = [line for line in text.splitlines() if not any(noise in line for noise in NOISE)]
        return '\n'.join(lines)[-limit:]
    def navigate(self, url):
        self.send(op='navigate',url=url)
        self.until(lambda e: e['event']=='loaded' and e['url'].rstrip('/')==url.rstrip('/'))
        self.settle()
    def latest_title(self):
        return next(e['title'] for e in reversed(self.seen) if e['event']=='title')
    def close(self):
        if self.p.stdin.closed:
            return
        try:
            self.send(op='shutdown')
            self.until(lambda e:e['event']=='closed')
            code = self.p.wait(timeout=HANG_BOUND)
            assert code==0, self.report(f'the host exited with {code}')
        finally:
            if self.p.poll() is None:
                self.p.kill()
                self.p.wait()
            self.p.stdin.close()
            self.p.stdout.close()
            self.log.close()
            self.profile.cleanup()

class NativeRegressions(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=cls.server.serve_forever,daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}/'
        cls.cross_site = f'http://localhost:{cls.server.server_port}/'
    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
    def setUp(self):
        self.host = Host()
        self.addCleanup(self.host.close)
        self.host.navigate(self.url)
    def install_counter(self):
        self.host.send(op='scripts',scripts=[COUNTER])
        self.host.until(lambda e:e['event']=='title' and e['title'].startswith('script-count-'))
        self.host.settle()
    def test_session_history_is_directional_and_uses_real_entry_ids(self):
        self.host.navigate(self.url+'b')
        self.host.navigate(self.url+'c')
        self.host.send(op='back')
        self.host.until(lambda e:e['event']=='loaded' and e['url']==self.url+'b')
        self.host.send(op='navigation_history',request_id=7,direction='back')
        try:
            back=self.host.until(lambda e:e['event']=='navigation_history',timeout=HANG_BOUND)
        except queue.Empty:
            self.host.log.seek(0)
            self.fail(self.host.log.read().decode(errors='replace')[-5000:])
        self.assertEqual(back['direction'],'back')
        self.assertEqual(back['entries'][0]['url'],self.url)
        self.assertNotIn(self.url+'b',[e['url'] for e in back['entries']])
        self.assertNotIn(self.url+'c',[e['url'] for e in back['entries']])

        self.host.send(op='navigation_history',request_id=8,direction='forward')
        forward=self.host.until(lambda e:e['event']=='navigation_history',timeout=HANG_BOUND)
        self.assertEqual(forward['request_id'],8)
        self.assertEqual(forward['direction'],'forward')
        self.assertEqual([e['url'] for e in forward['entries']],[self.url+'c'])
        self.host.send(op='history_entry',request_id=8,id=forward['entries'][0]['id'])
        self.host.until(lambda e:e['event']=='loaded' and e['url']==self.url+'c')

    def test_physical_resize_at_retina_scale(self):
        self.host.send(op='resize',width=1920,height=1080,scale_factor=2.0)
        deadline=time.monotonic()+HANG_BOUND
        while True:
            dimensions=self.host.frames.get(timeout=max(.01,deadline-time.monotonic()))
            if dimensions!=(1280,800):
                break
        self.assertEqual(dimensions,(1920,1080))
        self.host.send(op='focus',focused=True)
        # The second input is at logical x=200..385. Physical x=600 must hit it.
        self.host.send(op='mouse_button',button='left',down=True,x=600,y=24)
        self.host.send(op='mouse_button',button='left',down=False,x=600,y=24)
        self.host.send(op='key',key='z',down=True,ctrl=False,shift=False,alt=False,meta=False)
        self.host.send(op='key',key='z',down=False,ctrl=False,shift=False,alt=False,meta=False)
        event=self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{'))
        self.assertEqual(json.loads(event['title'])['focused'],'second')

    def test_scale_only_resize_replaces_queued_low_density_paint_without_input(self):
        self.host.send(op='resize',width=800,height=452,scale_factor=1.0)
        deadline=time.monotonic()+HANG_BOUND
        while True:
            dimensions=self.host.frames.get(timeout=max(.01,deadline-time.monotonic()))
            if dimensions==(800,452):
                break
        # The logical view stays 800x452. A low-density paint already queued
        # by CEF must not replace the Retina frame published for this resize.
        self.host.send(op='resize',width=1600,height=904,scale_factor=2.0)
        deadline=time.monotonic()+HANG_BOUND
        while True:
            dimensions=self.host.frames.get(timeout=max(.01,deadline-time.monotonic()))
            if dimensions==(1600,904):
                break
        while True:
            try:
                self.host.frames.get_nowait()
            except queue.Empty:
                break
        self.host.send(op='resize',width=1600,height=904,scale_factor=2.0)
        dimensions=self.host.frames.get(timeout=HANG_BOUND)
        self.assertEqual(dimensions,(1600,904))

    def test_resize_burst_during_cross_site_navigation_reaches_latest_geometry(self):
        self.host.send(op='navigate', url=self.cross_site)
        # Match startup's changing chrome height and later Retina promotion.
        for width, height, scale in [(800,492,1), (800,484,1), (1600,968,2),
                                     (1600,984,2), (1600,984,2)]:
            self.host.send(op='resize', width=width, height=height, scale_factor=scale)
            time.sleep(.01)
        deadline = time.monotonic()+HANG_BOUND
        seen = []
        while time.monotonic() < deadline:
            dimensions = self.host.frames.get(timeout=max(.01, deadline-time.monotonic()))
            seen.append(dimensions)
            if dimensions == (1600,984):
                return
        self.fail(f'No final-size frame without additional input: {seen}')

    def test_editing_keys_and_text(self):
        h = self.host
        h.send(op='focus',focused=True)
        for key, code, value, focused in [('x','KeyX','x','first'),('ArrowLeft','ArrowLeft','x','first'),('y','KeyY','yx','first'),('Backspace','Backspace','x','first'),('Tab','Tab','x','second')]:
            with self.subTest(key=key):
                h.send(op='key',key=key,down=True,ctrl=False,shift=False,alt=False,meta=False)
                h.send(op='key',key=key,down=False,ctrl=False,shift=False,alt=False,meta=False)
                event=h.until(lambda e:e['event']=='title' and e['title'].startswith('{'))
                state=json.loads(event['title'])
                self.assertEqual(state['keys'][-1]['key'],key)
                self.assertEqual(state['keys'][-1]['code'],code)
                self.assertEqual(state['value'],value)
                self.assertEqual(state['focused'],focused)
    def test_internal_documents_excluded(self):
        self.install_counter()
        self.host.navigate('chrome://version/')
        self.assertEqual(self.host.latest_title(),'About Version')
        # Explicit configuration updates must also leave internal pages alone.
        self.host.send(op='scripts',scripts=["document.title='privileged-script-ran'"])
        self.host.settle()
        self.assertEqual(self.host.latest_title(),'About Version')
        self.host.navigate('about:blank')
        self.assertIn(self.host.latest_title(),('', 'about:blank'))
    def test_scripts_once_per_document_and_cross_site(self):
        self.install_counter()
        self.assertEqual(self.host.latest_title(),'script-count-1')
        self.host.send(op='reload')
        self.host.until(lambda e:e['event']=='loaded' and e['url']==self.url)
        self.host.settle()
        self.assertEqual(self.host.latest_title(),'script-count-1')
        self.host.navigate(self.cross_site)
        self.assertEqual(self.host.latest_title(),'script-count-1')
        self.host.navigate(self.url)
        self.assertEqual(self.host.latest_title(),'script-count-1')
        self.host.send(op='scripts',scripts=[COUNTER])
        self.host.settle()
        self.assertEqual(self.host.latest_title(),'script-count-1')
        self.host.send(op='scripts',scripts=[COUNTER+";void 0"])
        self.host.until(lambda e:e['event']=='title' and e['title']=='script-count-2')
        self.host.settle()
        self.assertEqual(self.host.latest_title(),'script-count-2')

if __name__=='__main__':
    unittest.main(verbosity=2)
