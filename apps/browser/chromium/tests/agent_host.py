#!/usr/bin/env python3
"""What an agent host relies on, in real Chromium: the process budget, pages
nobody sees costing nothing, and the paint stream's two engine ops."""
import base64
import http.server
import json
import queue
import socket
import subprocess
import sys
import threading
import time
import unittest
from native_regressions import Host

PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP4z8DAwMDAxMDAwMDAAAANHQEDasKb6QAAAABJRU5ErkJggg==')
ANIMATED = b'''<!doctype html><title>Animated</title><h1 id="h">tick</h1>
<img src="/dot.png" width="32" height="32">
<script>let n = 0; setInterval(() => { document.body.style.background = n++ % 2 ? '#fff' : '#eee'; }, 50);</script>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body, kind = (PNG, 'image/png') if self.path.startswith('/dot.png') else (ANIMATED, 'text/html')
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def process_tree(root):
    """`(pid, kind)` for the host and every process below it."""
    rows = []
    for line in subprocess.run(['ps', '-axo', 'pid=,ppid=,command='], capture_output=True, text=True).stdout.splitlines():
        parts = line.split(None, 2)
        if len(parts) >= 2:
            rows.append((int(parts[0]), int(parts[1]), parts[2] if len(parts) > 2 else ''))
    keep, grown = {root}, True
    while grown:
        grown = False
        for pid, ppid, _ in rows:
            if ppid in keep and pid not in keep:
                keep.add(pid)
                grown = True
    tree = []
    for pid, _, command in rows:
        if pid not in keep:
            continue
        kind = 'browser' if pid == root else command.split('--type=')[1].split()[0] if '--type=' in command else 'other'
        if '--utility-sub-type=' in command:
            kind += ':' + command.split('--utility-sub-type=')[1].split()[0].split('.')[-1]
        tree.append((pid, kind))
    return tree


class AgentHost(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('0.0.0.0', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        port = cls.server.server_port
        # Four addresses of this machine are four different sites.
        cls.sites = [f'http://localhost:{port}', f'http://127.0.0.1:{port}', f'http://[::1]:{port}',
                     f'http://{socket.gethostname()}:{port}']

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.h = Host()
        self.addCleanup(self.h.close)
        self.h.send(op='visibility', visible=True)
        self.h.send(op='resize', width=800, height=600, scale_factor=1.0)
        self.request = 0

    def open_tab(self, tab, url):
        self.h.send(op='create_tab', tab=tab)
        self.h.until(lambda e: e['event'] == 'tab_ready' and e.get('tab') == tab)
        self.h.send(op='resize', tab=tab, width=800, height=600, scale_factor=1.0)
        self.h.send(op='navigate', tab=tab, url=url + '/')
        self.h.until(lambda e: e['event'] == 'loaded' and e.get('tab') == tab)

    def drain_frames(self):
        count = 0
        while True:
            try:
                self.h.frames.get_nowait()
                count += 1
            except queue.Empty:
                return count

    def page(self, action, **fields):
        self.request += 1
        request = self.request
        self.h.send(op='page', request_id=request, action=action, **fields)
        return self.h.until(lambda e: e['event'] in ('page_result', 'paint_resource', 'paint_snapshot')
                            and e['request_id'] == request)

    def test_four_tabs_on_four_sites_cost_six_processes(self):
        self.h.navigate(self.sites[0] + '/')
        for tab, site in enumerate(self.sites[1:], 2):
            self.open_tab(tab, site)
        time.sleep(2)
        kinds = [kind for _, kind in process_tree(self.h.p.pid)]
        # Linux keeps its zygotes: they are the renderer sandbox, not a cost
        # this budget may trade away.
        budget = [kind for kind in kinds if kind != 'zygote']
        self.assertEqual(sorted(budget), sorted(['browser', 'utility:StorageService'] + ['renderer'] * 4), kinds)
        self.assertNotIn('gpu-process', kinds)
        self.assertNotIn('utility:NetworkService', kinds)
        # A fifth tab on a site already open shares that site's renderer.
        self.open_tab(5, self.sites[0])
        time.sleep(1)
        self.assertEqual([kind for _, kind in process_tree(self.h.p.pid)].count('renderer'), 4)

    def test_a_hidden_page_paints_nothing_yet_answers_the_agent(self):
        self.h.navigate(self.sites[0] + '/')
        time.sleep(.5)
        self.assertGreater(self.drain_frames(), 0, 'a shown, animated page paints')
        self.h.send(op='visibility', visible=False)
        time.sleep(.5)
        self.drain_frames()
        time.sleep(1.5)
        self.assertEqual(self.drain_frames(), 0, 'a hidden page paints no frame')
        read = self.page('outline')
        self.assertTrue(read['ok'], read)
        self.assertIn('heading "tick"', read['outline'])
        self.h.send(op='visibility', visible=True)
        time.sleep(1)
        self.assertGreater(self.drain_frames(), 0, 'shown again, it paints again')

    def test_the_paint_stream_reads_layout_and_cached_resources_from_the_engine(self):
        self.h.navigate(self.sites[0] + '/')
        self.request += 1
        self.h.send(op='paint_capture', request_id=self.request,
                    styles=['background-color', 'color', 'font-size', 'display'])
        captured = self.h.until(lambda e: e['event'] == 'paint_snapshot' and e['request_id'] == self.request)
        self.assertTrue(captured['ok'], captured)
        snapshot = json.loads(captured['snapshot'])
        self.assertIn('tick', snapshot['strings'])
        layout = snapshot['documents'][0]['layout']
        self.assertEqual(len(layout['nodeIndex']), len(layout['bounds']))
        self.assertTrue(layout['paintOrders'], 'paint order is included')
        self.request += 1
        self.h.send(op='paint_resource', request_id=self.request, url=self.sites[0] + '/dot.png')
        resource = self.h.until(lambda e: e['event'] == 'paint_resource' and e['request_id'] == self.request)
        self.assertTrue(resource['ok'], resource)
        self.assertTrue(resource['base64'])
        self.assertEqual(base64.b64decode(resource['content']), PNG, 'the bytes the page loaded')
        self.request += 1
        self.h.send(op='paint_resource', request_id=self.request, url=self.sites[0] + '/never-loaded.png')
        missing = self.h.until(lambda e: e['event'] == 'paint_resource' and e['request_id'] == self.request)
        self.assertFalse(missing['ok'], 'only what the page already loaded, never a new fetch')


if __name__ == '__main__':
    unittest.main(verbosity=2)
