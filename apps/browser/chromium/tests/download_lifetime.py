#!/usr/bin/env python3
"""Close waits visibly for downloads; global IDs allow another tab to cancel."""
import http.server, threading, time, unittest
from pathlib import Path
from native_regressions import Host
PAYLOAD = b'download-lifetime\n' * 65536
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type','application/octet-stream')
        self.send_header('Content-Disposition','attachment; filename=lifetime.txt')
        self.send_header('Content-Length',str(len(PAYLOAD)))
        self.end_headers()
        try:
            for at in range(0,len(PAYLOAD),16384):
                self.wfile.write(PAYLOAD[at:at+16384]);self.wfile.flush();time.sleep(.02)
        except (BrokenPipeError,ConnectionResetError): pass
    def log_message(self,*args): pass
class DownloadHost(Host):
    def until(self, predicate, timeout=90):
        return super().until(predicate,timeout)
class Lifetime(unittest.TestCase):
    def test_origin_close_waits_then_completes_and_other_tab_cancels(self):
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Page)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        self.addCleanup(server.server_close);self.addCleanup(server.shutdown)
        host=DownloadHost();self.addCleanup(host.close)
        host.send(op='create_tab',tab=2)
        host.until(lambda e:e['event']=='tab_ready' and e.get('tab')==2)
        url=f'http://127.0.0.1:{server.server_port}/file'
        host.send(op='navigate',tab=2,url=url)
        prompt=host.until(lambda e:e['event']=='download_prompt')
        self.assertNotIn('tab',prompt)
        host.send(op='download_decision',tab=1,id=prompt['id'],allow=True)
        host.until(lambda e:e['event']=='download' and e['received']>=32768)
        host.send(op='try_close_tab',tab=2)
        host.until(lambda e:e['event']=='close_blocked' and e.get('tab')==2)
        done=host.until(lambda e:e['event']=='download' and e['id']==prompt['id'] and e['state']=='complete')
        self.assertEqual(Path(done['path']).read_bytes(),PAYLOAD)
        host.send(op='try_close_tab',tab=2)
        host.until(lambda e:e['event']=='closed' and e.get('tab')==2)
        host.send(op='navigate',tab=1,url=url)
        second=host.until(lambda e:e['event']=='download_prompt')
        self.assertNotEqual(second['id'],prompt['id'])
        host.send(op='download_decision',tab=1,id=second['id'],allow=True)
        host.until(lambda e:e['event']=='download' and e['id']==second['id'] and e['received']>=32768)
        host.send(op='create_tab',tab=3)
        host.until(lambda e:e['event']=='tab_ready' and e.get('tab')==3)
        host.send(op='download_cancel',tab=3,id=second['id'])
        host.until(lambda e:e['event']=='download' and e['id']==second['id'] and e['state']=='cancelled')
        host.send(op='try_close_tab',tab=1)
        host.until(lambda e:e['event']=='closed' and e.get('tab')==1)
        host.send(op='navigate',tab=3,url=url)
        pending=host.until(lambda e:e['event']=='download_prompt')
        host.send(op='create_tab',tab=4)
        host.until(lambda e:e['event']=='tab_ready' and e.get('tab')==4)
        host.send(op='try_close_tab',tab=3)
        host.until(lambda e:e['event']=='download' and e['id']==pending['id'] and e['state']=='cancelled')
        host.until(lambda e:e['event']=='closed' and e.get('tab')==3)
if __name__=='__main__': unittest.main(verbosity=2)
