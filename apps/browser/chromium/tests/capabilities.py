#!/usr/bin/env python3
"""Live CEF capability checks; uses actual IPC, network, and disk output."""
import http.server
import threading
import unittest
from pathlib import Path
from native_regressions import Host

PAYLOAD = b'qOS download fixture\n'
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        if self.path == '/download':
            self.send_header('Content-Disposition', 'attachment; filename="../../fixture.txt"')
            self.send_header('Content-Type', 'application/octet-stream')
            self.end_headers()
            self.wfile.write(PAYLOAD)
        else:
            self.send_header('Content-Type', 'text/html')
            self.end_headers()
            self.wfile.write(b'''<title>permission fixture</title><a style="position:absolute;top:50px;left:10px" href="/download">Download</a><button onclick="navigator.geolocation.getCurrentPosition(()=>document.title='granted',e=>document.title='denied-'+e.code)">Location</button>''')
    def log_message(self, *args): pass

class Capabilities(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=cls.server.serve_forever,daemon=True).start()
        cls.url=f'http://127.0.0.1:{cls.server.server_port}'
    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown();cls.server.server_close()
    def setUp(self):
        self.h=Host();self.addCleanup(self.h.close)
    def test_download_waits_for_accept_and_saves_exact_bytes(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        event=self.h.until(lambda e:e['event']=='download_prompt',timeout=5)
        self.assertNotIn('/',event['name'])
        name=event['name']
        self.h.send(op='download_decision',id=event['id'],allow=True)
        done=self.h.until(lambda e:e['event']=='download' and e['state']=='complete')
        saved=Path(done['path'])
        self.assertEqual(saved.read_bytes(),PAYLOAD)
        self.assertEqual(saved.name,name)
        # Each accepted download has an exclusive directory, never replaces a file.
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        event=self.h.until(lambda e:e['event']=='download_prompt')
        self.h.send(op='download_decision',id=event['id'],allow=True)
        other=self.h.until(lambda e:e['event']=='download' and e['state']=='complete')
        self.assertNotEqual(other['path'],str(saved))
        self.assertEqual(saved.read_bytes(),PAYLOAD)
    def test_declined_download_is_cancelled_without_a_saved_file(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        prompt=self.h.until(lambda e:e['event']=='download_prompt')
        self.h.send(op='download_decision',id=prompt['id'],allow=False)
        result=self.h.until(lambda e:e['event']=='download' and e['state']=='cancelled')
        self.assertFalse(result['path'])
        self.assertFalse((Path(self.h.profile.name)/'downloads').exists())
    def test_clear_on_exit_removes_data_before_next_startup(self):
        self.h.navigate(self.url+'/')
        root=Path(self.h.profile.name)/'default'
        root.mkdir(exist_ok=True)
        (root/'exit-clear-fixture').write_text('private')
        self.h.send(op='clear_on_exit',enabled=True)
        self.h.send(op='shutdown')
        self.h.until(lambda e:e['event']=='closed')
        self.assertEqual(self.h.p.wait(timeout=35),0)
        self.assertFalse(root.exists())
        # close() would send another shutdown, so only release fixture resources.
        self._cleanups.clear()
        self.h.p.stdin.close();self.h.p.stdout.close();self.h.log.close();self.h.profile.cleanup()
    def test_permission_denied_only_after_explicit_decision(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=18)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=18)
        prompt=self.h.until(lambda e:e['event']=='permission_prompt',timeout=5)
        self.assertIn('127.0.0.1',prompt['origin'])
        self.h.send(op='permission_decision',id=prompt['id'],allow=False)
        self.h.until(lambda e:e['event']=='title' and e['title']=='denied-1')

if __name__=='__main__': unittest.main(verbosity=2)
