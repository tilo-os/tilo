import importlib.util,json,os,signal,socket,subprocess,time
from pathlib import Path
assert os.getuid()==65534 and os.getgid()==65534
os.sched_setaffinity(0,sorted(os.sched_getaffinity(0))[:2])
assert len(os.sched_getaffinity(0))==2
assert next(l for l in Path('/proc/self/status').read_text().splitlines() if l.startswith('CapEff:')).split()[1]=='0000000000000000'
assert not Path('/dev/snd').exists() and not Path('/dev/dri').exists()
spec=importlib.util.spec_from_file_location('legacy','/source/scripts/legacy-app-caret-e2e.py');legacy=importlib.util.module_from_spec(spec);spec.loader.exec_module(legacy)
root=Path('/tmp/mounted');root.mkdir(mode=0o700);runtime=root/'runtime';runtime.mkdir(mode=0o700)
env=dict(os.environ,PATH='/usr/bin:/bin',HOME=str(root/'home'),USER='ordinary',LANG='C.UTF-8',XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME='/model-data',XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),TILO_IDENTITY_ROOT=str(root/'identity'),TILO_AUTHORITY_SOCKET_DIR=str(root/'authority'),TILO_FS_AUTHORITY_CACHE_DIR=str(root/'fs-authority'),TILO_FS_CONTENT_PEER_CACHE_DIR=str(root/'fs-peers'),TILO_TEST_CONTROL=str(runtime/'tilo-session/test-control.sock'),TILO_HEADLESS_SIZE='1600x900',TILO_WORKSPACE_ROOTS='',TILO_USH_HOME=str(root/'ush'),TILO_USH_IMAGES=str(root/'images'),TILO_USH_BIN='/rust/debug/tilo-ush',TILO_STATE_PATH=str(root/'config/tilo/state.json'),TILO_IDENTITY_FILE_SECRETS='1',RUST_LOG='warn',LP_NUM_THREADS='2',RAYON_NUM_THREADS='2',WGPU_BACKEND='vulkan')
for k in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','TILO_IDENTITY_ROOT','TILO_AUTHORITY_SOCKET_DIR','TILO_FS_AUTHORITY_CACHE_DIR','TILO_FS_CONTENT_PEER_CACHE_DIR']:Path(env[k]).mkdir(parents=True,exist_ok=True)
image=Path(env['TILO_USH_IMAGES'])/('11'*32);image.mkdir(parents=True);(image/'usr').symlink_to('/usr',target_is_directory=True);(image/'etc').mkdir();(image/'etc/passwd').write_text('root:x:0:0:root:/root:/bin/bash\n');(image/'etc/group').write_text('root:x:0:\n');(image.parent/'current-runtime').write_text('tapp:'+'11'*32+'\nprivate native qualification\n')
home=Path(env['TILO_USH_HOME'])/'environments/default/home';home.mkdir(parents=True);(home/'.bashrc').write_text("PS1='MOUNTED> '; PS2='CONT> '; HISTFILE=/dev/null\nset -o emacs\nbind 'set enable-active-region on'\n")
settings=Path(env['XDG_CONFIG_HOME'])/'tilo/settings.json';settings.parent.mkdir(parents=True,exist_ok=True);settings.write_text(json.dumps({'accessibility':{'dictation_language':'de'}}))
case=json.loads(Path('/case.json').read_text())
spec=importlib.util.spec_from_file_location('scorer','/speech-score.py');scorer=importlib.util.module_from_spec(spec);spec.loader.exec_module(scorer)
owned=[];control=legacy.Control(Path(env['TILO_TEST_CONTROL']))
def start(command,name,extra=None,pass_fds=()):
 f=(root/(name+'.log')).open('w');p=subprocess.Popen(command,env=extra or env,stdout=f,stderr=subprocess.STDOUT,start_new_session=True,pass_fds=pass_fds);owned.append((name,p,f));return p
def wait(desc,fn,seconds=25):
 end=time.monotonic()+seconds
 while True:
  r=fn()
  if r:return r
  assert time.monotonic()<end,desc
  time.sleep(.05)
def invoke(provider,id,args=None):return control.send({'op':'actions_invoke','provider':provider,'id':id,'args':args or {},'wait_ms':10000})
def listing(query):return control.send({'op':'actions_enumerate','query':query,'include_programmatic':True,'wait_ms':10000,'limit':100})
def action(name):return invoke('actions',name)
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
def prepare():
 assert native().get('available'),'original native editor must own focus before admission'
 action('dictation.toggle')
 wait('missing synthetic-only one-session consent frame',lambda:'tilo.shell.trusted-frame/dictation-microphone' in control.target_names(),25)
 control.send({'op':'key','key':'ArrowLeft'});control.send({'op':'key','key':'Enter'})
 wait('synthetic recorder not started',lambda:'tilo-dictation-microphone' in nodes(),25)
 w=workers();assert len(w)==1,('production speech worker not observable',w)
 print('TERMINAL_ACTUAL_WORKER_STARTED',json.dumps({'pids':w,'source':'isolated virtual audio; hardware disabled'}),flush=True)
def retired():
 wait('recorder retained after session retirement',lambda:'tilo-dictation-microphone' not in nodes(),15)
 wait('worker retained after session retirement',lambda:not workers(),15)
 wait('dictation frame retained after retirement',lambda:not any('dictation-' in n or 'dictation.live-preview' in n for n in control.target_names()),20)
try:
 dbus=runtime/'bus';start(['/usr/bin/dbus-daemon','--session','--nofork','--address=unix:path='+str(dbus)],'dbus');wait('private bus unavailable',dbus.is_socket);env['DBUS_SESSION_BUS_ADDRESS']='unix:path='+str(dbus)
 config=Path(env['XDG_CONFIG_HOME'])/'wireplumber/wireplumber.conf.d';config.mkdir(parents=True)
 (config/'fixture.conf').write_text('wireplumber.profiles = { main = { hardware.audio = disabled hardware.bluetooth = disabled hardware.video-capture = disabled support.dbus = disabled } }')
 start(['/usr/bin/pipewire'],'pipewire');wait('private PipeWire not ready',lambda:(runtime/'pipewire-0').is_socket())
 start(['/usr/bin/wireplumber'],'wireplumber')
 start(['/usr/bin/pw-loopback','-n','TiloPublicAudioFixture','-c','1','-m','MONO','-i','{node.name=TiloPublicAudioSink media.class=Audio/Sink}','-o','{node.name=TiloPublicAudioSource media.class=Audio/Source}'],'loopback')
 wait('virtual speech source absent',lambda:'TiloPublicAudioSource' in nodes())
 assert not any(n.startswith(('alsa_','bluez_')) for n in nodes())
 (runtime/'tilo').mkdir(mode=0o700);(runtime/'tilo/services').mkdir(mode=0o700)
 assert all((x.stat().st_mode&0o077)==0 for x in [runtime,runtime/'tilo',runtime/'tilo/services'])
 service=runtime/'tilo/services/ush.sock';ush=start(['/rust/debug/tilo-ush','serve','--socket',str(service),'--host-root','/qual-host'],'ush');wait('private production ush service unavailable',lambda:service.is_socket() or (ush.poll() is not None and (_ for _ in ()).throw(AssertionError('ush service exited'))))
 shell=start(['/rust/debug/shell','--mode','native','--backend','headless'],'shell');wait('private compositor unavailable',lambda:Path(env['TILO_TEST_CONTROL']).is_socket() or (shell.poll() is not None and (_ for _ in ()).throw(AssertionError('compositor exited'))),60)
 provision=control.send({'op':'provision_test_identity'});assert provision.get('answer',{}).get('throwaway') is True
 print('MOUNTED_PRIVATE_IDENTITY',json.dumps(provision),flush=True)
 os.killpg(shell.pid,signal.SIGTERM);shell.wait(timeout=10)
 state_path=Path(env['TILO_STATE_PATH']);assert state_path.is_relative_to(root)
 if state_path.exists():state_path.unlink()
 socket_path=Path(env['TILO_TEST_CONTROL'])
 if socket_path.exists():socket_path.unlink()
 shell=start(['/rust/debug/shell','--mode','native','--backend','headless'],'identified-shell');wait('identified private compositor unavailable',lambda:socket_path.is_socket() or (shell.poll() is not None and (_ for _ in ()).throw(AssertionError('identified compositor exited'))),60)
 legacy.complete_onboarding(control)
 print('MOUNTED_OPEN_REPLY',json.dumps(invoke('actions','window.new.terminal')),flush=True)
 wait('mounted Terminal shared widget absent',lambda:next((t for t in control.send({'op':'list_targets'})['targets'] if t['name'].endswith('/terminal') or t['name']=='terminal'),None),40)
 print('MOUNTED_TARGETS',json.dumps(control.send({'op':'list_targets'})),flush=True)
 time.sleep(2)
 control.send({'op':'click_named','name':'terminal'})
 action('group.maximize')
 wait('maximized native Terminal viewport too narrow to observe complete public transcript', lambda: next((t for t in control.send({'op':'list_targets'})['targets'] if t['name']=='terminal' and t['rect'][2]>=1300), None))
 time.sleep(.5)
 print('MOUNTED_TERMINAL_ACTIONS',json.dumps(listing('terminal')),flush=True)
 print('MOUNTED_PROGRAMMATIC_ACTIONS',json.dumps(listing('')),flush=True)
 print('MOUNTED_EDITOR_ACTIONS',json.dumps(listing('editable field')),flush=True)
 for operation in ['inspect-session','inspect-editor','read']:
  rows=listing('')['answer']['actions'];row=next(x for x in rows if x.get('application')=='so.tilo.terminal' and x.get('operation')==operation)
  print('MOUNTED_ACTUAL_OPERATION',operation,json.dumps(invoke(row['provider'],row['id'])),flush=True)
 def actual(operation,args=None,host=False):
  rows=listing('')['answer']['actions']
  row=next(x for x in rows if x.get('operation')==operation and x.get('application')=='so.tilo.terminal' and (x.get('resource')=='tilo.host:edit' if host else True))
  if host:
   def ready_host():
    current=next(x for x in listing('')['answer']['actions'] if x.get('operation')==operation and x.get('application')=='so.tilo.terminal' and x.get('resource')=='tilo.host:edit')
    return current if current['contract']['availability']['state']=='available' else None
   row=wait('mounted native host history never advertised '+operation,ready_host)
  reply=invoke(row['provider'],row['id'],args)
  print('MOUNTED_NATIVE_RECEIPT',operation,json.dumps(reply),flush=True)
  update=reply['answer']['receipt']['update'];assert update['state']=='succeeded',update
  return update.get('output',{}).get('value',{})
 def native():return actual('inspect-editor')
 def visible():
  lines=[line for line in actual('read')['output'].splitlines() if line.strip()]
  return lines[-1] if lines else ''
 def chord(key):
  control.send({'op':'key_down','key':'Control'})
  try:control.send({'op':'key','key':key})
  finally:control.send({'op':'key_up','key':'Control'})
 control.send({'op':'type','text':'café ☕'})
 wait('actual mounted original text unobservable',lambda:visible().endswith('café ☕'))
 for key in ['b','Space','a','x','x']:chord(key)
 original=wait('native original selection unavailable',lambda:(value if (value:=native()).get('available') and json.loads(value['lease'])['anchor']==0 and json.loads(value['lease'])['focus']==6 else None))
 actual('replace-editor-selection',{'session':original['session'],'lease':original['lease'],'text':'salut ','confirm':True})
 wait('mounted acknowledged replacement not visible',lambda:visible().endswith('salut ☕'))
 assert native()['can_undo']
 actual('undo',host=True);wait('ONE mounted native undo did not restore original',lambda:visible().endswith('café ☕'))
 restored=native();assert json.loads(restored['lease'])['anchor']==0 and json.loads(restored['lease'])['focus']==6 and restored['can_redo']
 actual('redo',host=True);wait('ONE mounted native redo did not restore replacement',lambda:visible().endswith('salut ☕'))
 actual('undo',host=True);wait('second mounted undo did not restore original',lambda:visible().endswith('café ☕'))
 print('PASS actual mounted Terminal semantic selected Unicode replacement and ONE native Undo/Redo/Undo with restored selection',flush=True)

 started=time.monotonic();prepare();capture_started=time.monotonic()
 player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'public-player')
 preview_seconds=None
 while player.poll() is None:
  if preview_seconds is None and 'dictation.live-preview' in control.target_names():preview_seconds=time.monotonic()-capture_started
  assert time.monotonic()-capture_started<25,'public playback exceeded bound'
  time.sleep(.1)
 assert player.returncode==0 and preview_seconds is not None,'public playback or streamed preview unobserved'
 invoke('actions','dictation.finish');retired()
 result=wait('recorded mounted Terminal final absent',lambda:(line if (line:=visible()).endswith('☕') and 'café' not in line else None))
 assert result.startswith('MOUNTED> '),result
 transcript=result[len('MOUNTED> '):-1].strip();reference=scorer.units(case['reference']);errors=scorer.distance(reference,scorer.units(transcript))
 assert errors/len(reference)<=.20,(transcript,errors,len(reference))
 print('TERMINAL_ACTUAL_FINAL',json.dumps({'text':transcript,'buffer':result,'word_errors':errors,'reference_words':len(reference),'wer':errors/len(reference),'first_nonempty_preview_seconds':preview_seconds,'capture_to_final_seconds':time.monotonic()-capture_started}),flush=True)
 actual('undo',host=True);wait('ONE recorded native Undo failed',lambda:visible().endswith('café ☕'))
 restored=native();assert json.loads(restored['lease'])['anchor']==0 and json.loads(restored['lease'])['focus']==6
 actual('redo',host=True);wait('ONE recorded native Redo failed',lambda:visible()==result)
 print('PASS actual mounted recorded speech selected Unicode replacement and ONE semantic native Undo/Redo with restored original selection',flush=True)
 chord('e');prepare();player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'cancel-player');time.sleep(4.5);assert player.poll() is None
 invoke('actions','dictation.cancel');retired()
 if player.poll() is None:os.killpg(player.pid,signal.SIGTERM)
 player.wait(timeout=5);assert visible()==result
 print('PASS mounted repeated session cancellation preserves buffer and retires recorder/worker',flush=True)
 prepare();player=start(['/usr/bin/pw-play','--target','TiloPublicAudioSink','/public.wav'],'focus-player');time.sleep(2);assert player.poll() is None
 original_session=native()['session']
 invoke('actions','window.new.terminal');retired()
 if player.poll() is None:os.killpg(player.pid,signal.SIGTERM)
 player.wait(timeout=5)
 invoke('actions','window.focus-previous')
 wait('original mounted Terminal did not regain native focus',lambda:native().get('session')==original_session and native().get('available'))
 assert visible()==result,'late result changed original after focus away/back'
 prepare();invoke('actions','dictation.cancel');retired();assert visible()==result
 print('PASS mounted focus away/back retires active speech; fresh repeated session cancellation preserves original',flush=True)
 print('MOUNTED_TERMINAL_RECORDED_COMPLETE',json.dumps({'physical_microphone':False,'installed':False,'model':'Small Q5_1','language':'de','elapsed_seconds':time.monotonic()-started}),flush=True)

finally:
 for name,p,f in reversed(owned):
  if p.poll() is None:os.killpg(p.pid,signal.SIGTERM)
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait(timeout=5)
  f.close();print('MOUNTED_OWNED_REAPED',name,p.returncode,flush=True)
  log=root/(name+'.log');print('MOUNTED_LOG',name,json.dumps(log.read_text()[-10000:]),flush=True)
  diagnostic=[line for line in log.read_text().splitlines() if 'DICTATION_NATIVE_HISTORY_DIAGNOSTIC' in line or 'DICTATION_TERMINAL_HISTORY_DIAGNOSTIC' in line]
  print('MOUNTED_HISTORY_DIAGNOSTIC',name,json.dumps(diagnostic[-120:]),flush=True)
