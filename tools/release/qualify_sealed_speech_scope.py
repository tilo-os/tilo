#!/usr/bin/env python3
"""Explicit verified-scope qualification of a descriptor-backed production worker.

Stage the exact materialization.json closure beside this file, outside the
builder container; run only inside the documented transient user service.
The canonical host-PSI admission paths remain unchanged. No model installation.
"""
from pathlib import Path
import json,os,subprocess,sys,hashlib,argparse
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--native-review',action='store_true')
parser.add_argument('--whisper-public-review',action='store_true',
                    help='qualify pinned multilingual Whisper with public fixtures; no installation')
parser.add_argument('--whisper-model',choices=['tiny','base','small-q5_1'],default='base')
parser.add_argument('--whisper-allocator',choices=['arena-1','default'],default='arena-1')
parser.add_argument('--whisper-diagnostics',action='store_true')
parser.add_argument('--expect-attention',choices=['flash','standard'])
parser.add_argument('--lifecycle',action='store_true')
parser.add_argument('--identity-review',action='store_true')
PUBLIC_CASES=['en_us-0','de_de-0','fr_fr-0','es_419-0','ar_eg-0','cmn_hans_cn-0']
parser.add_argument('--case',choices=PUBLIC_CASES,default='en_us-0')
parser.add_argument('--native-language','--language',dest='native_language',choices=['auto','en','de','fr','es','ar','zh'])
parser.add_argument('--append-case',action='append',choices=PUBLIC_CASES,default=[])
parser.add_argument('--vocabulary',action='append',default=[])
parser.add_argument('--native-phase',action='store_true')
parser.add_argument('--native-low-latency',action='store_true')
parser.add_argument('--native-right-context',type=int,choices=[0,1,3],default=1)
parser.add_argument('--phase-repeat',action='store_true')
parser.add_argument('--resident-second-case',choices=['en_us-0','de_de-0','silence'])
parser.add_argument('--noise-snr-db',type=int,choices=[10,20])
parser.add_argument('--frozen-evaluation-index',type=int,choices=range(32))
parser.add_argument('--evaluation-profile',choices=['context160','context320'])
args=parser.parse_args()
if args.whisper_diagnostics and not args.whisper_public_review:raise RuntimeError('Whisper diagnostics require explicit public review')
if args.expect_attention and not args.whisper_diagnostics:raise RuntimeError('attention expectation requires actual worker diagnostics')
if args.whisper_model!='base' and not args.whisper_public_review:raise RuntimeError('alternate Whisper model requires explicit public review')
if args.whisper_allocator!='arena-1' and not args.whisper_public_review:raise RuntimeError('alternate Whisper allocator requires explicit public review')
if args.whisper_public_review and (args.native_review or args.lifecycle or args.identity_review or args.frozen_evaluation_index is not None or args.evaluation_profile or args.native_phase or args.native_low_latency or args.native_right_context!=1 or args.phase_repeat or args.resident_second_case):raise RuntimeError('public Whisper review excludes native, lifecycle and frozen evaluation options')
if args.identity_review and not args.lifecycle:raise RuntimeError('identity review requires fixed native lifecycle')
evaluation=args.frozen_evaluation_index is not None
if evaluation and (not args.native_review or not args.evaluation_profile or args.case!='en_us-0' or args.lifecycle or args.vocabulary or args.noise_snr_db is not None or args.phase_repeat or args.resident_second_case or args.native_phase or args.native_low_latency or args.native_right_context!=1 or args.native_language or args.append_case):raise RuntimeError('frozen evaluation requires its fixed indexed journey')
if not evaluation and args.evaluation_profile:raise RuntimeError('evaluation profile requires fixed index')
if args.lifecycle and (not args.native_review or args.case!='en_us-0' or args.vocabulary or args.noise_snr_db is not None or args.phase_repeat or args.resident_second_case or args.native_phase or args.native_low_latency or args.native_right_context not in [1,3] or args.native_language or args.append_case):raise RuntimeError('native lifecycle requires its fixed public journey')
if not args.native_review and not args.whisper_public_review and (args.case!='en_us-0' or args.vocabulary or args.noise_snr_db is not None or args.phase_repeat or args.resident_second_case or args.native_phase or args.native_low_latency or args.native_right_context!=1 or args.native_language or args.append_case):raise RuntimeError('explicit public review required for alternate fixture')
if args.resident_second_case and (args.case not in ['en_us-0','de_de-0'] or args.native_phase or args.native_low_latency or args.native_right_context!=1 or args.phase_repeat or args.vocabulary or args.noise_snr_db is not None or args.native_language or args.append_case):raise RuntimeError('resident review has fixed bounded public sessions')
if args.append_case and args.native_language!='auto':raise RuntimeError('language transition requires explicit automatic recognition')
root=Path(__file__).parent
sys.path.insert(0,str(root))
import probe_asr_candidate as probe
cg=Path('/sys/fs/cgroup')/next(x.split('::',1)[1].lstrip('/') for x in Path('/proc/self/cgroup').read_text().splitlines() if x.startswith('0::'))
expected={'memory.max':'1342177280','memory.high':'1207959552','memory.swap.max':'0','cpu.max':'200000 100000','cpu.weight':'20','pids.max':'64'}
controls={k:(cg/k).read_text().strip() for k in expected}
if controls!=expected:raise RuntimeError('unverified scope controls')
runtime=subprocess.check_output(['systemctl','--user','show',cg.name,'-p','RuntimeMaxUSec','--value'],text=True).strip()
if runtime not in ['4min','240s']:raise RuntimeError('unverified runtime maximum:'+runtime)
avail=int(next(x.split()[1] for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:')))
if avail<8*1024*1024:raise RuntimeError('host headroom')
for ancestor in cg.parents:
 if not (ancestor/'memory.max').exists():continue
 maximum=(ancestor/'memory.max').read_text().strip();current=int((ancestor/'memory.current').read_text())
 if maximum!='max' and int(maximum)-current<1342177280:raise RuntimeError('ancestor headroom')
events=lambda:dict((x.split()[0],int(x.split()[1])) for x in (cg/'memory.events').read_text().splitlines())
before=events();pressure_before=(cg/'memory.pressure').read_text()
if any(before.get(k,1)!=0 for k in ['high','max','oom','oom_kill']):raise RuntimeError('scope not fresh')
if any(float(x.split()[1].split('=')[1])>5 for x in pressure_before.splitlines()):raise RuntimeError('scope pressure')
host_pressure=Path('/proc/pressure/memory').read_text()
print('SEALED_WORKER_SCOPE_BEFORE '+json.dumps(dict(cgroup=str(cg),controls=controls,runtime=runtime,available_kib=avail,events=before,scope_pressure=pressure_before,host_pressure=host_pressure,admission='explicit verified transient scope; default host PSI gate unchanged elsewhere')),flush=True)
receipt_path=root/'materialization.json'
if receipt_path.stat().st_size>65536:raise RuntimeError('bounded materialization receipt required')
receipts=json.loads(receipt_path.read_text())
required={'worker','qualifier','probe.py','probe_speech_memory.py','probe_asr_candidate.py','summarize_speech_benchmark.py','tilo-speech-worker'}
if args.native_review:required=(required-{'qualifier'})|{'libnemo_speech_asr.so','libggml.so.0','libggml-cpu.so.0','libggml-base.so.0'}
if evaluation:required=required|{'suite.json','evaluation-freeze.json','public/manifest.json'}
if args.lifecycle:required=required|{'lifecycle.py','probe_speech_stream.py'}
if args.resident_second_case:required=required|{'resident.py','probe_speech_stream.py'}
if set(receipts)!=required or any(not isinstance(h,str) or len(h)!=64 or any(c not in '0123456789abcdef' for c in h) for h in receipts.values()):raise RuntimeError('exact materialization closure required')
for n,h in receipts.items():
 if hashlib.sha256((root/n).read_bytes()).hexdigest()!=h:raise RuntimeError('materialization changed')
if evaluation:
 suite_path=root/'suite.json';freeze_path=root/'evaluation-freeze.json'
 if suite_path.stat().st_size>1048576 or freeze_path.stat().st_size>65536:raise RuntimeError('bounded frozen evaluation required')
 suite=json.loads(suite_path.read_text());payload=dict(suite);stated=payload.pop('suite_sha256')
 digest=hashlib.sha256(json.dumps(payload,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()).hexdigest()
 if stated!='9995db50a190bcd818373886e06c404c9d9009cbb85a76cf166dd65daf8ef8fd' or digest!=stated:raise RuntimeError('exact predeclared public suite required')
 freeze=json.loads(freeze_path.read_text())
 profiles={'context160':1,'context320':3}
 if (freeze.get('suite_sha256')!=stated or freeze.get('profiles')!=profiles or freeze.get('worker_sha256')!=receipts['worker'] or freeze.get('library_sha256')!=receipts['libnemo_speech_asr.so'] or freeze.get('scope_sha256')!=hashlib.sha256(Path(__file__).read_bytes()).hexdigest() or freeze.get('model_sha256')!='3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1'):raise RuntimeError('frozen engine/configuration changed')
 heldout=[case for case in suite['cases'] if case['split']=='holdout']
 if len(heldout)!=32 or len({case['id'] for case in heldout})!=32:raise RuntimeError('complete fixed holdout required')
 case=heldout[args.frozen_evaluation_index]
 manifest=json.loads((root/'public/manifest.json').read_text())
 expected=[{key:row[key] for key in ['id','language','file','size','sha256','reference']} for row in heldout]
 if manifest.get('cases')!=expected or manifest.get('dataset')!=suite['dataset'] or manifest.get('revision')!=suite['revision']:raise RuntimeError('frozen public manifest changed')
 args.case=case['id'];args.native_language=case['language'];args.native_right_context=profiles[args.evaluation_profile];args.native_low_latency=True
 marker=root/f'exposure-{args.evaluation_profile}-{args.frozen_evaluation_index}.json'
 with marker.open('x') as output:json.dump({'suite_sha256':stated,'case':args.case,'profile':args.evaluation_profile,'configuration_sha256':receipts['evaluation-freeze.json'],'purpose':'validation only; never tune on these references'},output)
 print('FROZEN_EVALUATION_EXPOSURE '+json.dumps({'case':args.case,'profile':args.evaluation_profile,'suite_sha256':stated,'configuration_sha256':receipts['evaluation-freeze.json']}),flush=True)
command=['bwrap','--unshare-all','--die-with-parent','--ro-bind','/usr','/usr','--symlink','usr/lib64','/lib64','--symlink','usr/lib','/lib','--proc','/proc','--dev','/dev','--tmpfs','/tmp','--dir','/review','--ro-bind',str(root),'/review','--ro-bind',str(root/'worker'),'/review/tilo-speech-worker','--clearenv','--setenv','LANG','C.UTF-8','--setenv','MALLOC_ARENA_MAX','1','--setenv','OMP_NUM_THREADS','2','--setenv','LD_LIBRARY_PATH','/review','--setenv','TILO_TEST_PRIVATE_NAMESPACE','1','--setenv','PYTHONPATH','/review','--chdir','/','/usr/bin/time','-f','ASR_RESOURCE {"user":%U,"system":%S}','/usr/bin/python3','/review/probe.py','--manifest','/review/public/manifest.json','--qualifier','/review/qualifier','--worker','/review/tilo-speech-worker','--model','base','--case','en_us-0','--cpus','2','--first-draft-seconds','2']
if args.whisper_public_review:
 if args.whisper_diagnostics:command.append('--worker-diagnostics')
 if args.expect_attention:command.extend(['--expect-attention',args.expect_attention])
 command[command.index('--model')+1]=args.whisper_model
 if args.whisper_allocator=='default':
  i=command.index('MALLOC_ARENA_MAX');del command[i-1:i+2]
 command[command.index('--case')+1]=args.case
 if args.native_language:command.extend(['--language',args.native_language])
 for case in args.append_case:command.extend(['--append-case',case])
 for word in args.vocabulary:command.extend(['--vocabulary',word])
 if args.noise_snr_db is not None:command.extend(['--noise-snr-db',str(args.noise_snr_db)])
if args.native_review:
 i=command.index('--qualifier');del command[i:i+2]
 command[command.index('--case')+1]=args.case
 command.extend(['--native-review-model','/review/model.gguf'])
 if args.native_language:command.extend(['--language',args.native_language])
 for case in args.append_case:command.extend(['--append-case',case])
 for word in args.vocabulary:command.extend(['--vocabulary',word])
 if args.native_phase or args.phase_repeat or args.native_low_latency or args.native_right_context!=1:command.extend(['--continuous-audio','--native-right-context',str(args.native_right_context)])
 if args.native_low_latency:command.append('--worker-cadence')
 if args.noise_snr_db is not None:command.extend(['--noise-snr-db',str(args.noise_snr_db)])
 # Discard only this owned object's file pages, so the new scope accounts for
 # subsequent reads. fadvise is advisory; do not claim physical cold-device IO.
 fd=os.open(root/'model.gguf',os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW)
 try:os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED)
 finally:os.close(fd)
 print('NATIVE_OWNED_CACHE_ADVICE '+json.dumps({'requested':'DONTNEED on only owned immutable model','cold_device_claim':False}),flush=True)
if args.lifecycle:
 i=command.index('/review/probe.py')
 command=command[:i]+['/review/lifecycle.py','--manifest','/review/public/manifest.json','--worker','/review/tilo-speech-worker','--model','/review/model.gguf','--native-right-context',str(args.native_right_context)]
 if args.identity_review:command.append('--identity-review')
if args.resident_second_case:
 i=command.index('/review/probe.py')
 command=command[:i]+['/review/resident.py','--manifest','/review/public/manifest.json','--worker','/review/tilo-speech-worker','--model','/review/model.gguf','--case',args.case,'--second-case',args.resident_second_case]
for attempt in range(2 if args.phase_repeat else 1):
 result=probe.observe(command,sorted(os.sched_getaffinity(0))[:2])
 after=events();pressure_after=(cg/'memory.pressure').read_text()
 result.update(attempt=attempt+1,phase_repeat=args.phase_repeat,cache_advice_before_this_attempt=attempt==0,scope_events_before=before,scope_events_after=after,scope_pressure_before=pressure_before,scope_pressure_after=pressure_after,host_pressure_before=host_pressure,host_pressure_after=Path('/proc/pressure/memory').read_text(),memory_peak_bytes=int((cg/'memory.peak').read_text()),materialization_sha256=receipts,timing_classification='pressured-builder development; not physical latency qualification',io_isolation=False,native_review=args.native_review)
 print('SEALED_WORKER_SCOPE_RESULT '+json.dumps(result),flush=True)
 if result['exit']!=0 or result['guarded'] or any(after[k]!=before[k] for k in ['max','oom','oom_kill']):raise RuntimeError('guarded worker qualification failed')
