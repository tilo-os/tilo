# Historical source-pinned fixture; see README.md.
import importlib.util,json,os,signal,socket,subprocess,threading,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('legacy','/legacy-caret.py');legacy=importlib.util.module_from_spec(spec);spec.loader.exec_module(legacy)
spec=importlib.util.spec_from_file_location('scorer','/speech-score.py');scorer=importlib.util.module_from_spec(spec);spec.loader.exec_module(scorer)
assert os.getuid()==65534 and os.getgid()==65534
assert next(l for l in Path('/proc/self/status').read_text().splitlines() if l.startswith('CapEff:')).split()[1]=='0000000000000000'
runtime=Path('/tmp/gtk-runtime');runtime.mkdir(mode=0o700)
env=dict(os.environ,HOME='/tmp/gtk-home',XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME='/tmp/gtk-config',XDG_DATA_HOME='/model-data',XDG_STATE_HOME='/tmp/gtk-state',XDG_CACHE_HOME='/tmp/gtk-cache',TILO_IDENTITY_ROOT='/tmp/gtk-identity',TILO_AUTHORITY_SOCKET_DIR='/tmp/gtk-authority',TILO_FS_AUTHORITY_CACHE_DIR='/tmp/gtk-fs',TILO_FS_CONTENT_PEER_CACHE_DIR='/tmp/gtk-peer',TILO_TEST_CONTROL=str(runtime/'tilo-session/test-control.sock'),TILO_HEADLESS_SIZE='1600x900',TILO_WORKSPACE_ROOTS='',RUST_LOG='warn')
for key in ['HOME','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','TILO_IDENTITY_ROOT','TILO_AUTHORITY_SOCKET_DIR','TILO_FS_AUTHORITY_CACHE_DIR','TILO_FS_CONTENT_PEER_CACHE_DIR']:Path(env[key]).mkdir(parents=True,exist_ok=True)
case=json.loads(Path('/case.json').read_text())
settings=Path(env['XDG_CONFIG_HOME'])/'tilo/settings.json';settings.parent.mkdir(parents=True);settings.write_text(json.dumps({'accessibility':{'dictation_language':'de'}}))
owned=[];control=legacy.Control(Path(env['TILO_TEST_CONTROL']))
def start(command,name,extra=None):
 f=open('/tmp/'+name+'.log','w');p=subprocess.Popen(command,env=extra or env,stdout=f,stderr=subprocess.STDOUT,start_new_session=True);owned.append((name,p,f));return p
def wait(description,fn,seconds=20):
 end=time.monotonic()+seconds
 while True:
  result=fn()
  if result:return result
  assert time.monotonic()<end,description
  time.sleep(.05)
def nodes():
 p=subprocess.run(['/usr/bin/pw-dump'],env=env,capture_output=True,timeout=5,check=True)
 return [n.get('info',{}).get('props',{}).get('node.name','') for n in json.loads(p.stdout) if n.get('type')=='PipeWire:Interface:Node']
def workers():
 found=[]
 for p in Path('/proc').iterdir():
  if not p.name.isdigit():continue
  try:
   with (p/'cmdline').open('rb') as f:command=f.read(4096).split(b'\0')[0]
  except (FileNotFoundError,ProcessLookupError):continue
  if command==b'/usr/bin/tilo-speech-worker':found.append(int(p.name))
 return found
def chord(key,shift=False):
 control.send({'op':'key_down','key':'Control'})
 if shift:control.send({'op':'key_down','key':'Shift'})
 try:control.send({'op':'key','key':key})
 finally:
  if shift:control.send({'op':'key_up','key':'Shift'})
  control.send({'op':'key_up','key':'Control'})
def copy_all():
 chord('a');chord('c');time.sleep(.2)
 return legacy.clipboard_text(control)
def action(name):return control.send({'op':'actions_invoke','provider':'actions','id':name})
def stop_owned(p):
 if p.poll() is None:os.killpg(p.pid,signal.SIGTERM)
 p.wait(timeout=3)
def prepare():
 action('dictation.toggle')
 wait('missing synthetic-only one-session consent frame',lambda:'tilo.shell.trusted-frame/dictation-microphone' in control.target_names(),25)
 assert control.send({'op':'state_dump'})['state']['chrome']['seat_focus']['text_input_active'],'GTK field did not advertise text-input-v3'
 control.send({'op':'key','key':'ArrowLeft'});control.send({'op':'key','key':'Enter'})
 wait('synthetic recorder not started',lambda:'tilo-dictation-microphone' in nodes(),25)
 w=workers();assert len(w)==1,('production speech worker not observable',w)
 print('GTK_ACTUAL_WORKER_STARTED',json.dumps({'pids':w,'source':'isolated virtual audio; hardware disabled'}),flush=True)
def retired():
 wait('recorder retained after session retirement',lambda:'tilo-dictation-microphone' not in nodes(),15)
 wait('worker retained after session retirement',lambda:not workers(),15)
 wait('dictation frame retained after retirement',lambda:not any('dictation-' in n or 'dictation.live-preview' in n for n in control.target_names()),20)
before=time.monotonic()
try:
 dbus=runtime/'bus'
 start(['/usr/bin/dbus-daemon','--session','--nofork','--address=unix:path='+str(dbus)],'dbus')
 wait('private session DBus not ready',dbus.is_socket)
 env['DBUS_SESSION_BUS_ADDRESS']='unix:path='+str(dbus)
 config=Path(env['XDG_CONFIG_HOME'])/'wireplumber/wireplumber.conf.d';config.mkdir(parents=True)
 (config/'fixture.conf').write_text('wireplumber.profiles = { main = { hardware.audio = disabled hardware.bluetooth = disabled hardware.video-capture = disabled support.dbus = disabled } }')
 start(['/usr/bin/pipewire'],'pipewire');wait('private PipeWire not ready',lambda:(runtime/'pipewire-0').is_socket())
 start(['/usr/bin/wireplumber'],'wireplumber')
 start(['/usr/bin/pw-loopback','-n','TiloPublicAudioFixture','-c','1','-m','MONO','-i','{node.name=TiloPublicAudioSink media.class=Audio/Sink}','-o','{node.name=TiloPublicAudioSource media.class=Audio/Source}'],'loopback')
 wait('virtual speech source absent',lambda:'TiloPublicAudioSource' in nodes())
 assert not any(n.startswith(('alsa_','bluez_')) for n in nodes())
 shell=start(['/rust/debug/shell','--mode','native','--backend','headless'],'shell')
 wait('private compositor control absent',lambda:Path(env['TILO_TEST_CONTROL']).is_socket() or (shell.poll() is not None and (_ for _ in ()).throw(RuntimeError('shell exited before control'))),60)
 legacy.complete_onboarding(control)
 display=runtime/'tilo-session/wayland-display';wait('Wayland display not published',display.exists)
 appenv=dict(env,WAYLAND_DISPLAY=display.read_text().strip(),GDK_BACKEND='wayland',GSK_RENDERER='cairo',NO_AT_BRIDGE='1',XDG_DATA_HOME='/tmp/gtk-app-data',GTK_IM_MODULE='tilo-atomic',GTK_PATH='/gtk-module')
 app=start(['/fixture-client'],'gtk',appenv)
 gtk_case=legacy.Case('libadwaita',('privacy-client',),'so.tilo.DictationPrivacyFixture',(.5,.5),())
 frame=wait('actual GTK privacy client never presented',lambda:legacy.frame_or_process_exit(control,gtk_case,app),45)
 def geometry():
  lines=Path('/tmp/gtk.log').read_text().splitlines()
  if not any(l.startswith('GTK_PRIVACY_ROOT ') for l in lines):return None
  bounds={l.split()[1]:list(map(float,l.split()[2:])) for l in lines if l.startswith('GTK_PRIVACY_BOUNDS ')}
  if len(bounds)!=5:return None
  root=list(map(float,next(l for l in lines if l.startswith('GTK_PRIVACY_ROOT ')).split()[1:]))
  return bounds,root
 bounds,root=wait('actual GTK allocated field bounds unobservable',geometry)
 def focus(name):
  frame=legacy.frame_or_process_exit(control,gtk_case,app);rect=frame['rect'];x,y,w,h=bounds[name]
  control.send({'op':'click_at','x':rect['x']+rect['w']*(x+w*.5)/root[0],'y':rect['y']+rect['h']*(y+h*.5)/root[1]})
  wait('actual GTK field focus not observed: '+name,lambda:[l for l in Path('/tmp/gtk.log').read_text().splitlines() if l.startswith('GTK_PRIVACY_FOCUSED ')][-1:]==['GTK_PRIVACY_FOCUSED '+name])
 focus('plain')
 for key in ['h','i']:control.send({'op':'key','key':key})
 assert wait('ordinary native GTK single-line typing failed with adapter',lambda:copy_all()=='hi')
 print('PASS private GTK adapter preserves ordinary single-line physical key input',flush=True)
 prepare();action('dictation.cancel');retired()
 print('PASS actual GTK plain entry advertises eligible original field and canceled worker is reaped',flush=True)
 for field in ['password','pin']:
  focus(field);reply=action('dictation.toggle');print('GTK_PRIVACY_ACTION',field,json.dumps(reply),flush=True)
  time.sleep(1)
  print('GTK_PRIVACY_AVAILABLE_ACTIONS',field,json.dumps(control.send({'op':'actions_enumerate','query':'dictation','include_programmatic':True})),flush=True)
  print('GTK_PRIVACY_TARGETS',field,json.dumps([n for n in control.target_names() if 'dictation' in n]),flush=True)
  assert 'tilo.shell.trusted-frame/dictation-microphone' not in control.target_names(),'sensitive field offered capture consent'
  assert 'tilo-dictation-microphone' not in nodes() and not workers(),'sensitive field started microphone or worker'
  print('PASS actual GTK '+field+' metadata preserved; dictation refused before recorder or worker startup',flush=True)
  action('dictation.cancel');retired()
 # GTK PRIVATE requests no personalized-data updates, not secret-field refusal.
 # The earlier stricter assertion remains failed in its separate receipt.
 focus('private')
 prepare();action('dictation.cancel');retired()
 assert wait('canceled private-hint field changed',lambda:copy_all()=='private fixture')
 print('PASS actual GTK PRIVATE-hint entry keeps normal one-session consent; cancellation leaves exact original private-hint buffer unchanged',flush=True)
 focus('multiline')
 for key in ['h','i','Enter','o']:control.send({'op':'key','key':key})
 assert wait('ordinary native GTK multiline typing failed with adapter',lambda:copy_all()=='hi\no')
 control.send({'op':'key','key':'z'});assert wait('ordinary selected-range physical key replacement failed',lambda:copy_all()=='z')
 chord('z');assert wait('ONE native GTK undo did not restore ordinary selected-range key replacement',lambda:copy_all()=='hi\no')
 chord('z',shift=True);assert wait('ONE native GTK redo did not restore ordinary key replacement',lambda:copy_all()=='z')
 assert 'TILO_GTK_ATOMIC_COMMIT native text-view transaction completed' in Path('/tmp/gtk.log').read_text(),'adapter native TextView commit path unobserved'
 print('PASS private GTK adapter preserves ordinary multiline keyboard editing and selected-range ONE undo/redo',flush=True)
 print('GTK_PRIVACY_JOURNEY_COMPLETE',json.dumps({'seconds':time.monotonic()-before,'physical_microphone':False,'installed':False,'fields':['plain','password','pin','private','multiline']}),flush=True)

finally:
 if Path(env['TILO_TEST_CONTROL']).is_socket():
  try:
   print('GTK_OWNED_FINAL_STATE',json.dumps(control.send({'op':'state_dump'})),flush=True)
   print('GTK_OWNED_FINAL_CLIPBOARD',json.dumps(legacy.clipboard_text(control)),flush=True)
   print('GTK_OWNED_FINAL_FOREIGN_FRAMES',json.dumps(control.send({'op':'foreign_frames'})),flush=True)
   print('GTK_OWNED_FINAL_TARGETS',json.dumps(control.send({'op':'list_targets'})),flush=True)
  except (OSError,RuntimeError,ValueError) as e:print('GTK_OWNED_DIAGNOSTIC_FAILED',type(e).__name__,str(e),flush=True)
 for name,p,f in reversed(owned):
  try:
   if p.poll() is None:os.killpg(p.pid,signal.SIGTERM)
   p.wait(timeout=3)
  except (ProcessLookupError,subprocess.TimeoutExpired):
   if p.poll() is None:
    try:os.killpg(p.pid,signal.SIGKILL)
    except ProcessLookupError:pass
   p.wait(timeout=3)
  f.close()
 for name in ['gtk','shell','pipewire','wireplumber']:
  p=Path('/tmp/'+name+'.log')
  if p.exists():print('OWNED_LOG',name,p.read_text(errors='replace')[-12000:],flush=True)
