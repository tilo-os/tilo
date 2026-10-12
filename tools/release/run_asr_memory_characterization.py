#!/usr/bin/env python3
"""Remote public-only development matrix; holdout and production limits stay untouched."""
from pathlib import Path
import subprocess
import os
import json
import argparse
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--include-2g-characterization', action='store_true')
parser.add_argument('--include-adaptive-context', action='store_true',
                    help='reproduce rejected shorter-context quality tradeoff; never production qualification')
args = parser.parse_args()
root=Path('/tmp/tilo-dictation');out=root/'target/asr-memory-characterization-v1'
# Frozen exposed development cases only. Low-memory configuration precedes
# explicitly authorized 2GiB characterization. No holdout or model defaults.
rows=[('flash',1,model,'development-read-'+lang+'-0','offline',2,'cli')
      for model in ['small-q5','small-q8']
      for lang in ['en_us','de_de','fr_fr','es_419','ar_eg','cmn_hans_cn']]
rows += [('flash',1,model,'development-read-fr_fr-0','paced',cpus,'cli')
         for model in ['small-q5','small-q8'] for cpus in [2,1]]
if args.include_2g_characterization:
 rows += [('baseline',2,model,'development-read-fr_fr-0','offline',2,'cli') for model in ['base','base-q8','small-q5','small-q8']]
if args.include_2g_characterization:
 rows += [('baseline',2,'nemotron-q8','development-read-fr_fr-0','paced',2,'native')]
if args.include_adaptive_context:
 rows += [('flash-adaptive',1,'small-q5','development-read-fr_fr-0',mode,2,'cli') for mode in ['offline','paced']]
for variant,budget,model,case,mode,cpus,api in rows:
 values={line.split(':')[0]:int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if ':' in line and line.split()[1].isdigit()}
 psi=Path('/proc/pressure/memory').read_text();avg=float(psi.splitlines()[0].split()[1].split('=')[1])
 print('CHARACTERIZATION_HEADROOM '+json.dumps({'model':model,'budget_gib':budget,'available_kib':values['MemAvailable'],'memory_pressure_some_avg10':avg,'production_cap_unchanged':True}),flush=True)
 if values['MemAvailable']<8*1024*1024 or avg>5:raise RuntimeError('stop: remote memory pressure/headroom')
 subprocess.run(['python3',str(out/f'probe-{variant}-{budget}g.py'),'--model',model,'--case',case,'--mode',mode,'--cpus',str(cpus),'--allocator','arena-1','--nemo-api',api],env=dict(os.environ,PYTHONPATH=str(root/'tools/release')),check=True)
