#!/usr/bin/env python3
"""Review-only scheduler pool telemetry, explicit separate remote 2GiB cap.

Restores original source/binaries/intermediates. The SessionState diagnostic is
not reached by this cache-aware model: its indexed caches are allocated inside
model_tensor_container instead. Do not interpret a missing diagnostic as zero
state. NEMO_SPEECH_SCHEDULE_CAPTURE reports actual scheduler compute pools.
"""
from pathlib import Path
import shutil,subprocess,json,hashlib,os
root=Path('/tmp/tilo-dictation');out=root/'target/asr-nemo-scheduler-characterization-v1';source=root/'target/nemo-asr-review-v1/source';build=source/'build/cpu-asr';session=source/'src/runtime/ggml/session.cpp';original=session.read_bytes();patched=(out/'session-graph8192.cpp').read_text();needle='    return SessionState(std::move(tc));'
if patched.count(needle)!=1:raise ValueError('state source changed')
if hashlib.sha256(original).hexdigest()!='b37a3a1b5810d9034a847aa0923d5ae7063ec70f8f43080fec36c01db6e6d81c':raise ValueError('original source changed')
available=int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')));pressure=float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1]);print('STATE_ACCOUNTING_HEADROOM '+json.dumps(dict(available_kib=available,pressure_some_avg10=pressure)),flush=True)
if available<8*1024*1024 or pressure>5:raise RuntimeError('stop pressure/headroom')
patched=patched.replace(needle,'    if (memstats_enabled()) fprintf(stderr, "[memstats] persistent-stream-state bytes=%zu\\n", tc->total_backend_buffer_bytes());\n'+needle)
(out/'session-graph8192-state-accounting.cpp').write_text(patched)
try:
 session.write_text(patched)
 subprocess.run(['cmake','--build',str(build),'--target','nemo_speech_asr','--parallel','2'],check=True)
 shutil.copytree(build/'bin',out/'graph8192-state-accounting-bin',symlinks=True)
finally:
 session.write_bytes(original)
 for p in (out/'original-bin').iterdir():
  target=build/'bin'/p.name
  if target.is_symlink():target.unlink()
  if p.is_symlink():target.symlink_to(os.readlink(p))
  elif p.is_file():shutil.copy2(p,target)
 for p in (out/'original-intermediates').rglob('*'):
  if p.is_file():shutil.copy2(p,build/p.relative_to(out/'original-intermediates'))
 if session.read_bytes()!=original:raise RuntimeError('restoration')
probe=(out/'probe-graph8192-languages-2g.py').read_text().replace("ROOT/'asr-nemo-scheduler-characterization-v1/graph8192-bin'","ROOT/'asr-nemo-scheduler-characterization-v1/graph8192-state-accounting-bin'").replace("'--setenv', 'NEMO_SPEECH_MEMSTATS', '1',","'--setenv', 'NEMO_SPEECH_MEMSTATS', '1', '--setenv', 'NEMO_SPEECH_SCHEDULE_CAPTURE', '1',").replace('Nemo internal graph8192 prototype','Nemo internal graph8192 state/scheduler accounting')
(out/'probe-graph8192-state-accounting-2g.py').write_text(probe)
print('STATE_ACCOUNTING_BUILD '+json.dumps(dict(source_sha256=hashlib.sha256(patched.encode()).hexdigest(),library_sha256=hashlib.sha256((out/'graph8192-state-accounting-bin/libnemo_speech_asr.so').read_bytes()).hexdigest())),flush=True)
subprocess.run(['python3',str(out/'probe-graph8192-state-accounting-2g.py'),'--model','nemotron-q8','--case','development-read-fr_fr-0','--mode','paced','--cpus','2','--allocator','arena-1','--nemo-api','native'],env=dict(os.environ,PYTHONPATH=str(root/'tools/release')),check=True)
