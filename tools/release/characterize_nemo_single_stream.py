#!/usr/bin/env python3
"""Remote-only supported single-stream arena comparison under explicit 2GiB review.

Requires language-checked build_nemo_comparison.py and graph8192 receipts first.
No production change, downloads, microphone or holdout decoding. The original
scheduler failure and internal prototype success are reported separately.
"""
from pathlib import Path
import subprocess,shlex,os,json
root=Path('/tmp/tilo-dictation');source=root/'target/nemo-asr-review-v1/source';out=root/'target/asr-nemo-compare-slots1-v1';out.mkdir(mode=0o700,exist_ok=True)
fixture=(root/'target/asr-nemo-compare-v2/compare.cpp').read_text();needle='    config.batching.enabled = false;'
if fixture.count(needle)!=1:raise ValueError('fixture changed')
fixture=fixture.replace(needle,needle+'\n    config.batching.state_arena_slots = 1;')
(out/'compare.cpp').write_text(fixture)
lines=(source/'build/cpu-asr/build.ninja').read_text().splitlines();i=next(i for i,l in enumerate(lines) if 'recognizer.cpp.o: ' in l);includes=next(l.partition(' = ')[2] for l in lines[i:i+10] if l.strip().startswith('INCLUDES = '))
subprocess.run(['c++','-std=c++17','-O2',*shlex.split(includes),'-I'+str(source/'examples'),str(out/'compare.cpp'),'-L'+str(source/'build/cpu-asr/bin'),'-lnemo_speech_asr','-Wl,-rpath,'+str(source/'build/cpu-asr/bin'),'-o',str(out/'asr-nemo-compare')],check=True)
for profile,original in [('baseline',(root/'target/asr-memory-characterization-v1/probe-nemo-alloc-2g.py')),('graph8192',(root/'target/asr-nemo-scheduler-characterization-v1/probe-graph8192-languages-2g.py'))]:
 available=int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')));pressure=float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1]);print('SINGLE_STREAM_ARENA_HEADROOM '+json.dumps(dict(available_kib=available,pressure_some_avg10=pressure)),flush=True)
 if available<8*1024*1024 or pressure>5:raise RuntimeError('stop pressure/headroom')
 probe=original.read_text().replace("ROOT/'asr-nemo-compare-v1/asr-nemo-compare'","ROOT/'asr-nemo-compare-slots1-v1/asr-nemo-compare'").replace("ROOT/'asr-nemo-compare-v2/asr-nemo-compare'","ROOT/'asr-nemo-compare-slots1-v1/asr-nemo-compare'").replace("'--setenv', 'NEMO_SPEECH_MEMSTATS', '1',","'--setenv', 'NEMO_SPEECH_MEMSTATS', '1', '--setenv', 'NEMO_SPEECH_SCHEDULE_CAPTURE', '1',").replace('not production qualification','public state_arena_slots=1; not production qualification')
 target=out/('probe-'+profile+'-2g.py');target.write_text(probe)
 subprocess.run(['python3',str(target),'--model','nemotron-q8','--case','development-read-fr_fr-0','--mode','paced','--cpus','2','--allocator','arena-1','--nemo-api','native'],env=dict(os.environ,PYTHONPATH=str(root/'tools/release')),check=True)
