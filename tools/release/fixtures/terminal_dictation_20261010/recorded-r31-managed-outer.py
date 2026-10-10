import hashlib,json,os,shutil,subprocess,tempfile,time,signal,sys
from pathlib import Path
assert os.getpid()==1,"private outer PID namespace init required"
b=Path(sys.argv[1]);s=b/'src';out=Path(sys.argv[2])
public=Path('${PUBLIC_ROOT}');v=json.loads((out/'recorded-manifest.json').read_text());model_info=v['model'];model_source=public/model_info['file']
assert model_source.stat().st_size==model_info['size'];assert hashlib.sha256(model_source.read_bytes()).hexdigest()==model_info['sha256']
case=v['cases'][0];wave=public/case['file'];assert wave.stat().st_size==case['size'];assert hashlib.sha256(wave.read_bytes()).hexdigest()==case['sha256']
for name,pin in v['binaries'].items():
 q=Path(pin['path']);assert q.stat().st_size==pin['bytes'] and hashlib.file_digest(q.open('rb'),'sha256').hexdigest()==pin['sha256'],name
for name,digest in v['sources'].items():assert hashlib.sha256((s/name).read_bytes()).hexdigest()==digest,name
audio=public/'audio-tools';rpm=json.loads((public/'audio-tools-receipt.json').read_text());assert rpm['package']['signature_verified_before_use']
for name,digest in rpm['extracted_files'].items():assert hashlib.sha256((audio/name).read_bytes()).hexdigest()==digest,name
import ctypes
parent_status=Path('/proc/self/status').read_text();print('OUTER_PRIVATE_CAPABILITIES',next(l for l in parent_status.splitlines() if l.startswith('CapEff:')),flush=True)
ready_r,ready_w=os.pipe();release_r,release_w=os.pipe();keeper=os.fork()
if keeper==0:
 os.close(ready_r);os.close(release_w)
 libc=ctypes.CDLL(None,use_errno=True)
 if libc.unshare(0x10000000)!=0:os.write(ready_w,b'E');os._exit(1)
 os.write(ready_w,b'R');os.close(ready_w)
 while os.read(release_r,1):pass
 os.close(release_r);os._exit(0)
os.close(ready_w);os.close(release_r)
try:
 assert os.read(ready_r,1)==b'R','private namespace creation failed'
 Path(f'/proc/{keeper}/uid_map').write_text('0 0 1\n65534 65534 1\n')
 Path(f'/proc/{keeper}/gid_map').write_text('0 0 1\n65534 65534 1\n')
 namespace=os.open(f'/proc/{keeper}/ns/user',os.O_RDONLY|os.O_CLOEXEC)
 print('PRIVATE_IMAGE_USER_MAP',Path(f'/proc/{keeper}/uid_map').read_text(),flush=True)
except BaseException:
 os.close(release_w);os.waitpid(keeper,0);raise
finally:os.close(ready_r)
owner=None;receipt=None
with tempfile.TemporaryDirectory(prefix='tilo-recorded-isolated-') as temp:
 stage=Path(temp);os.chmod(stage,0o755);os.chown(stage,65534,65534)
 data=stage/'model-data';(data/'tilo').mkdir(parents=True)
 for folder in [data,data/'tilo']:os.chown(folder,65534,65534);folder.chmod(0o755)
 assert case['language']=='de'
 case_path=stage/'case.json';case_path.write_text(json.dumps(case));case_path.chmod(0o644)
 drop_path=stage/'drop.py';drop_path.write_text("import ctypes,os,runpy\nlibc=ctypes.CDLL(None,use_errno=True)\nassert libc.prctl(38,1,0,0,0)==0\nos.chmod('/tmp',0o1777);os.chmod('/dev/shm',0o1777);os.chmod('/model-data',0o755);os.chmod('/model-data/tilo',0o755)\nos.setgroups([]);os.setgid(65534);os.setuid(65534)\nassert os.getuid()==65534 and os.getgid()==65534 and os.getgroups()==[]\nrunpy.run_path('/assembly.py',run_name='__main__')\n");drop_path.chmod(0o644)
 inner_path=stage/'assembly.py';shutil.copyfile(out/'managed-inner.py',inner_path);inner_path.chmod(0o644)
 runtime_path=stage/'lib64';runtime_path.mkdir(parents=True)
 for p in [runtime_path,*runtime_path.rglob('*')]:p.chmod(0o755 if p.is_dir() else 0o644)
 private_wave=stage/'public.wav';shutil.copyfile(wave,private_wave);private_wave.chmod(0o644)
 model=stage/'model.bin';shutil.copyfile(model_source,model);os.chown(model,65534,65534);model.chmod(0o644)
 try:
  owner=subprocess.Popen([str(b/'target/debug/examples/speech_qualify'),'--hold-fixture',str(model)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,user=65534,group=65534,extra_groups=[])
  import selectors
  with selectors.DefaultSelector() as selector:
   selector.register(owner.stdout,selectors.EVENT_READ);assert selector.select(15),'held model fixture did not acknowledge'
   line=owner.stdout.readline(65537);assert line and len(line)<=65536,'missing bounded held fixture receipt'
  receipt=json.loads(line);assert receipt['ephemeral'] and receipt['model_sha256']==model_info['sha256']
  for name,source,destination in [('bin','/usr/bin','/host-bin'),('share','/usr/share','/host-share')]:
   directory=stage/name;directory.mkdir(exist_ok=True);os.chown(directory,0 if name=='lib64' else 65534,0 if name=='lib64' else 65534)
   for item in Path(source).iterdir():
    if item.name in ['tilo-speech-worker'] or (name=='lib64' and item.name=='tilo-speech'):continue
    (directory/item.name).symlink_to(destination+'/'+item.name)
  # The managed shell's immutable /usr must contain actual executables,
  # rather than symlinks crossing into the outer fixture's /host-bin.
  for name in ['bash','sh','bwrap','env','python3']:
   source=Path('/usr/bin')/name;dest=stage/'bin'/name
   if dest.exists() or dest.is_symlink():dest.unlink()
   shutil.copy2(source.resolve(),dest)
   assert hashlib.sha256(dest.read_bytes()).digest()==hashlib.sha256(source.read_bytes()).digest()
  # Preserve pinned-image terminal capability data inside the original
  # managed sandbox; do not expose the fixture's external /host-share.
  term_root=Path('/usr/share/terminfo')
  term_files=list(term_root.glob('*/xterm-256color'))
  assert len(term_files)==1,'pinned-image xterm capability file must be observable'
  term_source=term_files[0];assert 0<term_source.stat().st_size<65536
  term_dest=stage/'share/terminfo'
  if term_dest.is_symlink():term_dest.unlink()
  term_target=term_dest/term_source.relative_to(term_root)
  term_target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(term_source.resolve(),term_target)
  term_dest.chmod(0o755);term_target.parent.chmod(0o755);term_target.chmod(0o644)
  term_sha=hashlib.sha256(term_source.read_bytes()).hexdigest()
  assert hashlib.sha256(term_target.read_bytes()).hexdigest()==term_sha
  print('MOUNTED_FIXTURE_TERMINFO',json.dumps({'file':str(term_source),'sha256':term_sha,'bytes':term_source.stat().st_size,'installed':False,'original_sandbox_preserved':True}),flush=True)
  for item in (audio/'usr/bin').iterdir():
   dest=stage/'bin'/item.name
   if dest.exists() or dest.is_symlink():dest.unlink()
   dest.symlink_to('/fixture-audio/usr/bin/'+item.name)
  (stage/'bin/tilo-speech-worker').symlink_to('/rust/debug/tilo-speech-worker')
  extensions=stage/'share/tilo/system-extensions';extensions.mkdir(parents=True)
  (extensions/'local-model-catalog.json').symlink_to('/fixture/catalog.json');(extensions/'local-model-keys.json').symlink_to('/fixture/keys.json')
  command=['bwrap','--userns',str(namespace),'--cap-add','CAP_SETUID','--cap-add','CAP_SETGID','--unshare-ipc','--unshare-uts','--unshare-net','--die-with-parent','--new-session','--ro-bind','/usr','/usr','--ro-bind','/lib','/lib','--ro-bind','/lib64','/lib64','--ro-bind','/etc','/etc','--bind','/proc','/proc','--dev','/dev','--tmpfs','/tmp','--ro-bind','/usr/bin','/host-bin','--ro-bind','/usr/share','/host-share','--ro-bind',str(stage/'bin'),'/usr/bin','--ro-bind',str(stage/'share'),'/usr/share','--ro-bind',str(b/'target'),'/rust','--ro-bind',str(Path(receipt['root']).parent),'/fixture','--bind',str(data),'/model-data','--ro-bind',receipt['root'],'/model-data/tilo/models','--ro-bind',str(drop_path),'/drop.py','--ro-bind',str(inner_path),'/assembly.py','--ro-bind',str(case_path),'/case.json','--ro-bind',str(private_wave),'/public.wav','--ro-bind',str(audio),'/fixture-audio','--ro-bind',str(out/'host-root'),'/qual-host','--ro-bind',str(s),'/source','--ro-bind',str(s/'scripts/legacy-app-caret-e2e.py'),'/legacy-caret.py','--ro-bind',str(s/'tools/release/summarize_speech_benchmark.py'),'/speech-score.py','--clearenv','--setenv','XDG_DATA_HOME','/model-data','--setenv','LANG','C.UTF-8','--setenv','PATH','/usr/bin','--','/usr/bin/python3','/drop.py']
  before=time.monotonic();p=subprocess.run(command,stdin=subprocess.DEVNULL,pass_fds=(namespace,),timeout=300)
  print('OUTER_JOURNEY',json.dumps({'exit':p.returncode,'elapsed_seconds':round(time.monotonic()-before,3),'model_sha256':model_info['sha256'],'audio_sha256':case['sha256'],'ephemeral':True}),flush=True)
  if p.returncode:raise RuntimeError('assembled recorded-audio journey failed')
 finally:
  if owner is not None:
   owner.stdin.close()
   try:owner.wait(timeout=10)
   except subprocess.TimeoutExpired:owner.kill();owner.wait(timeout=5)
   owner.stdout.close();owner.stderr.close()
  if receipt is not None:assert not Path(receipt['root']).exists(),'ephemeral model witness was not removed'
  print('PASS ephemeral model witness removed; no installed store/catalogue changed',flush=True)
  os.close(namespace);os.close(release_w);_,keeper_status=os.waitpid(keeper,0);assert os.waitstatus_to_exitcode(keeper_status)==0,'owned user namespace keeper failed'
  print('PASS private mapped-image namespace keeper reaped',flush=True)
