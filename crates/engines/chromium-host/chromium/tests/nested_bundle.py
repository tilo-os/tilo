#!/usr/bin/env python3
"""Exercise the CEF host installed inside the outer Rust browser application."""
import http.server
import threading
import unittest
from native_regressions import HOST, Host

class NestedBundle(unittest.TestCase):
    def test_nested_runtime_initializes_navigates_and_delivers_frame(self):
        self.assertGreaterEqual(sum(parent.suffix=='.app' for parent in HOST.parents),2,
                                'Pass the runtime executable nested inside Browser.app')
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Type','text/html')
                self.end_headers()
                self.wfile.write(b'<!doctype html><title>nested runtime</title><body style="background:#1260b0">nested runtime')
            def log_message(self,*args): pass
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        host=Host()
        self.addCleanup(host.close)
        host.navigate(f'http://127.0.0.1:{server.server_port}/')
        self.assertEqual(host.latest_title(),'nested runtime')
        self.assertEqual(host.frames.get(timeout=10),(1280,800))

if __name__=='__main__': unittest.main(verbosity=2)
