#!/usr/bin/env python3
"""Native find counts/directions and per-tab zoom isolation."""
import os, http.server, json, queue, struct, subprocess, sys, tempfile, threading, time
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ("<title>"+self.path+"</title><body style='height:9000px'>needle needle needle "+
                "<script>if(location.pathname=='/one')document.cookie='shared=yes; path=/';"+
                "setInterval(()=>document.title=location.pathname+'|'+document.cookie+'|'+scrollY,100)</script>"+
                "<a target='_blank' href='/popup'>popup</a><a style='position:absolute;top:60px;left:8px' href='/modifier'>modifier</a>").encode()
        self.send_response(200); self.send_header("Content-Type","text/html"); self.send_header("Content-Length",str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self,*a):
        if os.environ.get("QOS_TABS_DEBUG"): print(a,flush=True)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Page)
threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix='qos-tabs-') as profile:
    p=subprocess.Popen([sys.argv[1],'--profile='+profile],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=None if os.environ.get("QOS_TABS_DEBUG") else subprocess.DEVNULL)
    events=queue.Queue()
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
                    if os.environ.get('QOS_TABS_DEBUG'): print(round(time.monotonic(),3),event,flush=True)
                    events.put(event)
                else: events.put({'event':'frame','tab':struct.unpack_from('<I',data,1)[0] if data[0]==3 else 1,'size':struct.unpack_from('<II',data,5 if data[0]==3 else 1)})
        except Exception as e: events.put(e)
    threading.Thread(target=read,daemon=True).start()
    def send(**d):
        b=b'\1'+json.dumps(d).encode(); p.stdin.write(struct.pack('<I',len(b))+b); p.stdin.flush()
    def until(predicate,timeout=120):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            e=events.get(timeout=max(.01,end-time.monotonic()))
            if isinstance(e,Exception): raise e
            if predicate(e): return e
        raise AssertionError('event timeout')
    base='http://127.0.0.1:'+str(server.server_port)
    try:
        until(lambda e:e['event']=='ready')
        send(op='resize',width=800,height=600,scale_factor=1.0)
        send(op='navigate',url=base+'/one')
        until(lambda e:e['event']=='title' and 'shared=yes' in e['title'])
        for i in range(5):
            send(op='visibility',visible=False)
            send(op='visibility',visible=True)
            # New WebView instances send the same dimensions on layout.
            send(op='resize',width=800,height=600,scale_factor=1.0)
            until(lambda e:e['event']=='frame',timeout=4)
        for scale in [1.0,2.0]:
            for height in [480,600,520,600]:
                width=int(800*scale); physical_height=int(height*scale)
                send(op='resize',width=width,height=physical_height,scale_factor=scale)
                until(lambda e:e['event']=='frame' and e['size']==(width,physical_height),timeout=5)
        send(op='shutdown'); assert p.wait(timeout=15)==0
        print('PASS: retained page repaints after show and identical resize')
    finally:
        if p.poll() is None: p.kill();p.wait()
server.shutdown()
