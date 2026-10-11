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
 appenv=dict(env,WAYLAND_DISPLAY=display.read_text().strip(),QT_QPA_PLATFORM='wayland',QT_WAYLAND_TEXT_INPUT_PROTOCOL='zwp_text_input_v3',QT_WAYLAND_DISABLE_WINDOWDECORATION='1',QT_PLUGIN_PATH='/qt-root/usr/lib64/qt6/plugins',LD_LIBRARY_PATH='/qt-root/usr/lib64',XDG_DATA_HOME='/tmp/qt-app-data')
 seed=Path('/tmp/gtk-original.txt');seed.write_text('alpha beta gamma')
 app=start(['/qt-root/usr/bin/featherpad','--standalone',str(seed)],'gtk',appenv)
 gtk_case=legacy.Case('qt6',('featherpad',),'FeatherPad',(.5,.5),('--standalone',))
 frame=wait('actual GNOME Text Editor never presented',lambda:legacy.frame_or_process_exit(control,gtk_case,app),45)
 rect=frame['rect'];control.send({'op':'click_at','x':rect['x']+rect['w']*.5,'y':rect['y']+rect['h']*.5})
 print('GTK_INITIAL_CLIPBOARD_OBSERVATION',json.dumps({'text':copy_all(),'fixture_file':seed.read_text()}),flush=True)
 assert wait('GTK initial buffer not observed',lambda:copy_all()=='alpha beta gamma')
 chord('Home')
 for _ in range(6):control.send({'op':'key','key':'ArrowRight'})
 control.send({'op':'key_down','key':'Shift'})
 try:
  for _ in range(4):control.send({'op':'key','key':'ArrowRight'})
 finally:control.send({'op':'key_up','key':'Shift'})
 chord('c');wait('GTK original selected range unobservable',lambda:legacy.clipboard_text(control)=='beta')
 print('PASS actual GTK original buffer and selected range observed before dictation',flush=True)
 prepare();started=time.monotonic();player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'player')
 preview=False
 while player.poll() is None:
  if 'dictation.live-preview' in control.target_names():preview=True
  assert time.monotonic()-started<25,'public playback exceeded bound';time.sleep(.1)
 assert player.returncode==0,'public playback failed'
 assert preview,'production streamed preview not observed'
 action('dictation.finish');retired()
 assert legacy.clipboard_text(control)=='beta','dictation changed the private test clipboard'
 result=copy_all();assert isinstance(result,str) and result.startswith('alpha ') and result.endswith(' gamma'),result
 transcript=result[6:-6];ref=scorer.units(case['reference']);errors=scorer.distance(ref,scorer.units(transcript));assert errors/len(ref)<=.20,(transcript,errors,len(ref))
 print('GTK_ACTUAL_FINAL',json.dumps({'text':transcript,'buffer':result,'word_errors':errors,'reference_words':len(ref),'wer':errors/len(ref),'seconds':time.monotonic()-started}),flush=True)
 chord('z');wait('GTK native undo did not restore original range',lambda:copy_all()=='alpha beta gamma')
 chord('z',shift=True);wait('GTK native redo did not restore one final',lambda:copy_all()==result)
 print('PASS actual GTK selected-range replacement and ONE native undo/redo with native Qt transaction',flush=True)
 chord('End');prepare();player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'cancel-player');time.sleep(4.5);assert player.poll() is None
 action('dictation.cancel');retired();stop_owned(player);assert copy_all()==result
 print('PASS actual GTK canceled active speech leaves original buffer unchanged and worker reaped',flush=True)
 # A repeated session must use fresh admission. Move to a second real GTK
 # window, then return to the original, without letting a late result insert.
 old_targets={t['name'] for t in control.send({'op':'list_targets'})['targets'] if t['name'].startswith('foreign-surface-') and '[' not in t['name']}
 assert len(old_targets)==1,old_targets
 original_name=next(iter(old_targets))
 chord('End');prepare();player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'focus-player');time.sleep(2);assert player.poll() is None
 second_file=Path('/tmp/gtk-second.txt');second_file.write_text('second')
 second=start(['/qt-root/usr/bin/featherpad','--standalone',str(second_file)],'gtk-second',appenv)
 fresh=wait('second actual GTK window never mapped',lambda:next((t for t in control.send({'op':'list_targets'})['targets'] if t['name'].startswith('foreign-surface-') and '[' not in t['name'] and t['name'] not in old_targets),None),30)
 fresh_id=int(fresh['name'].removeprefix('foreign-surface-'))
 wait('second GTK root mapped but did not present',lambda:any(f.get('surface_id')==fresh_id and f.get('app_id')=='FeatherPad' and f.get('presented') and f.get('frame_seq',0)>0 and not f.get('opening') for f in control.send({'op':'foreign_frames'})['foreign_frames']['frames']),30)
 fresh=next(t for t in control.send({'op':'list_targets'})['targets'] if t['name']==fresh['name'])
 x,y,w,h=fresh['rect'];control.send({'op':'click_at','x':x+w*.5,'y':y+h*.5})
 wait('second GTK window did not obtain keyboard focus',lambda:(control.send({'op':'state_dump'})['state']['chrome']['seat_focus']['keyboard'] or {}).get('surface')==fresh_id)
 retired();stop_owned(player);assert wait('second GTK original buffer not observed',lambda:copy_all()=='second')
 original=next(t for t in control.send({'op':'list_targets'})['targets'] if t['name']==original_name)
 x,y,w,h=original['rect'];control.send({'op':'click_at','x':x+w*.5,'y':y+h*.5})
 original_id=int(original_name.removeprefix('foreign-surface-'))
 wait('original GTK window did not regain keyboard focus',lambda:(control.send({'op':'state_dump'})['state']['chrome']['seat_focus']['keyboard'] or {}).get('surface')==original_id)
 assert wait('original GTK buffer changed across focus retirement',lambda:copy_all()==result)
 print('PASS repeated GTK session and focus-away/back permanently retire worker without late insertion',flush=True)
 print('FEATHERPAD_NATIVE_JOURNEY_COMPLETE',json.dumps({'seconds':time.monotonic()-before,'microphone':False,'installed':False,'app':os.environ['TILO_QT_EDITOR_KIND'],'model':'Small Q5_1','language':'de'}),flush=True)
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
