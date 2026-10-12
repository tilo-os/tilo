import http.server,json,os,socket,subprocess,threading,time
from pathlib import Path
runtime=Path('/tmp/assembled-runtime');runtime.mkdir(mode=0o700)
env=dict(os.environ,HOME='/tmp/assembled-home',XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME='/tmp/assembled-config',XDG_DATA_HOME='/model-data',XDG_STATE_HOME='/tmp/assembled-state',XDG_CACHE_HOME='/tmp/assembled-cache',TILO_IDENTITY_ROOT='/tmp/assembled-identity',TILO_AUTHORITY_SOCKET_DIR='/tmp/assembled-authority',TILO_FS_AUTHORITY_CACHE_DIR='/tmp/assembled-fs',TILO_FS_CONTENT_PEER_CACHE_DIR='/tmp/assembled-peer',TILO_TEST_CONTROL=str(runtime/'tilo-session/test-control.sock'),WGPU_BACKEND='vulkan',PATH='/usr/bin')
for key in ('HOME','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','TILO_IDENTITY_ROOT','TILO_AUTHORITY_SOCKET_DIR','TILO_FS_AUTHORITY_CACHE_DIR','TILO_FS_CONTENT_PEER_CACHE_DIR'):Path(env[key]).mkdir(mode=0o700)
case=json.loads(Path('/case.json').read_text()); settings=Path(env['XDG_CONFIG_HOME'])/'tilo/settings.json'; settings.parent.mkdir(parents=True,exist_ok=True); settings.write_text(json.dumps({'accessibility':{'dictation_language':case['language']}}))
assert os.getuid()==65534 and os.getgid()==65534
status=Path('/proc/self/status').read_text(); assert next(l for l in status.splitlines() if l.startswith('CapEff:')).split()[1]=='0000000000000000','fixture retained effective capabilities'
print('RUNTIME_IMAGE_METADATA',json.dumps({'uid':os.getuid(),'gid':os.getgid(),'uid_map':Path('/proc/self/uid_map').read_text(),'parents':[{'path':str(p),'uid':p.stat().st_uid,'mode':oct(p.stat().st_mode & 0o7777)} for p in [Path('/usr/lib64/tilo-speech/whistle/x86-64-v3'),Path('/usr/lib64/tilo-speech/whistle'),Path('/usr/lib64/tilo-speech'),Path('/usr/lib64'),Path('/usr'),Path('/')]]}),flush=True)
for p in [Path('/usr/share/tilo/system-extensions/local-model-catalog.json'),Path('/usr/share/tilo/system-extensions/local-model-keys.json'),Path('/model-data'),Path('/model-data/tilo'),Path('/model-data/tilo/models'),Path('/model-data/tilo/models/state.json')]:
 try:
  info=p.stat();data=p.read_bytes() if p.is_file() else b'';print('FIXTURE_PATH_PREFLIGHT',json.dumps({'path':str(p),'uid':info.st_uid,'mode':oct(info.st_mode & 0o777),'readable_bytes':len(data)}),flush=True)
 except OSError as e:print('FIXTURE_PATH_PREFLIGHT',json.dumps({'path':str(p),'error':e.errno}),flush=True)
flags=set(next(l for l in Path('/proc/cpuinfo').read_text().splitlines() if l.startswith('flags')).split(':',1)[1].split());print('IMAGE_CPU_PREFLIGHT',json.dumps(sorted(flags.intersection({'avx2','avx','fma','bmi1','bmi2','abm','sse3','pni','ssse3','sse4_1','sse4_2','f16c','movbe','popcnt','xsave'}))),flush=True)
import select,struct
payload=json.dumps({'language':case['language']}).encode();body=bytes([0])+(1).to_bytes(8,'little')+payload
probe=subprocess.Popen(['/usr/bin/tilo-speech-worker'],env={'XDG_DATA_HOME':'/model-data','HOME':env['HOME']},stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
try:
 probe.stdin.write(len(body).to_bytes(4,'little')+body);probe.stdin.flush();assert select.select([probe.stdout],[],[],15)[0],'fixed worker did not acknowledge preflight'
 header=probe.stdout.read(4);assert len(header)==4,'worker preflight header';length=int.from_bytes(header,'little');assert 0<length<=65545;reply=json.loads(probe.stdout.read(length));print('FIXED_WORKER_PREFLIGHT',json.dumps(reply),flush=True);assert reply['kind']=='ready',reply
finally:
 print('FIXED_WORKER_OBSERVABILITY',json.dumps({'pid':probe.pid,'argv0':Path(f'/proc/{probe.pid}/cmdline').read_bytes().split(b'\0')[0].decode()}),flush=True)
 if probe.poll() is None:probe.kill()
 probe.wait(timeout=5);probe.stdin.close();probe.stdout.close();probe.stderr.close()
journey_start=time.monotonic();loaded=threading.Event(); observation={}
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  body=b'<style>body{color:black;background:white}input{display:block}</style><style>body{margin:50px}input{width:800px;height:60px}</style><input id="first" autofocus value="alpha beta gamma"><input id="second" value="second"><script>let lastpointer="unseen",lastkey="unseen";addEventListener("keydown",e=>lastkey=e.key+",ctrl="+e.ctrlKey+",shift="+e.shiftKey+",trusted="+e.isTrusted);addEventListener("pointerdown",e=>lastpointer=e.clientX+","+e.clientY+","+e.isTrusted+","+e.target.id+",ctrl="+e.ctrlKey+",shift="+e.shiftKey+",button="+e.button+",buttons="+e.buttons);first.addEventListener("mouseup",()=>first.setSelectionRange(6,10));fetch("/loaded");setInterval(()=>fetch("/observe?focus="+document.hasFocus()+"&active="+document.activeElement.id+"&width="+innerWidth+"&height="+innerHeight+"&second="+encodeURIComponent(second.value)+"&value="+encodeURIComponent(first.value)+"&key="+encodeURIComponent(lastkey)+"&pointer="+encodeURIComponent(lastpointer)+"&start="+first.selectionStart+"&end="+first.selectionEnd+"&ratio="+devicePixelRatio),100);</script>'
  self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers()
  if self.path=='/loaded':loaded.set();body=b'ok'
  if self.path.startswith('/observe?'):
   from urllib.parse import urlsplit,parse_qs
   observation.update(parse_qs(urlsplit(self.path).query));body=b'ok'
  self.wfile.write(body)
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
owned=[]
def start(args,name,extra=None):
 f=open('/tmp/'+name+'.log','w');p=subprocess.Popen(args,env=extra or env,stdout=f,stderr=subprocess.STDOUT,start_new_session=True);owned.append((p,f));return p
def call(payload):
 with socket.socket(socket.AF_UNIX) as s:
  s.settimeout(10);s.connect(env['TILO_TEST_CONTROL']);s.sendall(json.dumps(payload).encode()+b'\n');data=b''
  while b'\n' not in data:
   chunk=s.recv(65536)
   if not chunk or len(data)+len(chunk)>1048576:raise RuntimeError('missing or unbounded owned control reply')
   data+=chunk
  reply=json.loads(data.split(b'\n')[0]);assert 'err' not in reply,reply;return reply
def active_workers():
 result=[]
 for candidate in Path('/proc').iterdir():
  if not candidate.name.isdigit():continue
  try:
   with (candidate/'cmdline').open('rb') as f:command=f.read(4096)
  except (FileNotFoundError,ProcessLookupError):continue
  if command.split(b'\0')[0]==b'/usr/bin/tilo-speech-worker':result.append(int(candidate.name))
 return result
try:
 config=Path(env['XDG_CONFIG_HOME'])/'wireplumber/wireplumber.conf.d';config.mkdir(parents=True)
 (config/'fixture.conf').write_text('wireplumber.profiles = { main = { hardware.audio = disabled hardware.bluetooth = disabled hardware.video-capture = disabled support.dbus = disabled } }')
 start(['/usr/bin/pipewire'],'pipewire')
 deadline=time.monotonic()+10
 while not (runtime/'pipewire-0').is_socket():
  assert time.monotonic()<deadline,'private PipeWire socket missing';time.sleep(.05)
 start(['/usr/bin/wireplumber'],'wireplumber')
 start(['/usr/bin/pw-loopback','-n','TiloPublicAudioFixture','-c','1','-m','MONO','-i','{node.name=TiloPublicAudioSink media.class=Audio/Sink}','-o','{node.name=TiloPublicAudioSource media.class=Audio/Source}'],'loopback')
 def nodes():
  result=subprocess.run(['/usr/bin/pw-dump'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=5,check=True)
  return [o.get('info',{}).get('props',{}).get('node.name','') for o in json.loads(result.stdout) if o.get('type')=='PipeWire:Interface:Node']
 deadline=time.monotonic()+10
 while 'TiloPublicAudioSource' not in nodes():
  assert time.monotonic()<deadline,'private virtual source missing';time.sleep(.1)
 assert not any(n.startswith(('alsa_','bluez_')) for n in nodes()),'hardware node in private graph'
 print('PASS isolated virtual audio graph with hardware disabled',flush=True)
 shell=start(['/rust/debug/shell','--backend','headless'],'shell')
 deadline=time.monotonic()+60
 while not Path(env['TILO_TEST_CONTROL']).is_socket():
  assert shell.poll() is None,Path('/tmp/shell.log').read_text();assert time.monotonic()<deadline,'control socket timeout';time.sleep(.1)
 ways=list(runtime.glob('wayland-*'));ways=[p for p in ways if p.is_socket()];assert len(ways)==1,ways
 clientenv=dict(env,WAYLAND_DISPLAY=ways[0].name)
 browser=start(['/rust/debug/examples/browser-text-input-qualification','--native-fixture','/fixture-cef/tilo-chromium-host','/tmp/assembled-browser-profile',f'http://127.0.0.1:{server.server_port}/input'],'browser',clientenv)
 assert loaded.wait(30),Path('/tmp/browser.log').read_text()
 print('PASS assembled current headless compositor and sandboxed native Chromium loopback page',flush=True)
 call({'op':'watch','watching':True})
 print('TARGETS',json.dumps(call({'op':'list_targets'})),flush=True)
 print('STATE',json.dumps(call({'op':'state_dump'})),flush=True)
 deadline=time.monotonic()+10
 target=None
 while target is None:
  target=next((t for t in call({'op':'list_targets'})['targets'] if t['name'].startswith('foreign-surface-')),None)
  assert target is not None or time.monotonic()<deadline,'native fixture never mapped its current surface'
  if target is None:time.sleep(.05)
 x,y,w,h=target['rect'];call({'op':'click_at','x':x+100,'y':y+80})
 deadline=time.monotonic()+10
 while call({'op':'state_dump'})['state']['chrome']['seat_focus']['keyboard'] is None:
  assert time.monotonic()<deadline,('native browser text input did not enable after field focus',call({'op':'state_dump'})['state']['chrome']['seat_focus'],observation);time.sleep(.05)
 deadline=time.monotonic()+10
 while observation.get('start')!=['6'] or observation.get('end')!=['10']:
  assert time.monotonic()<deadline,('original DOM range was not selected',observation)
  if observation.get('pointer')==['unseen']:
   call({'op':'click_at','x':x+100,'y':y+80});time.sleep(.2)
  else:time.sleep(.05)
 print('PASS native browser Wayland, DOM focus and selected original range before guarded admission',flush=True)
 time.sleep(.5)
 print('DICTATION',json.dumps(call({'op':'actions_invoke','provider':'actions','id':'dictation.toggle'})),flush=True)
 time.sleep(2)
 targets=call({'op':'list_targets'}); print('PREPARE_TARGETS',json.dumps(targets),flush=True)
 actions=call({'op':'actions_enumerate','query':'dictation','include_programmatic':True});print('CONSENT_ACTIONS',json.dumps(actions),flush=True)
 assert any(t['name']=='tilo.shell.trusted-frame/dictation-microphone' for t in targets['targets']),'trusted preparation frame missing'
 assert call({'op':'state_dump'})['state']['chrome']['seat_focus']['text_input_active'],'guarded preparation did not enable standard text input'
 print('PASS actual original-field Prepare ACK before synthetic consent',flush=True)
 call({'op':'key','key':'ArrowLeft'})
 call({'op':'key','key':'Enter'})
 deadline=time.monotonic()+20
 while 'tilo-dictation-microphone' not in nodes():
  assert time.monotonic()<deadline,('actual pinned virtual recorder did not start',call({'op':'list_targets'}));time.sleep(.1)
 workers=active_workers();assert len(workers)==1,('actual speech worker not observable',workers); print('OWNED_WORKER_STARTED',json.dumps({'pids':workers}),flush=True)
 print('PASS actual worker Start ACK and capture from isolated virtual source',flush=True)
 capture_ready=time.monotonic(); audio_started=time.monotonic(); preview_seen=None
 player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'public-player')
 while player.poll() is None:
  if preview_seen is None and any('dictation.live-preview' in t['name'] for t in call({'op':'list_targets'})['targets']):
   preview_seen=time.monotonic(); print('ACTUAL_PREVIEW',json.dumps({'case':case['id'],'seconds_from_audio_start':preview_seen-audio_started,'original_field_unchanged':observation.get('value')==['alpha beta gamma']}),flush=True); assert observation.get('value')==['alpha beta gamma']
  assert time.monotonic()-audio_started<25,'public playback exceeded bound';time.sleep(.05)
 assert player.returncode==0
 assert preview_seen is not None,'no actual visible dictation preview observed before playback completed'
 finish_requested=time.monotonic(); call({'op':'actions_invoke','provider':'actions','id':'dictation.finish'})
 deadline=time.monotonic()+30
 while observation.get('value',['alpha beta gamma'])[0]=='alpha beta gamma':
  assert time.monotonic()<deadline,('public final not inserted',observation);time.sleep(.1)
 assert 'tilo-dictation-microphone' not in nodes(),'capture retained after finish'
 assert observation['value'][0].startswith('alpha ') and observation['value'][0].endswith(' gamma'),observation
 assert observation.get('second')==['second'],observation
 print('ACTUAL_FINAL',json.dumps({'value':observation['value'][0],'elapsed_seconds':round(time.monotonic()-journey_start,3),'case':case['id'],'requested_language':case['language'],'seconds_from_audio_start':time.monotonic()-audio_started,'seconds_after_finish':time.monotonic()-finish_requested}),flush=True)
 print('PASS assembled public speech capture, private final receipt and selected-range insertion into original browser field',flush=True)
 final=observation['value'][0]
 def wait_for(description,predicate,seconds=10):
  deadline=time.monotonic()+seconds
  while not predicate():
   assert time.monotonic()<deadline,(description,observation);time.sleep(.05)
 def chord(key,shift=False):
  call({'op':'key_down','key':'Control'})
  if shift:call({'op':'key_down','key':'Shift'})
  call({'op':'key','key':key})
  if shift:call({'op':'key_up','key':'Shift'})
  call({'op':'key_up','key':'Control'})
 wait_for('dictation final acknowledgement did not retire trusted frame',lambda:not any('dictation-' in t['name'] for t in call({'op':'list_targets'})['targets']),15)
 print('POST_FINAL_STATE',json.dumps(call({'op':'state_dump'})['state']['chrome']),flush=True)
 print('HISTORY_ACTIONS',json.dumps(call({'op':'actions_enumerate','query':'undo','include_programmatic':True})),flush=True)
 chord('z');wait_for('native Undo did not restore original field',lambda:observation.get('value')==['alpha beta gamma'])
 chord('z',True);wait_for('native Redo did not restore dictated field',lambda:observation.get('value')==[final])
 print('PASS original browser native Undo/Redo through actual keyboard route',flush=True)
 def repeat():
  time.sleep(1.0);call({'op':'actions_invoke','provider':'actions','id':'dictation.toggle'})
  wait_for('fresh one-session consent frame not prepared',lambda:any(t['name']=='tilo.shell.trusted-frame/dictation-microphone' for t in call({'op':'list_targets'})['targets']),20)
  call({'op':'key','key':'ArrowLeft'});call({'op':'key','key':'Enter'})
  wait_for('repeat recorder did not start',lambda:'tilo-dictation-microphone' in nodes(),20)
  workers=active_workers();assert len(workers)==1,('repeated worker not observable',workers);print('OWNED_WORKER_STARTED',json.dumps({'pids':workers}),flush=True)
 repeat();cancel_player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'active-cancel-player');time.sleep(4.5);assert cancel_player.poll() is None;assert observation.get('value')==[final] and observation.get('second')==['second'];call({'op':'actions_invoke','provider':'actions','id':'dictation.cancel'})
 wait_for('canceled recorder still present',lambda:'tilo-dictation-microphone' not in nodes())
 assert observation.get('value')==[final],observation
 print('PASS repeated session Cancel removes recorder and leaves prior field unchanged',flush=True)
 def no_worker():
  return not active_workers()
 wait_for('canceled model worker did not terminate',no_worker)
 print('PASS canceled session model worker also terminated before successor request',flush=True)
 if cancel_player.poll() is None:
  import signal;os.killpg(cancel_player.pid,signal.SIGTERM);cancel_player.wait(timeout=5)

 repeat();print('PASS restart after observed canceled worker exit',flush=True);active_player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'active-focus-player');time.sleep(2);assert active_player.poll() is None,'public speech player must still be streaming before field focus moves';print('PASS active public speech stimulus before successor field focus',flush=True);fresh=next(t for t in call({'op':'list_targets'})['targets'] if t['name'].startswith('foreign-surface-'));fx,fy,fw,fh=fresh['rect'];print('POINTER_RECT',json.dumps({'old':[x,y,w,h],'fresh':fresh['rect']}),flush=True);call({'op':'click_at','x':fx+100,'y':fy+150})
 wait_for('focus change did not reach successor field',lambda:observation.get('active')==['second'])
 wait_for('focus change retained original recorder',lambda:'tilo-dictation-microphone' not in nodes())
 assert observation.get('value')==[final] and observation.get('second')==['second'],observation
 wait_for('retired focus worker did not terminate',no_worker)
 call({'op':'click_at','x':fx+100,'y':fy+80})
 wait_for('original field did not regain focus',lambda:observation.get('active')==['first'])
 time.sleep(1)
 assert observation.get('value')==[final] and observation.get('second')==['second'],observation
 assert not any('dictation.live-preview' in t['name'] for t in call({'op':'list_targets'})['targets']),'retired private preview remained observable'
 print('PASS focus away/back retires recorder and worker permanently with unchanged fields',flush=True)
 print('BEHAVIOR_COMPLETE',json.dumps({'case':case['id'],'elapsed_seconds':round(time.monotonic()-journey_start,3),'final':final}),flush=True)
 for token in case['required_words']: assert token.casefold() in observation['value'][0].casefold(),(case['id'],token,observation)
 print('JOURNEY_COMPLETE',json.dumps({'elapsed_seconds':round(time.monotonic()-journey_start,3),'final':final,'source':'virtual-public-audio','microphone':False}),flush=True)


finally:
 server.shutdown();server.server_close()
 for p,f in reversed(owned):
  if p.poll() is None:
   import signal
   os.killpg(p.pid,signal.SIGTERM)
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait(timeout=5)
  f.close()
 for name in ('shell','browser','pipewire','wireplumber','loopback'):
  path=Path('/tmp/'+name+'.log')
  if path.exists():print(name.upper()+'_LOG',path.read_text()[-12000:],flush=True)
