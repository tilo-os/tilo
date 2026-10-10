import importlib.util,json,os,signal,socket,subprocess,time
from pathlib import Path
assert os.getuid()==65534 and os.getgid()==65534
os.sched_setaffinity(0,sorted(os.sched_getaffinity(0))[:2])
assert len(os.sched_getaffinity(0))==2
assert next(l for l in Path('/proc/self/status').read_text().splitlines() if l.startswith('CapEff:')).split()[1]=='0000000000000000'
assert not Path('/dev/snd').exists() and not Path('/dev/dri').exists()
spec=importlib.util.spec_from_file_location('legacy','/source/scripts/legacy-app-caret-e2e.py');legacy=importlib.util.module_from_spec(spec);spec.loader.exec_module(legacy)
root=Path('/tmp/mounted');root.mkdir(mode=0o700);runtime=root/'runtime';runtime.mkdir(mode=0o700)
env=dict(os.environ,PATH='/usr/bin:/bin',HOME=str(root/'home'),USER='ordinary',LANG='C.UTF-8',XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),TILO_IDENTITY_ROOT=str(root/'identity'),TILO_AUTHORITY_SOCKET_DIR=str(root/'authority'),TILO_FS_AUTHORITY_CACHE_DIR=str(root/'fs-authority'),TILO_FS_CONTENT_PEER_CACHE_DIR=str(root/'fs-peers'),TILO_TEST_CONTROL=str(runtime/'tilo-session/test-control.sock'),TILO_HEADLESS_SIZE='1600x900',TILO_WORKSPACE_ROOTS='',TILO_USH_HOME=str(root/'ush'),TILO_USH_IMAGES=str(root/'images'),TILO_USH_BIN='/rust/debug/tilo-ush',TILO_STATE_PATH=str(root/'config/tilo/state.json'),TILO_IDENTITY_FILE_SECRETS='1',RUST_LOG='warn',LP_NUM_THREADS='2',RAYON_NUM_THREADS='2',WGPU_BACKEND='vulkan')
for k in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','TILO_IDENTITY_ROOT','TILO_AUTHORITY_SOCKET_DIR','TILO_FS_AUTHORITY_CACHE_DIR','TILO_FS_CONTENT_PEER_CACHE_DIR']:Path(env[k]).mkdir(parents=True,exist_ok=True)
image=Path(env['TILO_USH_IMAGES'])/('11'*32);image.mkdir(parents=True);(image/'usr').symlink_to('/usr',target_is_directory=True);(image/'etc').mkdir();(image/'etc/passwd').write_text('root:x:0:0:root:/root:/bin/bash\n');(image/'etc/group').write_text('root:x:0:\n');(image.parent/'current-runtime').write_text('tapp:'+'11'*32+'\nprivate native qualification\n')
home=Path(env['TILO_USH_HOME'])/'environments/default/home';home.mkdir(parents=True);(home/'.bashrc').write_text("PS1='MOUNTED> '; PS2='CONT> '; HISTFILE=/dev/null\nset -o emacs\nbind 'set enable-active-region on'\n")
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
try:
 dbus=runtime/'bus';start(['/usr/bin/dbus-daemon','--session','--nofork','--address=unix:path='+str(dbus)],'dbus');wait('private bus unavailable',dbus.is_socket);env['DBUS_SESSION_BUS_ADDRESS']='unix:path='+str(dbus)
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
 time.sleep(.5)
 print('MOUNTED_TERMINAL_ACTIONS',json.dumps(listing('terminal')),flush=True)
 print('MOUNTED_PROGRAMMATIC_ACTIONS',json.dumps(listing('')),flush=True)
 print('MOUNTED_EDITOR_ACTIONS',json.dumps(listing('editable field')),flush=True)
 for operation in ['inspect-session','inspect-editor','read']:
  rows=listing('')['answer']['actions'];row=next(x for x in rows if x.get('application')=='so.tilo.terminal' and x.get('operation')==operation)
  print('MOUNTED_ACTUAL_OPERATION',operation,json.dumps(invoke(row['provider'],row['id'])),flush=True)
 def actual(operation,args=None,host=False):
  rows=listing('')['answer']['actions']
  row=next(x for x in rows if x.get('operation')==operation and (x.get('resource')=='tilo.host:edit' if host else x.get('application')=='so.tilo.terminal'))
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

 print('MOUNTED_DICTATION_REPLY',json.dumps(invoke('actions','dictation.toggle')),flush=True)
 print('MOUNTED_POST_DICTATION_TARGETS',json.dumps(control.send({'op':'list_targets'})),flush=True)
 print('MOUNTED_SESSION_ACTIONS',json.dumps(listing('inspect session')),flush=True)
 print('MOUNTED_STATE',json.dumps(control.send({'op':'state_dump'})),flush=True)
 print('MOUNTED_TERMINAL_INSPECTION_COMPLETE',json.dumps({'physical_microphone':False,'model_activation':False,'full_dictation_acceptance':False}),flush=True)
finally:
 for name,p,f in reversed(owned):
  if p.poll() is None:os.killpg(p.pid,signal.SIGTERM)
  try:p.wait(timeout=5)
  except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait(timeout=5)
  f.close();print('MOUNTED_OWNED_REAPED',name,p.returncode,flush=True)
  log=root/(name+'.log');print('MOUNTED_LOG',name,json.dumps(log.read_text()[-10000:]),flush=True)
