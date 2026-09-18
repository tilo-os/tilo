#!/usr/bin/env python3
"""Retain public Kbuild output after failed make, without changing its exit code."""
import argparse,hashlib,io,json,os,re,shutil,signal,stat,subprocess,sys,tarfile,tempfile,time
from pathlib import Path
SOURCE='33e83f64cde9df4ee65ff47a708f125d932256da'
PRIVATE=re.compile(br'-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----|"secret_key_hex"\s*:')
MAX_BYTES=8*1024**3
MAX_FILES=250000

def require(ok,message):
 if not ok:raise ValueError(message)
def sha(path):
 with Path(path).open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()
def encoded(value):return json.dumps(value,sort_keys=True,separators=(',',':')).encode()
def generated(name):
 p=Path(name)
 return (name.endswith(('.a','.ko')) or name in ['vmlinux','vmlinux.o','vmlinux.a','System.map','Module.symvers','modules.order','arch/arm64/boot/Image']
         or p.parent==Path('certs') and (p.name.endswith('.o') or p.name in ['signing_key.x509','x509_certificate_list','x509_revocation_list']))

def capture(root,output,run_id,attempt,exit_code,checkpoint,*,reserve=8*1024**3,srpm='/work/kernel.src.rpm'):
 root=Path(root);output=Path(output);checkpoint=Path(checkpoint)
 require(root.is_dir() and not root.is_symlink() and root.resolve()==root,'canonical kernel root required')
 require(output.is_dir() and not output.is_symlink() and output.resolve()==output and output!=root and root not in output.parents,'separate checkpoint mount required')
 require(not any(output.iterdir()),'fresh run-specific checkpoint cache required')
 require(re.fullmatch('[1-9][0-9]*',run_id) and re.fullmatch('[1-9][0-9]*',attempt) and 0<exit_code<256,'failed run identity required')
 deadline=time.monotonic()+300;entries=[];invalid=[];total=0
 excluded=['tools/testing/selftests','certs/signing_key.pem','certs/signing-key.pem']
 for parent,dirs,files in os.walk(root,followlinks=False):
  dirs[:]=sorted(d for d in dirs if (Path(parent)/d).relative_to(root).as_posix() not in excluded)
  for name in sorted(dirs+files):
   path=Path(parent)/name;rel=path.relative_to(root).as_posix();st=path.lstat()
   require(time.monotonic()<deadline and len(entries)<MAX_FILES,'capture bound exceeded')
   if rel in excluded:continue
   if generated(rel):invalid.append(rel);continue
   require(stat.S_ISREG(st.st_mode) or stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode),'special file rejected')
   if stat.S_ISLNK(st.st_mode):
    require(not os.path.isabs(os.readlink(path)) and root in path.resolve().parents,'escaping link rejected')
   if stat.S_ISREG(st.st_mode):require(st.st_nlink==1,'hardlinked kernel file rejected');total+=st.st_size
   entries.append((path,rel,st))
 require(total<=MAX_BYTES,f'public kernel bytes {total} exceed capture cap {MAX_BYTES}')
 require(shutil.disk_usage(output).free>=total+len(entries)*2048+reserve,'checkpoint scratch reserve unavailable')
 with tempfile.TemporaryDirectory(prefix='.capture-',dir=output) as temporary:
  work=Path(temporary);records=[];hashes={}
  with tarfile.open(work/'tree.tar','w',format=tarfile.PAX_FORMAT) as archive:
   for path,rel,before in entries:
    require(time.monotonic()<deadline,'capture deadline')
    info=archive.gettarinfo(path,arcname=rel);info.pax_headers['mtime']=str(before.st_mtime_ns//10**9)+'.'+str(before.st_mtime_ns%10**9).zfill(9)
    records.append({'path':rel,'size':before.st_size,'mode':stat.S_IMODE(before.st_mode),'mtime_ns':before.st_mtime_ns})
    if info.isfile():
     digest=hashlib.sha256();tail=b''
     with path.open('rb') as stream:
      for chunk in iter(lambda:stream.read(1024*1024),b''):
       require(not PRIVATE.search(tail+chunk),'private marker rejected');tail=chunk[-100:];digest.update(chunk)
      stream.seek(0);archive.addfile(info,stream)
     after=path.stat();require((before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)==(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns),'kernel output changed during capture')
     hashes[rel]=digest.hexdigest()
    else:archive.addfile(info)
   manifest={'schema':1,'purpose':'incomplete-public-kbuild-checkpoint','root':str(root),'files':records,'excluded':excluded,'invalidated':sorted(invalid),
    'fresh_key_required_in_resume_run':True,'writer_audit_completed':False,'quiescence':'owned make process group terminated and reaped',
    'installable':False,'build_success':False}
   data=encoded(manifest);info=tarfile.TarInfo('.tilo-checkpoint.json');info.size=len(data);info.mode=0o600;archive.addfile(info,io.BytesIO(data));hashes[info.name]=hashlib.sha256(data).hexdigest()
  # Verify archive bytes a second time before any ready marker can be exported.
  with tarfile.open(work/'tree.tar','r') as archive:
   for entry in archive:
    require(time.monotonic()<deadline,'capture readback deadline')
    if entry.isfile():
     digest=hashlib.sha256()
     with archive.extractfile(entry) as stream:
      for chunk in iter(lambda:stream.read(1024*1024),b''):
       require(time.monotonic()<deadline,'capture readback deadline');digest.update(chunk)
     require(digest.hexdigest()==hashes[entry.name],'archive readback differs')
  scan={'schema':1,'purpose':'scanned-incomplete-kbuild-checkpoint','regular_bytes':total,'entries':len(records)+1,
        'private_marker_scan_passed':True,'file_sha256':hashes,'capture':manifest,'installable':False}
  (work/'scan.json').write_bytes(encoded(scan))
  toolchain=checkpoint/'toolchain.json';require(toolchain.is_file() and not toolchain.is_symlink(),'reviewed toolchain record required');shutil.copyfile(toolchain,work/'toolchain.json')
  inherited=(checkpoint/'approval.json').read_bytes();require(len(inherited)<1024*1024,'inherited approval bound')
  record={'schema':1,'purpose':'failed-make-public-kernel-checkpoint','source_revision':SOURCE,'run_id':run_id,'attempt':attempt,
   'make_exit_code':exit_code,'archive_sha256':sha(work/'tree.tar'),'scan_sha256':sha(work/'scan.json'),'resolved_config_sha256':sha(root/'.config'),
   'actual_source_rpm_sha256':sha(srpm),'toolchain_versions':[subprocess.check_output([n,'--version']).decode().splitlines()[0] for n in ['gcc','ld','make']],
   'toolchain_evidence_sha256':sha(work/'toolchain.json'),'parent_approval_sha256':hashlib.sha256(inherited).hexdigest(),'parent_approval':json.loads(inherited),
   'capture_helper_sha256':sha(__file__),'installable':False,'build_success':False,'resume_admitted':False,'fresh_key_required_in_resume_run':True}
  (work/'capture-receipt.json').write_bytes(encoded({'schema':1,'success':True,'capture':record}))
  names=['tree.tar','scan.json','toolchain.json','capture-receipt.json']
  (work/'SHA256SUMS').write_text(''.join(sha(work/n)+'  '+n+'\n' for n in names))
  for name in names+['SHA256SUMS']:(work/name).rename(output/name)
  (output/'ready').write_text(SOURCE+':'+run_id+':'+attempt+'\n')
 return record

def run_make(command,capture_callback):
 child=subprocess.Popen(command,start_new_session=True)
 try:result=child.wait()
 finally:
  try:os.killpg(child.pid,signal.SIGKILL)
  except ProcessLookupError:pass
  child.wait()
 status=result if result>=0 else 128-result
 if status:
  try:capture_callback(status)
  except Exception as error:print('public failure checkpoint unavailable: '+type(error).__name__+': '+str(error),file=sys.stderr)
 return status


def derive(recipe):
 text=recipe.decode();marker='    make -j"$JOBS" all; \\\n';require(text.count(marker)==1,'unique resumed make seam required')
 start=text.rfind('RUN --mount=type=bind,from=kernel-checkpoint',0,text.index(marker));require(start>=0,'reviewed recovery context required')
 text=text[:start]+'ARG KERNEL_CHECKPOINT_RUN_ID\nARG KERNEL_CHECKPOINT_ATTEMPT\n'+text[start:]
 text=text.replace('RUN --mount=type=bind,from=kernel-checkpoint,target=/checkpoint,ro ',
  'RUN --mount=type=cache,id=tilo-kernel-failure-${KERNEL_CHECKPOINT_RUN_ID}-${KERNEL_CHECKPOINT_ATTEMPT},target=/failure-cache,sharing=locked \\\n    --mount=type=bind,from=kernel-checkpoint,target=/checkpoint,ro ',1)
 text=text.replace(marker,'    python3 /checkpoint/kernel_failure_checkpoint.py --root "$KSRC" --output /failure-cache --checkpoint /checkpoint --run-id "$KERNEL_CHECKPOINT_RUN_ID" --attempt "$KERNEL_CHECKPOINT_ATTEMPT" -- make -j"$JOBS" all; \\\n',1)
 text+='''
# Independent public cache export; never depends on the failed compile stage.
FROM ${BASE_REF} AS failure-export
ARG KERNEL_CHECKPOINT_RUN_ID
ARG KERNEL_CHECKPOINT_ATTEMPT
RUN --mount=type=cache,id=tilo-kernel-failure-${KERNEL_CHECKPOINT_RUN_ID}-${KERNEL_CHECKPOINT_ATTEMPT},target=/failure-cache,sharing=locked,ro \\
    test "$(cat /failure-cache/ready)" = "'''+SOURCE+''':${KERNEL_CHECKPOINT_RUN_ID}:${KERNEL_CHECKPOINT_ATTEMPT}" && \\
    cd /failure-cache && sha256sum -c SHA256SUMS && mkdir /public && \\
    cp tree.tar scan.json toolchain.json capture-receipt.json SHA256SUMS ready /public/
FROM scratch AS failed-kernel-checkpoint
LABEL org.tilo.artifact.purpose="failed-kernel-checkpoint" org.tilo.artifact.installable="false" org.tilo.artifact.resume-admitted="false"
COPY --from=failure-export /public/ /
'''
 return text.encode()

if __name__=='__main__':
 parser=argparse.ArgumentParser(description=__doc__)
 for field in ['root','output','checkpoint','run-id','attempt']:parser.add_argument('--'+field,required=True)
 parser.add_argument('command',nargs=argparse.REMAINDER);args=parser.parse_args();command=args.command[1:] if args.command[:1]==['--'] else args.command
 require(command[:1]==['make'] and command[-1:] == ['all'],'only kernel make admitted')
 sys.exit(run_make(command,lambda status:capture(args.root,args.output,args.run_id,args.attempt,status,args.checkpoint)))
