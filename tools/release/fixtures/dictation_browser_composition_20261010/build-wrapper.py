from pathlib import Path
import fcntl,hashlib,json,os,shutil,stat,subprocess,tempfile,time,uuid
INPUTS={'head': '3b54993ac921d7dd33aa6b743ed650aa3d148a53', 'source_hashes': {'apps/browser/chromium/src/host.cc': '11de5ba9110c68cc88cb9c962284942669206c9daf17129afd63169f2d13ae5a', 'apps/browser/chromium/src/page_automation.js': '6c6b94597687b8c11466528f9dc3199e283035c2e3a282d1432c81a1581e2ce0', 'apps/browser/chromium/tests/guarded_text_input.py': '0fd1aced9b1669e998769dff6c1e34643e0c7ee232dee6708f3c9f945ce8b99b', 'apps/browser/chromium/tests/native_composition.py': 'f10ecffcf9a637f29361a3d56bb1c8ae893bf29529c930e34b0c99927e964ac8'}}
BASE=Path('${OWNED_BUILDER_WORKTREE}')
OWNER=Path('${CEF_OWNER_ROOT}')
SDKNAME='cef_binary_152.0.6+g708dc14+chromium-152.0.7977.83_linux64_tilo-40a39125d44e97d1_minimal'
SDK=OWNER/'native-qualification-revision-two/native-output/sdk'/SDKNAME
IMAGE='sha256:3e643adc70d3dc2b6c3f3fecf0944f5c9273e0ac1aef0c382b5ca208456a67b8'
def digest(p):
 with Path(p).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
lockpath=Path('${OWNED_CUSTODY_LOCK}')
fd=os.open(lockpath,os.O_RDWR|os.O_NOFOLLOW|os.O_CLOEXEC);st=os.fstat(fd)
assert stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and not st.st_mode&0o022
fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
assert (st.st_dev,st.st_ino)==(lockpath.stat().st_dev,lockpath.stat().st_ino)
for p,h in INPUTS['source_hashes'].items():assert digest(BASE/'src'/p)==h,p
blocker=BASE/'target/thin-app/libtilo_chromium_blocker.a';assert blocker.is_file()
assert digest(SDK/'Release/libcef.so')=='c8c3c51ebcb086b5b22cfdcdc07115826332ed3c6e038b316ec593951cf4e3da'
receipt=json.loads((OWNER/'candidate-packages-A/candidate-receipt.json').read_text())
assert receipt['archive_sha256']=='cece35681a153ef16446214a8be35a766a2f9f64a5b9149c63c900857a70c57f' and receipt['archive_size']==167327081
# Verify extracted SDK against the exact pinned archive inventory before reuse.
for item in receipt['inventory']:
 p=SDK.parent/item['path'];assert p.lstat().st_mode==item['mode'],item['path']
 if 'link' in item:assert p.is_symlink() and str(p.readlink())==item['link'] and p.resolve().is_relative_to(SDK.resolve())
 else:assert p.is_file() and p.stat().st_size==item['size'] and digest(p)==item['sha256'],item['path']
assert subprocess.run(['podman','image','exists',IMAGE],capture_output=True).returncode==0
fs=os.statvfs('/tmp');assert fs.f_bavail*fs.f_frsize>10*1024**3,'Own output headroom unavailable'
out=Path(tempfile.mkdtemp(prefix='tilo-dictation-browser-composition-'));print('OWNED_REVIEW',out,flush=True)
(out/'source/apps/browser').mkdir(parents=True);shutil.copytree(BASE/'src/apps/browser/chromium',out/'source/apps/browser/chromium')
(out/'source/scripts/browser').mkdir(parents=True);shutil.copyfile(BASE/'src/scripts/browser/build-host.py',out/'source/scripts/browser/build-host.py')
shutil.copyfile(blocker,out/'libtilo_chromium_blocker.a');(out/'home').mkdir()
fcntl.flock(fd,fcntl.LOCK_UN);os.close(fd)
inner=out/'build.py';inner.write_text("""from pathlib import Path
import importlib.util,subprocess,shutil
r=Path('/review');src=r/'source/apps/browser/chromium'
subprocess.run(['cmake','-S',str(src),'-B',str(r/'cmake-build'),'-DCEF_ROOT=/sdk','-DTILO_BLOCKER_LIBRARY='+str(r/'libtilo_chromium_blocker.a'),'-DCMAKE_BUILD_TYPE=Release','-DTILO_OUTPUT_DIR='+str(r/'host')],check=True)
subprocess.run(['cmake','--build',str(r/'cmake-build'),'--parallel','2'],check=True)
spec=importlib.util.spec_from_file_location('packager',r/'source/scripts/browser/build-host.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.install_runtime(r/'host',r/'runtime',None)
assert not (r/'runtime/chromium/chrome-sandbox').exists()
""")
name='dictation-browser-composition-'+uuid.uuid4().hex[:12]
command=['podman','run','--rm','--pull=never','--init','--name',name,'--network=none','--userns=keep-id','--user=1000:1000','--cap-drop=ALL','--security-opt=no-new-privileges','--security-opt=label=disable','--read-only','--tmpfs','/tmp:rw,nosuid,nodev,size=512m','--cpus=2','--memory=4g','--memory-swap=4g','--pids-limit=256','--ulimit','core=0:0','-e','HOME=/review/home','-e','PYTHONDONTWRITEBYTECODE=1','-v',str(out)+':/review:rw','-v',str(SDK)+':/sdk:ro',IMAGE,'python3','/review/build.py']
start=time.monotonic();build_exit=None
try:
 with (out/'build.log').open('wb') as log:run=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,timeout=420);build_exit=run.returncode
finally:
 if subprocess.run(['podman','container','exists',name],capture_output=True).returncode==0:subprocess.run(['podman','stop','--time','3',name],capture_output=True,timeout=15)
 assert subprocess.run(['podman','container','exists',name],capture_output=True).returncode!=0,'Owned container leaked'
r={**INPUTS,'existing_corrected_sdk':str(SDK),'sdk_archive_sha256':receipt['archive_sha256'],'sdk_archive_size':receipt['archive_size'],'sdk_inventory_verified':len(receipt['inventory']),'sdk_read_only':True,'production_pins_changed':False,'image':IMAGE,'blocker_sha256':digest(out/'libtilo_chromium_blocker.a'),'build_exit':build_exit,'build_seconds':time.monotonic()-start,'owned_container_verified_absent':name,'build_log_sha256':digest(out/'build.log'),'owned_review':str(out)}
if build_exit==0:r['host_sha256']=digest(out/'runtime/chromium/tilo-chromium-host');r['runtime_libcef_sha256']=digest(out/'runtime/chromium/libcef.so')
(out/'build-receipt.json').write_text(json.dumps(r,indent=2)+'\n');print(json.dumps(r),flush=True)
print((out/'build.log').read_text()[-5000:],flush=True)
assert build_exit==0,'Preserved build failure; no native tests started'
