import hashlib,json,os,re,subprocess,time
from pathlib import Path
import sys
b=Path(sys.argv[1]);s=b/'src';out=Path(sys.argv[2])
cid=out/'run-de_de-0-r1.cid';log=out/'run-de_de-0-r1.log';assert not cid.exists() and not log.exists(),'fresh qualification required'
receipt=json.loads((out/'manifest.json').read_text())
receipt.update({'limits':{'cpus':2,'memory_bytes':3*1024**3,'pids':256,'runtime_seconds':240},'driver_hashes':{n:hashlib.sha256((out/n).read_bytes()).hexdigest() for n in ['recorded-inner.py','recorded-outer.py']}})
command=['podman','run','--rm','--cidfile',str(cid),'--timeout','240','--network=none','--memory=3g','--cgroup-conf=memory.swap.max=0','--cpus=2','--pids-limit=256','--security-opt','label=disable','--user','0:0','--volume',f'{b}:{b}:ro','--entrypoint','python3','sha256:a3f63d1003795b29af6e512fde6a3f37c7942b986d7a57e4fe4fd9f32c204d9e',str(out/'recorded-outer.py'),str(b),str(out)]
cpus=sorted(os.sched_getaffinity(0))[:2];assert len(cpus)==2;receipt['cpu_affinity']=cpus
gate=out/'run-de_de-0-r1.gate';assert not gate.exists()
wrapper="import pathlib,time,runpy,sys; p=pathlib.Path(sys.argv[1]); deadline=time.monotonic()+20\nwhile not p.exists():\n if time.monotonic()>deadline: raise SystemExit('resource gate not released')\n time.sleep(.05)\nsys.argv=sys.argv[2:];runpy.run_path(sys.argv[0],run_name='__main__')"
wrapper='import os,json; os.sched_setaffinity(0,'+repr(cpus)+'); assert len(os.sched_getaffinity(0))==2; print(\"CPU_AFFINITY\",json.dumps(sorted(os.sched_getaffinity(0))),flush=True)\n'+wrapper
command[-3:]=['-u','-c',wrapper,str(gate),str(out/'recorded-outer.py'),str(b),str(out)]
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
  receipt['observed_scope_controls']={'memory.max':(cg/'memory.max').read_text().strip(),'cpu.max':(cg/'cpu.max').read_text().strip(),'pids.max':(cg/'pids.max').read_text().strip(),'memory.swap.max':swap}
  gate.write_text('verified bounded resources\n')
  deadline=before+250;peak_memory=0;peak_pids=0;peak_anon=0;peak_file=0
  while p.poll() is None:
   if time.monotonic()>deadline:raise subprocess.TimeoutExpired(command,250)
   try:
    peak_memory=max(peak_memory,int((cg/'memory.current').read_text()));peak_pids=max(peak_pids,int((cg/'pids.current').read_text()));stats=dict(line.split() for line in (cg/'memory.stat').read_text().splitlines());peak_anon=max(peak_anon,int(stats['anon']));peak_file=max(peak_file,int(stats['file']))
   except FileNotFoundError:
    receipt['sampling_scope_removed_before_cli_exit']=True;break
   time.sleep(.1)
  receipt['exit']=p.wait(timeout=max(1,deadline-time.monotonic()));receipt['sampled_peak_memory_bytes']=peak_memory;receipt['sampled_peak_process_threads']=peak_pids;receipt['sampled_peak_anon_bytes']=peak_anon;receipt['sampled_peak_file_cache_bytes']=peak_file
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
 receipt['log_sha256']=hashlib.sha256(log.read_bytes()).hexdigest();receipt['complete']=receipt.get('exit')==0 and 'FEATHERPAD_NATIVE_JOURNEY_COMPLETE' in log.read_text()
 (out/'run-de_de-0-r1-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({k:receipt[k] for k in ['complete','exit','failure','elapsed_seconds'] if k in receipt}),flush=True)
print(log.read_text()[-16000:],flush=True)
if not receipt['complete']:raise SystemExit(1)
