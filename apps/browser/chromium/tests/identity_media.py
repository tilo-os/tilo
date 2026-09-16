#!/usr/bin/env python3
"""Native favicon, navigation-scoped media state and mute round-trip fixture."""
import base64,http.server,io,json,os,queue,struct,subprocess,sys,tempfile,threading,time,wave

audio=io.BytesIO()
with wave.open(audio,"wb") as output:
    output.setnchannels(1);output.setsampwidth(1);output.setframerate(8000);output.writeframes(bytes([128,160,128,96])*8000)
WAV=base64.b64encode(audio.getvalue()).decode()
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path=="/icon.png":
            body=base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            self.send_response(200);self.send_header("Content-Type","image/png")
        else:
            body=("<link rel='icon' type='image/png' href='/icon.png'><title>media fixture</title><button id=p onclick='a.play()'>play</button><button id=s onclick='a.pause()'>pause</button><audio id=a loop src='data:audio/wav;base64,"+WAV+"'></audio>").encode()
            self.send_response(200);self.send_header("Content-Type","text/html")
        self.send_header("Content-Length",str(len(body)));self.end_headers();self.wfile.write(body)
    def log_message(self,*_):pass
server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Page);threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix="qos-identity-") as profile:
    p=subprocess.Popen([sys.argv[1],"--profile="+profile],stdin=subprocess.PIPE,stdout=subprocess.PIPE)
    q=queue.Queue()
    def exact(n):
        out=b""
        while len(out)<n: out+=p.stdout.read(n-len(out))
        return out
    def reader():
        while True:
            n,=struct.unpack("<I",exact(4));data=exact(n)
            if data[0]==1:
                event=json.loads(data[1:])
                if os.environ.get("QOS_IDENTITY_DEBUG"):print(event,flush=True)
                q.put(event)
    threading.Thread(target=reader,daemon=True).start()
    def send(**value):
        data=b"\1"+json.dumps(value).encode();p.stdin.write(struct.pack("<I",len(data))+data);p.stdin.flush()
    def until(test):
        end=time.monotonic()+45
        while time.monotonic()<end:
            event=q.get(timeout=end-time.monotonic())
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
    until(lambda e:e["event"]=="media_state" and e["playing"])
    send(op="audio_muted",muted=True);assert until(lambda e:e["event"]=="audio_muted")["muted"]
    send(op="audio_muted",muted=False);assert not until(lambda e:e["event"]=="audio_muted")["muted"]
    send(op="mouse_button",button="left",down=True,x=75,y=12);send(op="mouse_button",button="left",down=False,x=75,y=12)
    until(lambda e:e["event"]=="media_state" and not e["playing"])
    send(op="navigate",url=base+"/next");until(lambda e:e["event"]=="uri" and e["url"].endswith("/next"))
    send(op="shutdown");p.wait(timeout=15);print("PASS: bounded favicon, media start/pause, native mute round-trip and navigation scope")
server.shutdown()
