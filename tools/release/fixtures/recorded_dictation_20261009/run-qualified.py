# Portable view of a frozen qualification driver; original bytes are indexed
# in docs/architecture/dictation-execution-environment-index.json.
# This does not qualify current source or select/install a model.
import os, re
if not os.path.isabs(os.environ['TILO_QUALIFICATION_BUILD_ROOT']):
    raise ValueError('an explicit absolute owned build root is required')
if not re.fullmatch(r'(?:[a-z0-9][a-z0-9./:_-]*@)?sha256:[0-9a-f]{64}', os.environ['TILO_QUALIFICATION_CONTAINER_IMAGE']):
    raise ValueError('an explicit immutable qualified container image is required')
import hashlib,json,os,re,subprocess,time
from pathlib import Path
b=Path(os.environ['TILO_QUALIFICATION_BUILD_ROOT']).resolve();s=b/'src';out=s/'target/dictation-recorded-compositor-20261009'
cid=out/'run-r15.cid';log=out/'run-r15.log';assert not cid.exists() and not log.exists(),'fresh qualification required'
binaries={n:{'bytes':(b/'target'/p).stat().st_size,'sha256':hashlib.file_digest((b/'target'/p).open('rb'),'sha256').hexdigest()} for n,p in [('shell','debug/shell'),('browser_fixture','debug/examples/browser-text-input-qualification'),('worker','debug/tilo-speech-worker'),('witness','debug/examples/speech_qualify')]}
names=['crates/sdk/ui-core/src/app/host_commands.rs','crates/sdk/ui-core/src/widget/webview.rs','Cargo.lock','apps/notes/src/app.rs','crates/session/shell/src/compositor/dictation.rs','crates/session/shell/src/compositor/dictation/runtime.rs','crates/session/shell/src/compositor/audio_capture.rs','crates/sdk/ui-core/examples/browser_text_input_qualification.rs','crates/sdk/ui-core/src/webview/text_input.rs','apps/browser/chromium/src/page_automation.js']
receipt={'source_head':'b21d60d2914b704c982e99727f2a42a38d158ebc','source_state':'uncommitted guarded native-history key-release fix','binaries':binaries,'sources':{n:hashlib.sha256((s/n).read_bytes()).hexdigest() for n in names},'live_microphone':False,'installed':False,'limits':{'cpus':2,'memory_bytes':3*1024**3,'pids':256,'runtime_seconds':240},'driver_hashes':{n:hashlib.sha256((out/n).read_bytes()).hexdigest() for n in ['recorded-inner.py','recorded-outer.py']}}
command=['podman','run','--rm','--cidfile',str(cid),'--timeout','240','--network=none','--memory=3g','--cgroup-conf=memory.swap.max=0','--cpus=2','--pids-limit=256','--security-opt','label=disable','--user','0:0','--volume',f'{b}:{b}:ro','--entrypoint','python3',os.environ['TILO_QUALIFICATION_CONTAINER_IMAGE'],str(out/'recorded-outer.py'),str(b)]
cpus=sorted(os.sched_getaffinity(0))[:2];assert len(cpus)==2;receipt['cpu_affinity']=cpus
gate=out/'run-r15.gate';assert not gate.exists()
wrapper="import pathlib,time,runpy,sys; p=pathlib.Path(sys.argv[1]); deadline=time.monotonic()+20\nwhile not p.exists():\n if time.monotonic()>deadline: raise SystemExit('resource gate not released')\n time.sleep(.05)\nsys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')"
wrapper='import os,json; os.sched_setaffinity(0,'+repr(cpus)+'); assert len(os.sched_getaffinity(0))==2; print(\"CPU_AFFINITY\",json.dumps(sorted(os.sched_getaffinity(0))),flush=True)\n'+wrapper
command[-2:]=['-u','-c',wrapper,str(gate),str(out/'recorded-outer.py'),str(b)]
before=time.monotonic();p=None;identity=None
try:
 with log.open('w') as f:
  p=subprocess.Popen(command,stdout=f,stderr=subprocess.STDOUT)
  deadline=time.monotonic()+15
  while not cid.exists():
   if p.poll() is not None:raise RuntimeError('container failed before private namespace creation')
   if time.monotonic()>deadline:raise RuntimeError('owned container CID not published')
   time.sleep(.05)
  identity=cid.read_text().strip();assert re.fullmatch('[0-9a-f]{64}',identity)
  state=json.loads(subprocess.check_output(['podman','inspect',identity],text=True,timeout=5))[0];config=state['HostConfig'];assert config['Memory']==3*1024**3 and config['PidsLimit']==256
  assert config.get('NanoCpus',0)==2000000000 or (config.get('CpuQuota',0)==200000 and config.get('CpuPeriod',0)==100000),config
  receipt['container_scope']=state['State'];receipt['image']=state['Image']
  cg=Path('/sys/fs/cgroup')/state['State']['CgroupPath'].lstrip('/');assert int((cg/'memory.max').read_text())==3*1024**3
  quota,period=map(int,(cg/'cpu.max').read_text().split());assert quota==2*period
  assert int((cg/'pids.max').read_text())==256
  (cg/'memory.swap.max').write_text('0');swap=(cg/'memory.swap.max').read_text().strip();assert swap=='0',f'expected memory.swap.max=0, observed {swap}'
  receipt['observed_scope_controls']={'memory.max':(cg/'memory.max').read_text().strip(),'cpu.max':(cg/'cpu.max').read_text().strip(),'pids.max':(cg/'pids.max').read_text().strip()}
  gate.write_text('verified bounded resources\n')
  deadline=before+250;peak_memory=0;peak_pids=0
  while p.poll() is None:
   if time.monotonic()>deadline:raise subprocess.TimeoutExpired(command,250)
   try:
    peak_memory=max(peak_memory,int((cg/'memory.current').read_text()));peak_pids=max(peak_pids,int((cg/'pids.current').read_text()))
   except FileNotFoundError:
    receipt['sampling_scope_removed_before_cli_exit']=True;break
   time.sleep(.1)
  receipt['exit']=p.wait(timeout=max(1,deadline-time.monotonic()));receipt['sampled_peak_memory_bytes']=peak_memory;receipt['sampled_peak_process_threads']=peak_pids
  line=next(line for line in log.read_text().splitlines() if line.startswith('CPU_AFFINITY '));receipt['observed_cpu_affinity']=json.loads(line.split(' ',1)[1]);assert receipt['observed_cpu_affinity']==cpus
except Exception as e:
 receipt['failure']=f'{type(e).__name__}: {e}'
 if p is not None and p.poll() is None and identity is not None:subprocess.run(['podman','stop','--time','5',identity],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)
finally:
 if identity is not None:subprocess.run(['podman','rm','--force',identity],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=15)
 if p is not None:
  try:p.wait(timeout=10)
  except subprocess.TimeoutExpired:p.kill();p.wait(timeout=5)
 receipt['elapsed_seconds']=time.monotonic()-before
 receipt['log_sha256']=hashlib.sha256(log.read_bytes()).hexdigest();receipt['complete']=receipt.get('exit')==0 and 'JOURNEY_COMPLETE' in log.read_text()
 (out/'run-r15-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({k:receipt[k] for k in ['complete','exit','failure','elapsed_seconds'] if k in receipt}),flush=True)
print(log.read_text()[-16000:],flush=True)
if not receipt['complete']:raise SystemExit(1)
