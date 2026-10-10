import hashlib,json,os,shutil,subprocess,tempfile,time,signal,sys
from pathlib import Path
assert os.getpid()==1,"private outer PID namespace init required"
b=Path(sys.argv[1]);s=b/'src';out=s/'target/dictation-recorded-compositor-20261009'
public=s/'target/dictation-base-flash-attested-20261008/public'
v=json.loads((public/'manifest.json').read_text());model_info=v['models']['base'];model_source=public/model_info['file'];assert model_source.stat().st_size==model_info['size'];assert hashlib.sha256(model_source.read_bytes()).hexdigest()==model_info['sha256']
case=next(c for c in v['cases'] if c['id']=='en_us-0');wave=public/case['file'];assert wave.stat().st_size==case['size'];assert hashlib.sha256(wave.read_bytes()).hexdigest()==case['sha256']
rpm=json.loads((out/'rpm-provenance.json').read_text());audio=out/'audio-tools'
for name,digest in rpm['extracted_files'].items():assert hashlib.sha256((audio/name).read_bytes()).hexdigest()==digest,name
owner=None;receipt=None
with tempfile.TemporaryDirectory(prefix='tilo-recorded-isolated-') as temp:
 stage=Path(temp);os.chmod(stage,0o755);os.chown(stage,65534,65534)
 private_wave=stage/'public.wav';shutil.copyfile(wave,private_wave);private_wave.chmod(0o644)
 import importlib.util
 spec=importlib.util.spec_from_file_location('cef_package',s/'scripts/browser/build-host.py');package=importlib.util.module_from_spec(spec);spec.loader.exec_module(package)
 packaged=stage/'runtime';packaged.mkdir();package.install_runtime(s/'target/dictation-browser-paragraph-20261009/rich-island-runtime',packaged)
 assert not (packaged/'chromium/chrome-sandbox').exists(),'maintained runtime must use user namespace confinement'
 model=stage/'model.bin';shutil.copyfile(model_source,model);os.chown(model,65534,65534);model.chmod(0o644)
 try:
  owner=subprocess.Popen([str(b/'target/debug/examples/speech_qualify'),'--hold-fixture',str(model)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,user=65534,group=65534,extra_groups=[])
  import selectors
  with selectors.DefaultSelector() as selector:
   selector.register(owner.stdout,selectors.EVENT_READ);assert selector.select(15),'held model fixture did not acknowledge'
   line=owner.stdout.readline(65537);assert line and len(line)<=65536,'missing bounded held fixture receipt'
  receipt=json.loads(line);assert receipt['ephemeral'] and receipt['model_sha256']==model_info['sha256']
  for name,source,destination in [('bin','/usr/bin','/host-bin'),('share','/usr/share','/host-share')]:
   directory=stage/name;directory.mkdir();os.chown(directory,65534,65534)
   for item in Path(source).iterdir():
    if item.name in ['tilo-speech-worker']:continue
    (directory/item.name).symlink_to(destination+'/'+item.name)
  for item in (audio/'usr/bin').iterdir():
   dest=stage/'bin'/item.name
   if dest.exists() or dest.is_symlink():dest.unlink()
   dest.symlink_to('/fixture-audio/usr/bin/'+item.name)
  (stage/'bin/tilo-speech-worker').symlink_to('/rust/debug/tilo-speech-worker')
  extensions=stage/'share/tilo/system-extensions';extensions.mkdir(parents=True)
  (extensions/'local-model-catalog.json').symlink_to('/fixture/catalog.json');(extensions/'local-model-keys.json').symlink_to('/fixture/keys.json')
  command=['bwrap','--unshare-user','--unshare-ipc','--unshare-uts','--unshare-net','--die-with-parent','--new-session','--ro-bind','/usr','/usr','--ro-bind','/lib','/lib','--ro-bind','/lib64','/lib64','--ro-bind','/etc','/etc','--bind','/proc','/proc','--dev','/dev','--tmpfs','/tmp','--ro-bind','/usr/bin','/host-bin','--ro-bind','/usr/share','/host-share','--ro-bind',str(stage/'bin'),'/usr/bin','--ro-bind',str(stage/'share'),'/usr/share','--ro-bind',str(b/'target'),'/rust','--ro-bind',str(Path(receipt['root']).parent),'/fixture','--ro-bind',receipt['root'],'/model-data/tilo/models','--ro-bind',str(out/'recorded-inner.py'),'/assembly.py','--ro-bind',str(private_wave),'/public.wav','--ro-bind',str(audio),'/fixture-audio','--ro-bind',str(packaged/'chromium'),'/fixture-cef','--clearenv','--setenv','XDG_DATA_HOME','/model-data','--setenv','LANG','C.UTF-8','--setenv','PATH','/usr/bin','--','/usr/bin/python3','/assembly.py']
  before=time.monotonic();p=subprocess.run(command,stdin=subprocess.DEVNULL,user=65534,group=65534,extra_groups=[],timeout=215)
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
