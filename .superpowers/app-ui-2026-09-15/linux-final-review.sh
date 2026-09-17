#!/bin/bash
set -euo pipefail
review_share=/media/psf/Home/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15
review_run=${1:?Provide immutable final run directory name}
review_src=/home/parallels/qos-ui-review-$review_run
review_share=$review_share/$review_run
if [[ -e "$review_src" ]]; then
  echo "Source directory already exists; choose a fresh review run: $review_src" >&2
  exit 1
fi
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
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_BUILD_JOBS=4
cargo build --locked --no-default-features -p osk -p windows-app -p vpn --bins
cargo test --locked --no-default-features -p vpn --lib
python3 - "$review_share" <<'CAPTURE'
from pathlib import Path
import subprocess,os,json,hashlib,sys,struct
root=Path(sys.argv[1]);records=[]
for name in ['osk','windows']:
 out=root/'captures'/name;out.mkdir(parents=True,exist_ok=True)
 binary=Path('/home/parallels/qos-target/debug')/name
 binary_digest=hashlib.sha256(binary.read_bytes()).hexdigest()
 rows=[]
 for variant,viewport,scale in [('wide','1200x800',1),('compact','800x700',1),('narrow','480x900',1),('large-text','800x900',2),('scale-baseline','800x900',1),('short','800x480',1)]:
  profile=out/(variant+'-profile');profile.mkdir(exist_ok=True)
  command=[str(binary),'--shots',str(out/(variant+'.png')),'--viewport',viewport,'--text-scale',str(scale)]
  env=dict(os.environ,HOME=str(profile),XDG_DATA_HOME=str(profile/'data'),XDG_CONFIG_HOME=str(profile/'config'),XDG_CACHE_HOME=str(profile/'cache'),XDG_STATE_HOME=str(profile/'state'),QUICK_UI_VELLO_CPU='1',QOS_SHOTS_THEME='dark',QOS_SHOTS_TEXT_SCALE=str(scale))
  with (out/(variant+'.log')).open('w') as log:r=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=90)
  png=out/(variant+'.png');actual=None;png_digest=None
  if png.is_file():
   data=png.read_bytes()
   if data[:8]==b'\x89PNG\r\n\x1a\n':actual=struct.unpack('>II',data[16:24])
   png_digest=hashlib.sha256(data).hexdigest()
  status='captured' if r.returncode==0 and actual==tuple(map(int,viewport.split('x'))) else 'failed'
  rows.append(dict(variant=variant,command=command,exit_code=r.returncode,status=status,path=str(png),actual_dimensions=actual,png_sha256=png_digest,requested_scale=scale))
 final_digest=hashlib.sha256(binary.read_bytes()).hexdigest()
 record=dict(app='apps/'+name,platform='Fedora44 ARM64',source_manifest='linux-source-hashes.json',binary_sha256=binary_digest,binary_sha256_after=final_digest,binary_stable=binary_digest==final_digest,captures=rows)
 (out/'manifest.json').write_text(json.dumps(record,indent=2));records.append(record)
 print('Linux capture results',name,[row['status'] for row in rows],flush=True)
(root/'linux-capture-results.json').write_text(json.dumps(records,indent=2))
if any(not record['binary_stable'] or any(row['status']!='captured' for row in record['captures']) for record in records):
 raise SystemExit('One or more Linux captures failed, used the wrong viewport, or changed binary during capture')
CAPTURE
