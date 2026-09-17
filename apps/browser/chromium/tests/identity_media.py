#!/usr/bin/env python3
"""Native favicon, navigation-scoped media state and mute round-trip fixture."""
import atexit,base64,http.server,io,json,os,queue,struct,subprocess,sys,tempfile,threading,time,wave

audio=io.BytesIO()
with wave.open(audio,"wb") as output:
    output.setnchannels(1);output.setsampwidth(1);output.setframerate(8000);output.writeframes(bytes([128,160,128,96])*8000)
WAV=base64.b64encode(audio.getvalue()).decode()
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path=="/icon.png":
            body=base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            self.send_response(200);self.send_header("Content-Type","image/png")
        elif self.path=="/frame":
            body=("<button id=p onclick='a.play()'>play</button><button id=s onclick='a.pause()'>pause</button><audio id=a loop src='data:audio/wav;base64,"+WAV+"'></audio>").encode()
            self.send_response(200);self.send_header("Content-Type","text/html")
        elif self.path=="/slow.wav":
            time.sleep(1.0);body=base64.b64decode(WAV)
            self.send_response(200);self.send_header("Content-Type","audio/wav")
        elif self.path=="/slow":
            body=b"<button id=p onclick=\"document.title='slow clicked';a.play().catch(e=>document.title='slow failed')\">play</button><button id=s onclick='a.pause()'>pause</button><audio id=a preload=none src='/slow.wav'></audio>"
            self.send_response(200);self.send_header("Content-Type","text/html")
        elif self.path.startswith("/empty"):
            body=b"<!doctype html><title>frame</title>"
            self.send_response(200);self.send_header("Content-Type","text/html")
        elif self.path=="/many":
            body=("<!doctype html><title>many frames</title>"+"".join(f"<iframe src='/empty?{i}'></iframe>" for i in range(80))).encode()
            self.send_response(200);self.send_header("Content-Type","text/html")
        else:
            body=("<link rel='icon' type='image/png' href='/icon.png'><title>media fixture</title><button id=p onclick='a.play()'>play</button><button id=s onclick='a.pause()'>pause</button><audio id=a loop src='data:audio/wav;base64,"+WAV+"'></audio><iframe src='/frame' style='position:absolute;left:0;top:50px;width:200px;height:80px;border:0'></iframe>").encode()
            self.send_response(200);self.send_header("Content-Type","text/html")
        self.send_header("Content-Length",str(len(body)));self.end_headers();self.wfile.write(body)
    def log_message(self,*_):pass
server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Page);threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix="qos-identity-") as profile:
    p=subprocess.Popen([sys.argv[1],"--profile="+profile],stdin=subprocess.PIPE,stdout=subprocess.PIPE)
    def cleanup():
        if p.poll() is None:
            p.terminate()
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait(timeout=5)
        server.shutdown()
    atexit.register(cleanup)
    q=queue.Queue()
    def exact(n):
        out=b""
        while len(out)<n:
            chunk=p.stdout.read(n-len(out))
            if not chunk: raise EOFError("native helper closed its output")
            out+=chunk
        return out
    def reader():
        try:
            while True:
                n,=struct.unpack("<I",exact(4));data=exact(n)
                if data[0]==1:
                    event=json.loads(data[1:])
                    if os.environ.get("QOS_IDENTITY_DEBUG"):print(event,flush=True)
                    q.put(event)
        except Exception as error:q.put({"event":"reader_error","message":str(error)})
    threading.Thread(target=reader,daemon=True).start()
    def send(**value):
        data=b"\1"+json.dumps(value).encode();p.stdin.write(struct.pack("<I",len(data))+data);p.stdin.flush()
    def until(test):
        end=time.monotonic()+45
        while time.monotonic()<end:
            event=q.get(timeout=end-time.monotonic())
            if event["event"]=="reader_error":raise AssertionError(event["message"])
            if test(event):return event
        raise AssertionError("event timeout")
    base=f"http://127.0.0.1:{server.server_port}"
    until(lambda e:e["event"]=="ready");send(op="resize",width=640,height=480,scale_factor=1);send(op="navigate",url=base+"/")
    loaded=False;favicon=None
    while not loaded or favicon is None:
        event=q.get(timeout=45)
        loaded |= event["event"]=="loaded" and event.get("url")==base+"/"
        if event["event"]=="favicon":favicon=event
    assert favicon["page_url"]==base+"/" and favicon["width"]<=64 and favicon["height"]<=64
    send(op="mouse_button",button="left",down=True,x=25,y=12);send(op="mouse_button",button="left",down=False,x=25,y=12)
    playing=until(lambda e:e["event"]=="media_state" and e["playing"])
    assert playing["document_generation"]>0
    send(op="audio_muted",muted=True);assert until(lambda e:e["event"]=="audio_muted")["muted"]
    send(op="audio_muted",muted=False);assert not until(lambda e:e["event"]=="audio_muted")["muted"]
    send(op="mouse_button",button="left",down=True,x=75,y=12);send(op="mouse_button",button="left",down=False,x=75,y=12)
    until(lambda e:e["event"]=="media_state" and not e["playing"])
    send(op="mouse_button",button="left",down=True,x=25,y=62);send(op="mouse_button",button="left",down=False,x=25,y=62)
    iframe_playing=until(lambda e:e["event"]=="media_state" and e["playing"])
    send(op="mouse_button",button="left",down=True,x=75,y=62);send(op="mouse_button",button="left",down=False,x=75,y=62)
    until(lambda e:e["event"]=="media_state" and not e["playing"] and e["document_generation"]==iframe_playing["document_generation"])
    send(op="mouse_button",button="left",down=True,x=25,y=12);send(op="mouse_button",button="left",down=False,x=25,y=12)
    before_reload=until(lambda e:e["event"]=="media_state" and e["playing"])
    send(op="navigate",url=base+"/")
    reset=until(lambda e:e["event"]=="media_state" and not e["playing"] and e["document_generation"]>before_reload["document_generation"])
    assert reset["page_url"]==base+"/"
    send(op="navigate",url=base+"/slow");until(lambda e:e["event"]=="loaded" and e.get("url")==base+"/slow")
    time.sleep(0.2)
    send(op="mouse_button",button="left",down=True,x=25,y=12);send(op="mouse_button",button="left",down=False,x=25,y=12)
    until(lambda e:e["event"]=="title" and e["title"]=="slow clicked")
    slow_playing=until(lambda e:e["event"]=="media_state" and e["playing"] and e["page_url"]==base+"/slow")
    send(op="mouse_button",button="left",down=True,x=75,y=12);send(op="mouse_button",button="left",down=False,x=75,y=12)
    until(lambda e:e["event"]=="media_state" and not e["playing"] and e["document_generation"]==slow_playing["document_generation"])
    send(op="navigate",url=base+"/many")
    loaded_many=False;frame_limit=None
    while not loaded_many or frame_limit is None:
        event=q.get(timeout=45)
        loaded_many |= event["event"]=="loaded" and event.get("url")==base+"/many"
        if event["event"]=="media_frame_limit":frame_limit=event
    assert frame_limit["tracked_frames"]==64
    send(op="navigate",url=base+"/");until(lambda e:e["event"]=="loaded" and e.get("url")==base+"/")
    time.sleep(0.2)
    send(op="mouse_button",button="left",down=True,x=25,y=12);send(op="mouse_button",button="left",down=False,x=25,y=12)
    until(lambda e:e["event"]=="media_state" and e["playing"] and e["page_url"]==base+"/")
    send(op="mouse_button",button="left",down=True,x=75,y=12);send(op="mouse_button",button="left",down=False,x=75,y=12)
    until(lambda e:e["event"]=="media_state" and not e["playing"] and e["page_url"]==base+"/")
    send(op="navigate",url=base+"/next")
    until(lambda e:e["event"]=="uri" and e["url"].endswith("/next"))
    until(lambda e:e["event"]=="media_state" and not e["playing"] and e["page_url"].endswith("/next"))
    # Cross-site replacement can overlap old/new CEF browser handles sharing
    # a numeric ID. The replacement's iframe must still publish context-close.
    cross_site=f"http://localhost:{server.server_port}/"
    send(op="navigate",url=cross_site)
    until(lambda e:e["event"]=="loaded" and e.get("url")==cross_site)
    time.sleep(0.2)
    send(op="mouse_button",button="left",down=True,x=25,y=62)
    send(op="mouse_button",button="left",down=False,x=25,y=62)
    cross_playing=until(lambda e:e["event"]=="media_state" and e["playing"] and e["page_url"]==cross_site)
    send(op="scripts",scripts=["document.querySelector('iframe').remove()"])
    until(lambda e:e["event"]=="media_state" and not e["playing"] and e["document_generation"]==cross_playing["document_generation"])
    send(op="shutdown");p.wait(timeout=15);print("PASS: bounded favicon/frame state, preload-none playback, lifecycle reset, media start/pause and native mute round-trip")
server.shutdown();atexit.unregister(cleanup)
