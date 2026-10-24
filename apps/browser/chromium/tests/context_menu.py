#!/usr/bin/env python3
"""Native link menu targets, white page background and oversized-link safety."""
import os, http.server, json, queue, struct, subprocess, sys, tempfile, threading, time
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ("<title>"+self.path+"</title><body style='height:9000px'>needle needle needle "+
                "<script>if(location.pathname=='/one')document.cookie='shared=yes; path=/';"+
                "setInterval(()=>document.title=location.pathname+'|'+document.cookie+'|'+scrollY+'|'+innerWidth,100)</script>"+
                "<a target='_blank' href='/popup'>popup</a><a style='position:absolute;top:60px;left:8px' href='/modifier'>modifier</a>"+
                "<a style='position:absolute;top:100px;left:8px' href='/"+("x"*9000)+"'>oversized</a>").encode()
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
                else: events.put({'event':'frame','tab':struct.unpack_from('<I',data,1)[0] if data[0]==3 else 1,'pixel':list(data[13:17] if data[0]==3 else data[9:13])})
        except Exception as e: events.put(e)
    threading.Thread(target=read,daemon=True).start()
    def send(**d):
        b=b'\1'+json.dumps(d).encode(); p.stdin.write(struct.pack('<I',len(b))+b); p.stdin.flush()
    def until(predicate,timeout=60):
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
        send(op='visibility',visible=False)
        send(op='visibility',visible=True)
        frame=until(lambda e:e['event']=='frame')
        assert frame['pixel']==[255,255,255,255], frame
        send(op='mouse_button',button='right',down=True,x=20,y=65)
        send(op='mouse_button',button='right',down=False,x=20,y=65)
        menu=until(lambda e:e['event']=='context_menu',timeout=5)
        assert {26501,26502}.issubset({item.get('id') for item in menu['items']}), menu  # separators have none
        send(op='context_action',id=menu['id'],command=26501)
        until(lambda e:e['event']=='open_tab' and e['url']==base+'/modifier')
        send(op='mouse_button',button='right',down=True,x=20,y=105)
        send(op='mouse_button',button='right',down=False,x=20,y=105)
        send(op='zoom',level=1)
        def still_alive(event):
            assert event['event'] != 'context_menu', event
            return event['event']=='zoom'
        until(still_alive,timeout=5)
        # A tab closed with its menu still open: the host answers the menu,
        # and its other tab goes on.
        send(op='create_tab',tab=2)
        send(op='navigate',tab=2,url=base+'/two')
        until(lambda e:e['event']=='title' and e.get('tab')==2 and e['title'].startswith('/two|'))
        send(op='visibility',tab=1,visible=False)
        send(op='visibility',tab=2,visible=True)
        until(lambda e:e['event']=='frame' and e['tab']==2)
        send(op='mouse_button',tab=2,button='right',down=True,x=300,y=300)
        send(op='mouse_button',tab=2,button='right',down=False,x=300,y=300)
        until(lambda e:e['event']=='context_menu' and e.get('tab')==2,timeout=5)
        send(op='close_tab',tab=2)
        until(lambda e:e['event']=='closed' and e.get('tab')==2,timeout=15)
        send(op='visibility',tab=1,visible=True)
        send(op='zoom',tab=1,level=0)
        until(lambda e:e['event']=='zoom' and e.get('tab',1)==1,timeout=15)
        assert p.poll() is None, f'the host exited with {p.returncode}'
        # Closed with a menu still open (a window closed over it): the host
        # answers the menu and exits cleanly.
        send(op='mouse_button',button='right',down=True,x=300,y=300)
        send(op='mouse_button',button='right',down=False,x=300,y=300)
        until(lambda e:e['event']=='context_menu',timeout=5)
        send(op='shutdown'); code=p.wait(timeout=15); assert code==0, f'the host exited with {code}'
        print('PASS: native link context target and action')
    finally:
        if p.poll() is None: p.kill();p.wait()
        if log:
            log.seek(0); said=log.read().decode(errors='replace')
            if said: print(said[-20000:])
server.shutdown()
