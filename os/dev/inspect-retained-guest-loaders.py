#!/usr/bin/env python3
"""Inspect pinned guest7532 public loader bytes; never start its container."""
import argparse,hashlib,io,json,os,posixpath,re,selectors,subprocess,tarfile,time
from pathlib import Path
REFERENCE='ghcr.io/tilo-os/tilo-development-windows-guest@sha256:592518b08a4c88395b7054c293eab9aeb52750869aba3a59dbe87d98556ae765'
CONFIG='sha256:98cc5759016c9371d67a5374c34445397ec9ca4910b7792c6091c677fb3ef720'
LOADERS=('usr/bin/wine32','usr/bin/wine64','usr/lib64/wine/i386-unix/wine','usr/lib/wine/x86_64-unix/wine','usr/lib/wine/i386-unix/wine','usr/lib64/wine/x86_64-unix/wine')
INVENTORY='usr/lib/tilo/windows-guest/installed-packages.tsv'
PREFIXES={'usr/lib64/wine/i386-unix':'/usr/lib/wine/i386-unix','usr/lib/wine/x86_64-unix':'/usr/lib64/wine/x86_64-unix'}
def run(argv,limit=1024*1024,timeout=30):
 process=subprocess.Popen(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE);data=bytearray();errors=bytearray();deadline=time.monotonic()+timeout
 try:
  with selectors.DefaultSelector() as sel:
   sel.register(process.stdout,selectors.EVENT_READ,data);sel.register(process.stderr,selectors.EVENT_READ,errors)
   while sel.get_map():
    if time.monotonic()>deadline:raise TimeoutError('inspection command deadline')
    for key,_ in sel.select(.2):
     block=os.read(key.fileobj.fileno(),65536)
     if not block:sel.unregister(key.fileobj);continue
     key.data.extend(block)
     if len(data)>limit or len(errors)>65536:raise ValueError('inspection output bound')
  status=process.wait(timeout=5)
  if status:raise ValueError('inspection command failed: '+str(argv[0])+': '+errors.decode(errors='replace')[-512:])
  return bytes(data)
 finally:
  if process.poll() is None:process.kill()
  process.wait();process.stdout.close();process.stderr.close()
def member(raw):
 with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
  entries=tar.getmembers()
  if len(entries)!=1:raise ValueError('one file expected from docker cp')
  item=entries[0]
  if item.issym():return {'type':'symlink','target':item.linkname},None
  if not item.isfile() or item.size>16*1024*1024:raise ValueError('bounded regular loader required')
  data=tar.extractfile(item).read()
  return {'type':'file','size':len(data),'sha256':hashlib.sha256(data).hexdigest()},data
def elf(data):
 if len(data)<64 or data[:4]!=b'\x7fELF' or data[4] not in (1,2) or data[5]!=1:raise ValueError('little-endian ELF32/64 required')
 machine=int.from_bytes(data[18:20],'little')
 if (data[4],machine) not in ((1,3),(2,62)):raise ValueError('expected x86 loader architecture')
 return {'bits':32 if data[4]==1 else 64,'machine':machine}
def main():
 parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);args=parser.parse_args()
 args.output.mkdir(parents=True,exist_ok=False);container=None
 result={'schema':1,'reference':REFERENCE,'expected_config':CONFIG,'guest_executed':False,'files':{}}
 try:
  # Pull may be large, but occurs only on the registry-local disposable runner.
  run(['docker','pull','--platform=linux/amd64',REFERENCE],limit=1024*1024,timeout=600)
  image=json.loads(run(['docker','image','inspect',REFERENCE]))[0]
  if image['Id']!=CONFIG or (image['Os'],image['Architecture'])!=('linux','amd64'):raise ValueError('pinned guest config/platform differs')
  manifest=run(['docker','buildx','imagetools','inspect','--raw',REFERENCE])
  if hashlib.sha256(manifest).hexdigest()!=REFERENCE.split('@sha256:')[1] or json.loads(manifest)['config']['digest']!=CONFIG:raise ValueError('immutable manifest differs')
  container=run(['docker','create','--network=none','--entrypoint=/unused',REFERENCE]).decode().strip()
  if not re.fullmatch('[0-9a-f]{64}',container):raise ValueError('invalid owned container identity')
  for path,expected in PREFIXES.items():
   record,data=member(run(['docker','cp',container+':/rootfs/'+path,'-'],limit=65536))
   if record!={'type':'symlink','target':expected} or data is not None:raise ValueError('alternate loader prefix differs: '+path)
   result['files'][path]=record
  for path in (*LOADERS,INVENTORY):
   try:record,data=member(run(['docker','cp',container+':/rootfs/'+path,'-'],limit=17*1024*1024))
   except ValueError as error:
    result['files'][path]={'unavailable':str(error)};continue
   result['files'][path]=record
   if data is None:continue
   target=args.output/path.replace('/','__')
   if path==INVENTORY:
    rows=[line for line in data.decode().splitlines() if 'wine' in line.lower()]
    (args.output/'wine-installed-packages.tsv').write_text('\n'.join(rows)+'\n');record['wine_rows']=rows;continue
   record.update(elf(data));target.write_bytes(data);target.chmod(0o600)
   report=run(['readelf','-n','-d','-lW',str(target)],limit=1024*1024)
   (target.with_suffix('.readelf.txt')).write_bytes(report)
  for path,record in result['files'].items():
   if record.get('type')=='symlink':
    target=record['target'];resolved=posixpath.normpath(target.lstrip('/') if target.startswith('/') else posixpath.join(posixpath.dirname(path),target))
    record['guest_relative_target']=resolved;record['target_in_inspected_set']=resolved in result['files']
  for path,record in result['files'].items():
   chain=[];current=path
   for _ in range(8):
    row=result['files'].get(current,{})
    if row.get('type')!='symlink':break
    if current in chain:raise ValueError('loader symlink loop')
    chain.append(current);current=row['guest_relative_target']
   if chain:
    record['inspected_chain']=chain+[current]
    record['resolved_sha256']=result['files'].get(current,{}).get('sha256')
  for prefix,target in PREFIXES.items():
   alternate=prefix+'/wine';canonical=target.lstrip('/')+'/wine';resolved=result['files'].get(canonical,{})
   result['files'][alternate]['guest_prefix_resolution']={'prefix':prefix,'target':target,'canonical':canonical,'sha256':resolved.get('sha256'),'bits':resolved.get('bits')}
   if result['files'][alternate].get('type')=='file' and result['files'][alternate]['sha256']!=resolved.get('sha256'):raise ValueError('alternate loader bytes differ from mapped canonical loader')
  for path,bits in [('usr/bin/wine32',32),('usr/bin/wine64',64),('usr/lib/wine/i386-unix/wine',32),('usr/lib64/wine/x86_64-unix/wine',64)]:
   if result['files'].get(path,{}).get('bits')!=bits:raise ValueError('canonical loader width missing/differs: '+path)
  if 'wine_rows' not in result['files'].get(INVENTORY,{}):raise ValueError('package inventory unavailable')
  result['canonical_widths_verified']=True
 finally:
  if container:
   run(['docker','rm',container]);result['owned_container_removed']=True
  (args.output/'inspection.json').write_text(json.dumps(result,indent=2)+'\n')
if __name__=='__main__':main()
