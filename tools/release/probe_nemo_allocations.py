#!/usr/bin/env python3
"""Remote-only glibc allocator diagnosis for the pinned public Nemotron case.

Uses the explicitly authorized separate 2GiB characterization budget, never
production qualification or a deployed cap change. No downloads or microphone.
Build the pinned native/model fixtures and memory probes first.
"""
from pathlib import Path
import subprocess,os,json
root=Path('/tmp/tilo-dictation');out=root/'target/asr-memory-characterization-v1'
(out/'alloc_trace.c').write_text((root/'tools/release/fixtures/asr_alloc_trace.c').read_text())
subprocess.run(['cc','-std=gnu11','-O2','-shared','-fPIC',str(out/'alloc_trace.c'),'-ldl','-o',str(out/'alloc-trace.so')],check=True)
probe=(out/'probe-baseline-2g.py').read_text()
needle="'--setenv', 'LD_LIBRARY_PATH', '/review/bin',"
if probe.count(needle)!=2:raise ValueError('review namespace changed')
probe=probe.replace(needle, "'--setenv', 'LD_LIBRARY_PATH', '/review/bin', '--ro-bind', str(ROOT/'asr-memory-characterization-v1/alloc-trace.so'), '/review/alloc-trace.so', '--setenv', 'LD_PRELOAD', '/review/alloc-trace.so',")
(out/'probe-nemo-alloc-2g.py').write_text(probe)
values={l.split(':')[0]:int(l.split()[1]) for l in Path('/proc/meminfo').read_text().splitlines() if ':' in l and l.split()[1].isdigit()}
avg=float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
print('ALLOCATION_TRACE_HEADROOM '+json.dumps({'available_kib':values['MemAvailable'],'pressure_some_avg10':avg,'cgroup_memory_max':Path('/sys/fs/cgroup/memory.max').read_text().strip(),'cgroup_memory_current_bytes':int(Path('/sys/fs/cgroup/memory.current').read_text()),'production_cap_unchanged':True}),flush=True)
if values['MemAvailable']<8*1024*1024 or avg>5:raise RuntimeError('stop: pressure/headroom guard')
subprocess.run(['python3',str(out/'probe-nemo-alloc-2g.py'),'--model','nemotron-q8','--case','development-read-fr_fr-0','--mode','paced','--cpus','2','--allocator','arena-1','--nemo-api','native'],env=dict(os.environ,PYTHONPATH=str(root/'tools/release')),check=True)
