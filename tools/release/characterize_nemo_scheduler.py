#!/usr/bin/env python3
"""Review-only native scheduler-budget prototype within a separate 2GiB limit.

This patches an internal constant, not a supported recognizer CLI setting.
Snapshots and restores original pinned source, binaries and build intermediates.
Retains all near-capacity guards; never changes production or publishes a model.
Run only on the owned remote review checkout after allocation diagnosis.
"""
from pathlib import Path
import subprocess,shutil,hashlib,json,os
root=Path('/tmp/tilo-dictation');source=root/'target/nemo-asr-review-v1/source';build=source/'build/cpu-asr';out=root/'target/asr-nemo-scheduler-characterization-v1';out.mkdir(mode=0o700,exist_ok=True)
pressure=float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1]);available=int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
print('SCHEDULER_CHARACTERIZATION_HEADROOM '+json.dumps({'available_kib':available,'pressure_some_avg10':pressure,'cgroup_memory_max':Path('/sys/fs/cgroup/memory.max').read_text().strip(),'budget_bytes':2147483648,'production_cap_unchanged':True}),flush=True)
if available<8*1024*1024 or pressure>5:raise RuntimeError('stop: pressure/headroom')
session=source/'src/runtime/ggml/session.cpp';original=session.read_bytes();needle=b'static constexpr size_t kSchedGraphSize = 65536;'
if original.count(needle)!=1:raise ValueError('pinned scheduler source changed')
if hashlib.sha256(original).hexdigest() != 'b37a3a1b5810d9034a847aa0923d5ae7063ec70f8f43080fec36c01db6e6d81c':raise ValueError('exact reviewed scheduler source required')
if not (root/'target/asr-nemo-compare-v2/asr-nemo-compare').is_file():raise ValueError('build_nemo_comparison.py required before characterization')
if (out/'original-bin').exists():raise ValueError('original snapshot already exists; never overwrite its receipts')
shutil.copytree(build/'bin',out/'original-bin',symlinks=True)
artifacts=[*build.rglob('*.a'),*build.rglob('session.cpp.o')]
for p in artifacts:
 q=out/'original-intermediates'/p.relative_to(build);q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,q)
patched=original.replace(needle,b'static constexpr size_t kSchedGraphSize = 8192;')
(out/'session-original.cpp').write_bytes(original);(out/'session-graph8192.cpp').write_bytes(patched)
try:
 session.write_bytes(patched)
 subprocess.run(['cmake','--build',str(build),'--target','nemo_speech_asr','--parallel','2'],check=True)
 shutil.copytree(build/'bin',out/'graph8192-bin',symlinks=True)
finally:
 session.write_bytes(original)
 for p in (out/'original-bin').iterdir():
  target=build/'bin'/p.name
  if target.is_symlink():target.unlink()
  if p.is_symlink():target.symlink_to(os.readlink(p))
  elif p.is_file():shutil.copy2(p,target)
 for p in artifacts:shutil.copy2(out/'original-intermediates'/p.relative_to(build),p)
 if session.read_bytes()!=original:raise RuntimeError('pinned source restoration failed')
 for p in (out/'original-bin').iterdir():
  target=build/'bin'/p.name
  if p.is_file() and not p.is_symlink() and p.read_bytes()!=target.read_bytes():raise RuntimeError('pinned binary restoration failed')
receipts={}
for folder in ['original-bin','graph8192-bin']:
 receipts[folder]={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in (out/folder).iterdir() if p.is_file() and not p.is_symlink()}
receipts.update(original_session_sha256=hashlib.sha256(original).hexdigest(),patched_session_sha256=hashlib.sha256(patched).hexdigest(),scheduler_graph_capacity=8192,base_scheduler_graph_capacity=65536,public_recognizer_option=False,production_changed=False)
(out/'build-receipts.json').write_text(json.dumps(receipts,indent=2)+'\n');print('SCHEDULER_BUILD_RECEIPTS '+json.dumps(receipts),flush=True)
probe=(root/'target/asr-memory-characterization-v1/probe-nemo-alloc-2g.py').read_text();needle="ROOT/'nemo-asr-review-v1/source/build/cpu-asr/bin'"
if probe.count(needle)!=1:raise ValueError('native library bind source changed')
probe=probe.replace(needle,"ROOT/'asr-nemo-scheduler-characterization-v1/graph8192-bin'").replace("EXPLICIT REMOTE CHARACTERIZATION 2GiB; baseline", "EXPLICIT REMOTE CHARACTERIZATION 2GiB; Nemo internal graph8192 prototype")
(out/'probe-graph8192-2g.py').write_text(probe)
pressure=float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1]);available=int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
if available<8*1024*1024 or pressure>5:raise RuntimeError('stop: post-build pressure/headroom')
subprocess.run(['python3',str(out/'probe-graph8192-2g.py'),'--model','nemotron-q8','--case','development-read-fr_fr-0','--mode','paced','--cpus','2','--allocator','arena-1','--nemo-api','native'],env=dict(os.environ,PYTHONPATH=str(root/'tools/release')),check=True)
