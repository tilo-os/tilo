#!/usr/bin/env python3
"""Retained CEF tabs: routing, hidden painting, shared cookies and close isolation."""
import os, http.server, json, queue, struct, subprocess, sys, tempfile, threading, time
# How long a wait on the host may take before it counts as hung: the
# shared hang bound (tools/tests/hang.py), so a loaded runner is not a failure.
HANG_BOUND=float(os.environ.get('TILO_TEST_HANG_BOUND_SECS','120'))
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ("<title>"+self.path+"</title><body style='height:9000px'>"+
                "<script>if(location.pathname=='/one')document.cookie='shared=yes; path=/';"+
                "setInterval(()=>document.title=location.pathname+'|'+document.cookie+'|'+scrollY,100)</script>"+
                "<a target='_blank' href='/popup'>popup</a><a style='position:absolute;top:60px;left:8px' href='/modifier'>modifier</a>").encode()
        self.send_response(200); self.send_header("Content-Type","text/html"); self.send_header("Content-Length",str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self,*a):
        if os.environ.get("TILO_TABS_DEBUG"): print(a,flush=True)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Page)
threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix='tilo-tabs-') as profile:
    log=None if os.environ.get("TILO_TABS_DEBUG") else tempfile.TemporaryFile()
    p=subprocess.Popen([sys.argv[1],'--profile='+profile],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log)
    events=queue.Queue()
    seen=[]
    def exact(n):
        data=b''
        while len(data)<n:
            part=p.stdout.read(n-len(data))
            if not part: raise EOFError()
            data+=part
        return data
    def read():
        try:
            while True:
                n,=struct.unpack('<I',exact(4)); assert 0<n<=64*1024*1024
                data=exact(n)
                if data[0]==1:
                    event=json.loads(data[1:])
                    if os.environ.get('TILO_TABS_DEBUG'): print(round(time.monotonic(),3),event,flush=True)
                    events.put(event)
                else: events.put({'event':'frame','tab':struct.unpack_from('<I',data,1)[0] if data[0]==3 else 1})
        except Exception as e: events.put(e)
    threading.Thread(target=read,daemon=True).start()
    def send(**d):
        b=b'\1'+json.dumps(d).encode(); p.stdin.write(struct.pack('<I',len(b))+b); p.stdin.flush()
    def until(predicate,timeout=HANG_BOUND):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            try: e=events.get(timeout=max(.01,end-time.monotonic()))
            except queue.Empty: break
            if isinstance(e,Exception): raise AssertionError(f'{e!r}; the host exited with {p.poll()}; saw {seen[-20:]}')
            if e['event']!='frame': seen.append(e)
            if predicate(e): return e
        raise AssertionError(f'event timeout; saw {seen[-20:]}')
    base='http://127.0.0.1:'+str(server.server_port)
    try:
        until(lambda e:e['event']=='ready')
        send(op='navigate',url=base+'/one')
        until(lambda e:e['event']=='title' and 'shared=yes' in e['title'])
        send(op='create_tab',tab=2)
        send(op='navigate',tab=2,url=base+'/two')
        until(lambda e:e['event']=='title' and e.get('tab')==2 and '/two|shared=yes' in e['title'])
        send(op='visibility',tab=1,visible=False)
        send(op='visibility',tab=2,visible=True)
        until(lambda e:e['event']=='frame' and e.get('tab')==2)
        send(op='scroll',tab=2,x=100,y=100,dx=0,dy=-600)
        until(lambda e:e['event']=='title' and e.get('tab')==2 and not e['title'].endswith('|0'))
        send(op='visibility',tab=2,visible=False)
        send(op='visibility',tab=1,visible=True)
        # A new-window link reports an intent instead of replacing tab 1.
        send(op='mouse_button',tab=1,button='left',down=True,x=20,y=15)
        send(op='mouse_button',tab=1,button='left',down=False,x=20,y=15)
        until(lambda e:e['event']=='open_tab' and e.get('tab')==1 and e['url']==base+'/popup')
        send(op='modifiers',tab=1,meta=sys.platform=='darwin',ctrl=sys.platform!='darwin',shift=False,alt=False)
        send(op='mouse_button',tab=1,button='left',down=True,x=20,y=65)
        send(op='mouse_button',tab=1,button='left',down=False,x=20,y=65)
        until(lambda e:e['event']=='open_tab' and e.get('tab')==1 and e['url']==base+'/modifier')
        send(op='modifiers',tab=1,meta=False,ctrl=False,shift=False,alt=False)
        send(op='mouse_button',tab=1,button='middle',down=True,x=20,y=65)
        send(op='mouse_button',tab=1,button='middle',down=False,x=20,y=65)
        until(lambda e:e['event']=='open_tab' and e.get('tab')==1 and e['url']==base+'/modifier')
        send(op='close_tab',tab=1)
        until(lambda e:e['event']=='closed' and e.get('tab',1)==1)
        send(op='visibility',tab=2,visible=True)
        send(op='navigate',tab=2,url=base+'/survives')
        until(lambda e:e['event']=='title' and e.get('tab')==2 and '/survives|shared=yes' in e['title'])
        send(op='back',tab=2)
        until(lambda e:e['event']=='title' and e.get('tab')==2 and '/two|shared=yes' in e['title'] and not e['title'].endswith('|0'))
        send(op='shutdown'); assert p.wait(timeout=HANG_BOUND)==0
        print('PASS: real retained tabs, routed frames, shared cookies, scroll and close isolation')
    finally:
        if p.poll() is None: p.kill();p.wait()
        if log:
            log.seek(0); said=log.read().decode(errors='replace')
            if said: print(said[-20000:])
server.shutdown()
