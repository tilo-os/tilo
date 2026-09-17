#!/bin/bash
set -euo pipefail
review_share=/media/psf/Home/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15
review_src=/home/parallels/qos-ui-review-20260915
mkdir -p "$review_src"
tar -xzf "$review_share/linux-source.tar.gz" -C "$review_src"
python3 - "$review_src" "$review_share/linux-source-hashes.json" <<'CHECK'
from pathlib import Path
import json,hashlib,sys
root=Path(sys.argv[1]); hashes=json.loads(Path(sys.argv[2]).read_text())
for p,h in hashes.items():
 assert hashlib.sha256((root/p).read_bytes()).hexdigest()==h,p
print('Verified immutable Linux source',len(hashes),flush=True)
CHECK
cd "$review_src"
export PATH=/home/parallels/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/home/parallels/qos-target
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_BUILD_JOBS=4
cargo build --locked --no-default-features -p osk -p windows-app --bins
python3 - "$review_share" <<'CAPTURE'
from pathlib import Path
import subprocess,os,json,hashlib,sys
root=Path(sys.argv[1]);records=[]
for name in ['osk','windows']:
 out=root/'captures'/name;out.mkdir(exist_ok=True)
 binary=Path('/home/parallels/qos-target/debug')/name
 rows=[]
 for variant,viewport,scale in [('wide','1200x800',1),('compact','800x700',1),('narrow','480x900',1),('large-text','800x900',2)]:
  profile=out/(variant+'-profile');profile.mkdir(exist_ok=True)
  command=[str(binary),'--shots',str(out/(variant+'.png')),'--viewport',viewport,'--text-scale',str(scale)]
  env=dict(os.environ,HOME=str(profile),XDG_DATA_HOME=str(profile/'data'),XDG_CONFIG_HOME=str(profile/'config'),QUICK_UI_VELLO_CPU='1',QOS_SHOTS_TEXT_SCALE=str(scale))
  with (out/(variant+'.log')).open('w') as log:r=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=90)
  rows.append(dict(variant=variant,command=command,exit_code=r.returncode))
 record=dict(app='apps/'+name,platform='Fedora44 ARM64',source_manifest='linux-source-hashes.json',binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),captures=rows)
 (out/'manifest.json').write_text(json.dumps(record,indent=2));records.append(record)
 print('CapturedLinux',name,flush=True)
(root/'linux-capture-results.json').write_text(json.dumps(records,indent=2))
CAPTURE
