#!/usr/bin/env python3
"""External operator launcher; Kubernetes credentials never enter the guest.

Requires an already provisioned, sleeping, dedicated Kata pod. Inspection is
performed against its live host processes immediately before authenticated exec.
No pod creation, credential provisioning, node mutation or disk publication.
"""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import time
import zipfile


class ControllerTransportError(ValueError):
    """A reconnectable Kubernetes control-plane request did not complete."""


# Runs in the host namespace through the existing kata-deploy agent. Never reads
# process environments or registry configuration. OCI process/env is omitted.
HOST_INSPECT = r'''
import hashlib,json,os,pathlib,stat,subprocess,sys
p=json.load(sys.stdin);P=pathlib.Path
def ctr(identity):
 d=json.loads(subprocess.check_output(['/usr/bin/ctr','-n','k8s.io','containers','info',identity]));s=d['Spec']
 return {k:d[k] for k in ('ID','Labels','Runtime')}|{'spec':{k:s.get(k) for k in ('mounts','linux','annotations')}}
c=ctr(p['container']);sandbox=c['spec']['annotations']['io.kubernetes.cri.sandbox-id'];s=ctr(sandbox)
processes=[]
for d in P('/proc').glob('[0-9]*'):
 try:a=(d/'cmdline').read_bytes().rstrip(b'\0').split(b'\0');a=[x.decode() for x in a]
 except (OSError,UnicodeError):continue
 if a and a[0]=='/opt/kata/bin/qemu-system-aarch64' and 'sandbox-'+sandbox in a:processes.append((d,a))
assert len(processes)==1,'expected one bound QEMU process'
d,argv=processes[0];fds=[]
for f in (d/'fd').iterdir():
 try:target=os.readlink(f);kind=stat.S_IFMT(f.stat().st_mode)
 except FileNotFoundError:continue
 fds.append({'path':target,'kind':kind})
shims=[]
for x in P('/proc').glob('[0-9]*'):
 try:a=(x/'cmdline').read_bytes().rstrip(b'\0').split(b'\0')
 except OSError:continue
 if a and a[0]==b'/opt/kata/runtime-rs/bin/containerd-shim-kata-v2' and sandbox.encode() in a:shims.append(x)
assert len(shims)==1
shared='/run/kata-containers/shared/sandboxes/'+sandbox+'/ro'
sock='/run/kata/'+sandbox+'/root/virtiofsd.sock'
export=(shims[0]/'root'/shared.lstrip('/')).stat()
socket_path=shims[0]/'root'/sock.lstrip('/')
assert stat.S_ISSOCK(socket_path.stat().st_mode)
owner=int(P(str(socket_path)+'.pid').read_text().strip())
servers=[]
for x in P('/proc').glob('[0-9]*'):
 try:a=(x/'cmdline').read_bytes().rstrip(b'\0').split(b'\0');a=[i.decode() for i in a]
 except (OSError,UnicodeError):continue
 if not a or a[0]!='/opt/kata/libexec/virtiofsd' or sock not in a:continue
 root=(x/'root').stat()
 if (root.st_dev,root.st_ino)!=(export.st_dev,export.st_ino):continue
 links=[]
 for fd in (x/'fd').iterdir():
  try:links.append(os.readlink(fd))
  except FileNotFoundError:continue
 parent=int((x/'stat').read_text().split()[3])
 servers.append({'pid':int(x.name),'argv':a,'root_identity':[root.st_dev,root.st_ino],'export_identity':[export.st_dev,export.st_ino],'socket_bound':(int(x.name)==owner or parent==owner) and sock+'.pid' in links,'socket_owner_pid':owner,'mountinfo':(x/'mountinfo').read_text()})
assert servers and all(x['socket_bound'] for x in servers),'expected chrooted virtiofs server bound to shared root'
assert all(x['mountinfo']==servers[0]['mountinfo'] for x in servers),'virtiofs parent/worker mount namespaces differ'
# LocalStorage in pinned runtime-rs creates directories inside the exported
# sandbox rootfs. They are not separate host mounts. Observe their real inode
# identities through all three independently bound views, never infer by name.
local_volumes=[]
uid=c['Labels']['io.kubernetes.pod.uid']
prefix='/var/lib/kubelet/pods/'+uid+'/volumes/kubernetes.io~empty-dir/'
for m in c['spec']['mounts']:
 if m.get('type')!='bind' or not m.get('source','').startswith(prefix):continue
 name=m['source'][len(prefix):]
 if not name or '/' in name or name in ('.','..'):continue
 roots=[P('/proc')/str(servers[0]['pid'])/'root'/'passthrough'/sandbox/'rootfs',
        shims[0]/'root'/shared.lstrip('/')/'passthrough'/sandbox/'rootfs',
        shims[0]/'root'/'run/containerd/io.containerd.runtime.v2.task/k8s.io'/sandbox/'rootfs']
 chains=[]
 for rootfs in roots:
  chain=[]
  for path in (rootfs,rootfs/'local',rootfs/'local'/name):
   try:
    info=path.lstat();chain.append({'kind':stat.S_IFMT(info.st_mode),'device':info.st_dev,'inode':info.st_ino})
   except FileNotFoundError:chain.append(None)
  chains.append(chain)
 if any(chain[-1] is not None for chain in chains):
  local_volumes.append({'source':m['source'],'destination':m['destination'],'name':name,'chains':chains})

def digest(path):
 h=hashlib.sha256()
 with open(path,'rb') as f:
  for chunk in iter(lambda:f.read(1024*1024),b''):h.update(chunk)
 return h.hexdigest()
config=P('/opt/kata/share/defaults/kata-containers/runtime-rs/runtimes/qemu-runtime-rs/configuration-qemu-runtime-rs.toml')
paths=[config,*sorted((config.parent/'config.d').glob('*.toml')),P('/opt/kata/containerd/config.d/kata-deploy.toml'),P('/etc/containerd/config.toml'),P('/opt/kata/bin/qemu-system-aarch64'),P('/opt/kata/libexec/virtiofsd'),P('/opt/kata/runtime-rs/bin/containerd-shim-kata-v2')]
hashes={str(x):digest(x) for x in paths}
for flag in ('-kernel','-bios'):
 assert argv.count(flag)==1;path=argv[argv.index(flag)+1];hashes[path]=digest(path)
image='/opt/kata/share/kata-containers/kata-ubuntu-noble.image';hashes[image]=digest(image)
print(json.dumps({'container':c,'sandbox':s,'pid':int(d.name),'start':(d/'stat').read_text().split()[21],'argv':argv,'fds':fds,'mountinfo':(shims[0]/'mountinfo').read_text(),'virtiofs':servers[0],'local_volumes':local_volumes,'hashes':hashes}))
'''

GUEST_JOB = r'''
import base64,contextlib,hashlib,importlib.util,io,json,os,pathlib,stat,sys,tempfile,types,zipfile
job=pathlib.Path(sys.argv[1]); auth=None if sys.argv[2]=='-' else pathlib.Path(sys.argv[2]); payload_path=job/'request.json'; status_path=job/'status.json'; log_path=job/'log.txt'
MAX_LOG=256*1024
class BoundedLog:
 def __init__(self,path):
  fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_APPEND|os.O_NOFOLLOW,0o600); i=os.fstat(fd)
  if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>MAX_LOG:os.close(fd);raise ValueError('creator log is invalid')
  self.f=os.fdopen(fd,'wb',buffering=0);self.count=i.st_size
 def write(self,value):
  raw=value.encode() if isinstance(value,str) else value; take=max(0,MAX_LOG-self.count)
  if take:self.f.write(raw[:take]);self.count+=min(len(raw),take)
  return len(raw)
 def flush(self):self.f.flush()
 def close(self):self.f.close()
def private(path,limit):
 i=path.lstat()
 if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>limit:raise ValueError('private creator job file is invalid')
 fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  o=os.fstat(fd)
  if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):raise ValueError('private creator job file changed')
  value=os.read(fd,limit+1)
  if len(value)!=i.st_size:raise ValueError('private creator job file changed')
  return value
 finally:os.close(fd)
def atom(path,value):
 encoded=(json.dumps(value,sort_keys=True,separators=(',',':'))+'\n').encode(); temporary=path.with_name('.'+path.name+'.tmp')
 fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 try:
  with os.fdopen(fd,'wb') as f:f.write(encoded);f.flush();os.fsync(f.fileno())
  os.replace(temporary,path)
 finally:
  try:temporary.unlink()
  except FileNotFoundError:pass
def process_identity(request):
 start=pathlib.Path('/proc/self/stat').read_text().split()[21]
 argv=pathlib.Path('/proc/self/cmdline').read_bytes(); cgroup=pathlib.Path('/proc/self/cgroup').read_bytes()
 return {'pid':os.getpid(),'start':start,'argv_sha256':hashlib.sha256(argv).hexdigest(),'cgroup_sha256':hashlib.sha256(cgroup).hexdigest(),'request_sha256':hashlib.sha256(request).hexdigest()}
def state(binding,name,process,**extra):atom(status_path,{'schema':1,'purpose':'tilo-supervised-creator-job','state':name,'binding':binding,'process':process,**extra})
payload=None; process=None; log=BoundedLog(log_path)
try:
 request=private(payload_path,8*1024*1024); payload=json.loads(request); binding=payload['binding']; process=process_identity(request)
 state(binding,'running',process)
 if auth is not None: private(auth,1024*1024)
 with tempfile.TemporaryDirectory(prefix='tilo-supervised-helper-',dir=job) as d:
  d=pathlib.Path(d)
  with contextlib.redirect_stdout(log),contextlib.redirect_stderr(log):
   z=zipfile.ZipFile(io.BytesIO(base64.b64decode(payload['helpers'],validate=True)))
   for name in z.namelist():
    assert pathlib.PurePosixPath(name).name==name and name.endswith('.py')
    pathlib.Path(d,name).write_bytes(z.read(name))
   sys.path.insert(0,str(d))
   spec=importlib.util.spec_from_file_location('disk_creator',d/'create-development-disk.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
   args=payload['args']
   for name in ('public_bundle','output','installer_bootc_directory'):args[name]=pathlib.Path(args[name])
   if auth is not None: os.environ['REGISTRY_AUTH_FILE']=str(auth)
   receipt=m.create_from_supervised_controller(types.SimpleNamespace(**args),payload['observation'])
   receipt['controller_helper']={'revision':payload['helper_revision'],'bundle_sha256':payload['helper_bundle_sha256']}
   receipt['controller_job']={'binding':binding}
   encoded=(json.dumps(receipt,sort_keys=True)+'\n').encode(); destination=args['output'].with_name('creation.json'); temporary=destination.with_name('.creation.json.tmp')
   fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
   try:
    with os.fdopen(fd,'wb') as stream:stream.write(encoded);stream.flush();os.fsync(stream.fileno())
    os.replace(temporary,destination)
   finally:
    try:temporary.unlink()
    except FileNotFoundError:pass
 if auth is not None:
  auth.unlink();auth.parent.rmdir();auth=None
 state(binding,'succeeded',process,creation_sha256=hashlib.sha256(encoded).hexdigest(),log_bytes=log.count,log_truncated=log.count>=MAX_LOG)
except BaseException:
 try:
  if payload is not None: state(payload['binding'],'failed',process,log_bytes=log.count,log_truncated=log.count>=MAX_LOG)
 except BaseException:pass
finally:
 if auth is not None:
  try:auth.unlink()
  except FileNotFoundError:pass
  try:auth.parent.rmdir()
  except OSError:pass
 log.close()
'''


GUEST_LAUNCH = r'''
import base64,hashlib,json,os,pathlib,stat,subprocess,sys,time
payload=json.load(sys.stdin); job_source=base64.b64decode(sys.argv[1],validate=True)
work=pathlib.Path('/work'); output=work/'disk-output'; job=output/'.creator-job'
def atom(path,value):
 encoded=(json.dumps(value,sort_keys=True,separators=(',',':'))+'\n').encode(); temporary=path.with_name('.'+path.name+'.tmp')
 fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 try:
  with os.fdopen(fd,'wb') as f:f.write(encoded);f.flush();os.fsync(f.fileno())
  os.replace(temporary,path)
 finally:
  try:temporary.unlink()
  except FileNotFoundError:pass
def regular(path,limit):
 i=path.lstat()
 if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>limit:raise ValueError('creator job state is invalid')
 return json.loads(path.read_bytes())
binding=payload.get('binding'); expected=payload.get('binding_sha256')
if (not isinstance(binding,dict) or not isinstance(expected,str) or hashlib.sha256(json.dumps(binding,sort_keys=True,separators=(',',':')).encode()).hexdigest()!=expected):raise ValueError('creator job binding is invalid')
if (not work.is_dir() or work.is_symlink() or work.stat().st_uid or work.stat().st_gid
    or work.stat().st_dev==pathlib.Path('/').stat().st_dev):raise ValueError('creator work directory is invalid')
os.chmod(work,0o700)
if stat.S_IMODE(work.stat().st_mode)!=0o700:raise ValueError('creator work directory permissions are invalid')
mounts=[line.split() for line in pathlib.Path('/proc/self/mountinfo').read_text().splitlines()]
if not any(len(row)>8 and row[4]=='/tmp' and '-' in row and row[row.index('-')+1]=='tmpfs' for row in mounts):raise ValueError('creator credential directory is not memory-backed')
if output.exists() or output.is_symlink():
 if output.is_symlink() or not output.is_dir() or output.stat().st_uid or output.stat().st_gid or stat.S_IMODE(output.stat().st_mode)!=0o700:raise ValueError('creator output directory is invalid')
else:output.mkdir(mode=0o700)
if output.is_symlink() or not output.is_dir() or output.stat().st_uid or output.stat().st_gid or stat.S_IMODE(output.stat().st_mode)!=0o700:raise ValueError('creator output directory is invalid')
if job.exists() or job.is_symlink():
 if job.is_symlink() or not job.is_dir() or job.stat().st_uid or job.stat().st_gid or stat.S_IMODE(job.stat().st_mode)!=0o700:raise ValueError('creator job directory is invalid')
 status=regular(job/'status.json',65536)
 if status.get('binding')!=binding or status.get('purpose')!='tilo-supervised-creator-job' or status.get('schema')!=1:raise ValueError('creator job binding differs')
 print(json.dumps(status,sort_keys=True));sys.exit(0)
if any(output.iterdir()):raise ValueError('creator output is not empty')
credential=None; started=False
try:
 raw_auth=base64.b64decode(payload.pop('registry_auth'),validate=True)
 auth_argument='-'
 if raw_auth:
  if len(raw_auth)>1024*1024:raise ValueError('creator registry auth is invalid')
  credential=pathlib.Path('/tmp')/('.tilo-creator-auth-'+expected)
  credential.mkdir(mode=0o700); auth=credential/'registry-auth.json'; fd=os.open(auth,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
  with os.fdopen(fd,'wb') as f:f.write(raw_auth);f.flush();os.fsync(f.fileno())
  auth_argument=str(auth)
 job.mkdir(mode=0o700)
 atom(job/'request.json',payload); source=job/'job.py'; fd=os.open(source,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o700)
 with os.fdopen(fd,'wb') as f:f.write(job_source);f.flush();os.fsync(f.fileno())
 atom(job/'status.json',{'schema':1,'purpose':'tilo-supervised-creator-job','state':'launched','binding':binding,'launched_at':int(time.time())})
 with open(os.devnull,'wb') as null:
  if auth_argument=='-': command=['/usr/bin/python3',str(source),str(job),auth_argument]
  else: command=['/bin/sh','-ceu','trap "rm -f -- \"$1\"; rmdir -- \"${1%/*}\" || :" EXIT HUP INT TERM; /usr/bin/python3 "$2" "$3" "$1"','sh',auth_argument,str(source),str(job)]
  subprocess.Popen(command,stdin=null,stdout=null,stderr=null,start_new_session=True,close_fds=True)
 started=True; print(json.dumps(regular(job/'status.json',65536),sort_keys=True))
finally:
 if credential is not None and not started:
  try:(credential/'registry-auth.json').unlink()
  except FileNotFoundError:pass
  try:credential.rmdir()
  except OSError:pass
'''


def checked(argv, payload=None):
    try:
        result = subprocess.run(argv, input=payload, text=True, capture_output=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ControllerTransportError(f'{Path(argv[0]).name} transport failed') from error
    if result.returncode:
        # Kubernetes and registry errors can contain sensitive configuration.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout


def private_input(path, *, limit):
    """Read one bounded, owned regular file without following a replacement."""
    path = Path(path)
    before = path.lstat()
    if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
            or before.st_uid != os.geteuid() or before.st_mode & 0o077
            or before.st_size < 2 or before.st_size > limit):
        raise ValueError('private input must be a populated bounded regular file')
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
    fd = os.open(path, flags)
    try:
        current = os.fstat(fd)
        if (current.st_dev, current.st_ino, current.st_size) != (before.st_dev, before.st_ino, before.st_size):
            raise ValueError('private input changed while opening')
        chunks = []
        remaining = limit + 1
        while remaining:
            chunk = os.read(fd, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b''.join(chunks)
        if len(data) != current.st_size:
            raise ValueError('private input changed while reading')
        return data
    finally:
        os.close(fd)


def helper_bundle(revision):
    """Archive exactly the tracked Python helpers at the requested checkout."""
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('helper revision must be a full Git commit')
    root = Path(__file__).resolve().parents[2]
    actual = checked(['git', '-C', str(root), 'rev-parse', 'HEAD']).strip()
    if actual != revision:
        raise ValueError('helper checkout differs from requested revision')
    if checked(['git', '-C', str(root), 'status', '--porcelain', '--untracked-files=no', '--', 'os/dev']):
        raise ValueError('tracked disk helpers differ from the helper revision')
    names = checked(['git', '-C', str(root), 'ls-tree', '-r', '--name-only', revision, '--', 'os/dev']).splitlines()
    names = [name for name in names if name.endswith('.py') and Path(name).parent.as_posix() == 'os/dev']
    bundle = io.BytesIO()
    with zipfile.ZipFile(bundle, 'w', zipfile.ZIP_DEFLATED) as archive:
        for name in names:
            data = subprocess.run(['git', '-C', str(root), 'show', revision + ':' + name],
                                  check=True, capture_output=True).stdout
            archive.writestr(Path(name).name, data)
    value = bundle.getvalue()
    return value, hashlib.sha256(value).hexdigest()


def persist_receipt(path, output):
    """Validate and atomically persist the one guest receipt."""
    try:
        receipt = json.loads(output)
    except json.JSONDecodeError as error:
        raise ValueError('guest did not return one creation receipt') from error
    if receipt.get('disk_created') is not True or receipt.get('purpose') != 'tilo-development-unaccepted-test-disk':
        raise ValueError('guest did not return a completed creation receipt')
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    encoded = (json.dumps(receipt, indent=2, sort_keys=True) + '\n').encode()
    fd, temporary = tempfile.mkstemp(prefix='.creation-', suffix='.json', dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(encoded); stream.flush(); os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try: os.unlink(temporary)
        except FileNotFoundError: pass
    return receipt


def canonical_sha256(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def creator_job_binding(operation, pod_uid, node_uid, container_id, image, disk_args,
                        observation, helper_revision, helper_bundle_sha256):
    """Bind a reconnectable remote job to exactly one admitted creator."""
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}', operation):
        raise ValueError('creator operation is invalid')
    if not re.fullmatch(r'[0-9a-f]{64}', helper_bundle_sha256):
        raise ValueError('creator helper bundle digest is invalid')
    binding = {'schema': 1, 'purpose': 'tilo-supervised-creator-job-binding',
               'operation': operation, 'pod_uid': pod_uid, 'node_uid': node_uid,
               'container_id': container_id, 'image': image,
               'output': disk_args.get('output'), 'helper_revision': helper_revision,
               'helper_bundle_sha256': helper_bundle_sha256,
               'job_sha256': hashlib.sha256(GUEST_JOB.encode()).hexdigest(),
               'disk_args_sha256': canonical_sha256(disk_args),
               'observation_sha256': canonical_sha256(observation)}
    return binding, canonical_sha256(binding)


def validate_creator_job_status(value, binding):
    base = {'schema', 'purpose', 'state', 'binding'}
    if (not isinstance(value, dict) or value.get('schema') != 1
            or value.get('purpose') != 'tilo-supervised-creator-job'
            or value.get('binding') != binding or value.get('state') not in
            {'launched', 'running', 'succeeded', 'failed'}):
        raise ValueError('creator job status differs from the admitted request')
    if value['state'] == 'launched':
        if (set(value) != base | {'launched_at'}
                or type(value['launched_at']) is not int or value['launched_at'] <= 0):
            raise ValueError('creator launch status has unexpected fields')
        return
    expected = base | {'process', 'log_bytes', 'log_truncated'}
    if value['state'] == 'succeeded':
        expected |= {'creation_sha256'}
    if set(value) != expected:
        raise ValueError('creator job status has unexpected fields')
    process = value['process']
    if (not isinstance(process, dict) or set(process) !=
            {'pid', 'start', 'argv_sha256', 'cgroup_sha256', 'request_sha256'}
            or type(process['pid']) is not int or process['pid'] <= 1
            or not isinstance(process['start'], str) or not process['start'].isdigit()
            or any(not isinstance(process[key], str) or not re.fullmatch(r'[0-9a-f]{64}', process[key])
                   for key in ('argv_sha256', 'cgroup_sha256', 'request_sha256'))
            or type(value['log_bytes']) is not int or not 0 <= value['log_bytes'] <= 256 * 1024
            or type(value['log_truncated']) is not bool):
        raise ValueError('creator job process identity is invalid')
    if value['state'] == 'succeeded' and not re.fullmatch(r'[0-9a-f]{64}', value['creation_sha256']):
        raise ValueError('creator job success receipt is invalid')


REMOTE_CREATOR_STATUS = r'''import hashlib,json,os,pathlib,stat,sys
job=pathlib.Path('/work/disk-output/.creator-job'); path=job/'status.json'; request=job/'request.json'
def invalid():print('TILO_CREATOR_STATUS_INVALID');raise SystemExit(2)
def read(path,limit):
 try:i=path.lstat()
 except OSError:invalid()
 if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size<2 or i.st_size>limit:invalid()
 fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  o=os.fstat(fd)
  if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):invalid()
  data=os.read(fd,limit+1)
  if len(data)!=i.st_size:invalid()
  return data
 finally:os.close(fd)
try:value=json.loads(read(path,65536))
except BaseException:invalid()
process=value.get('process')
source=job/'job.py'
try:source_sha=hashlib.sha256(read(source,1024*1024)).hexdigest()
except BaseException:invalid()
if source_sha!=value.get('binding',{}).get('job_sha256'):invalid()
if process:
 request_bytes=read(request,8*1024*1024)
 if hashlib.sha256(request_bytes).hexdigest()!=process.get('request_sha256'):invalid()
 if value.get('state')=='running':
  proc=pathlib.Path('/proc')/str(process['pid'])
  try:start=(proc/'stat').read_text().split()[21]; argv=(proc/'cmdline').read_bytes(); cgroup=(proc/'cgroup').read_bytes()
  except (FileNotFoundError,IndexError):print('TILO_CREATOR_PROCESS_GONE');raise SystemExit(3)
  if (start!=process['start'] or hashlib.sha256(argv).hexdigest()!=process['argv_sha256']
      or hashlib.sha256(cgroup).hexdigest()!=process['cgroup_sha256']):invalid()
print(json.dumps(value,sort_keys=True))'''


REMOTE_CREATOR_RECEIPT = r'''import base64,os,pathlib,stat,sys
p=pathlib.Path('/work/disk-output/creation.json');i=p.lstat()
if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or not 2<=i.st_size<=1024*1024:raise SystemExit(2)
fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
try:
 o=os.fstat(fd)
 if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):raise SystemExit(2)
 value=os.read(fd,1024*1024+1)
 if len(value)!=i.st_size:raise SystemExit(2)
 print(base64.b64encode(value).decode())
finally:os.close(fd)'''


def remote_creator_status(kubectl_base, namespace, pod, container, binding):
    try:
        result = subprocess.run(kubectl_base + ['-n', namespace, 'exec', pod, '-c', container,
                                                '--', 'python3', '-c', REMOTE_CREATOR_STATUS],
                                text=True, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ControllerTransportError('creator status poll lost transport') from error
    if result.returncode:
        # These constant stdout markers come from the authenticated remote
        # reader.  All other diagnostics stay local and are transport errors.
        if result.stdout.strip() == 'TILO_CREATOR_PROCESS_GONE':
            raise ValueError('detached creator process vanished')
        if result.stdout.strip() == 'TILO_CREATOR_STATUS_INVALID':
            raise ValueError('detached creator process identity changed')
        raise ControllerTransportError('creator status poll lost transport')
    output = result.stdout
    try:
        value = json.loads(output)
    except json.JSONDecodeError as error:
        raise ValueError('creator status is not JSON') from error
    validate_creator_job_status(value, binding)
    return value


def poll_creator_job(kubectl_base, namespace, pod, container, binding, *,
                     timeout=7200, poll_interval=10, retry_interval=2,
                     clock=time.monotonic, wall_clock=time.time, sleep=time.sleep):
    """Reconnect after each short exec; the remote process owns the long work."""
    deadline = clock() + timeout
    last_transport = None
    while clock() < deadline:
        try:
            status = remote_creator_status(kubectl_base, namespace, pod, container, binding)
        except ControllerTransportError as error:
            last_transport = error; sleep(retry_interval); continue
        if status['state'] == 'succeeded':
            return status
        if status['state'] == 'failed':
            raise ValueError('detached creator job failed')
        if status['state'] == 'launched' and wall_clock() - status['launched_at'] > 60:
            raise ValueError('detached creator launch did not establish a bound process')
        sleep(poll_interval)
    if last_transport is not None:
        raise ControllerTransportError('creator status polling timed out after transport loss') from last_transport
    raise ValueError('detached creator job timed out')


def remote_creator_receipt(kubectl_base, namespace, pod, container, status, binding):
    output = checked(kubectl_base + ['-n', namespace, 'exec', pod, '-c', container,
                                     '--', 'python3', '-c', REMOTE_CREATOR_RECEIPT])
    try:
        receipt = base64.b64decode(output.strip(), validate=True)
    except ValueError as error:
        raise ValueError('creator receipt is not bounded base64') from error
    if hashlib.sha256(receipt).hexdigest() != status['creation_sha256']:
        raise ValueError('creator receipt differs from detached success status')
    try:
        document = json.loads(receipt)
    except json.JSONDecodeError as error:
        raise ValueError('creator receipt is not JSON') from error
    if (document.get('controller_job') != {'binding': binding}
            or document.get('path') != binding.get('output')
            or document.get('size_bytes') != 64 * 1024**3
            or document.get('disk_created') is not True
            or document.get('controller_helper', {}).get('revision') != binding.get('helper_revision')
            or document.get('controller_helper', {}).get('bundle_sha256')
               != binding.get('helper_bundle_sha256')):
        raise ValueError('creator receipt does not bind the admitted detached job')
    return receipt


def persist_creator_job(path, binding, status):
    validate_creator_job_status(status, binding)
    document = {'schema': 1, 'purpose': 'tilo-supervised-creator-job-launch',
                'binding': binding, 'status': status}
    path = Path(path); path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    encoded = (json.dumps(document, indent=2, sort_keys=True) + '\n').encode()
    fd, temporary = tempfile.mkstemp(prefix='.creator-job-', suffix='.json', dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(encoded); stream.flush(); os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try: os.unlink(temporary)
        except FileNotFoundError: pass
    return document


def inspect_pod(pod, node, expected_uid, guest_image):
    if not re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', guest_image):
        raise ValueError('guest image must be independently pinned by digest')
    if pod['metadata']['uid'] != expected_uid or pod['metadata'].get('deletionTimestamp'):
        raise ValueError('pod identity changed or is terminating')
    s = pod['spec']
    if s['nodeName'] != node or s['runtimeClassName'] != 'kata-qemu-runtime-rs':
        raise ValueError('unexpected node or runtime')
    if any(s.get(x) for x in ('hostPID', 'hostIPC', 'hostNetwork')) or s.get('automountServiceAccountToken') is not False:
        raise ValueError('host namespace or service account exposure')
    if pod['metadata'].get('annotations') or s.get('initContainers') or s.get('ephemeralContainers'):
        raise ValueError('override annotations or extra containers are unsupported')
    if len(s['containers']) != 1:
        raise ValueError('dedicated single-container pod required')
    c = s['containers'][0]
    if c['image'] != guest_image:
        raise ValueError('pod image differs from pinned guest tools image')
    if c.get('volumeDevices') or c.get('envFrom') or any('valueFrom' in x for x in c.get('env', [])):
        raise ValueError('device or secret injection unsupported')
    if any('/' in key for kind in ('requests', 'limits') for key in c.get('resources', {}).get(kind, {})):
        raise ValueError('extended device resource unsupported')
    if s.get('resourceClaims'):
        raise ValueError('dynamic resource claims unsupported')
    volumes = s.get('volumes', [])
    if not volumes or any(set(x) not in ({'name', 'emptyDir'}, {'name', 'configMap'}) for x in volumes):
        raise ValueError('only dedicated emptyDir and ConfigMap volumes supported')
    status = pod['status'].get('containerStatuses', [])
    if pod['status']['phase'] != 'Running' or len(status) != 1 or not status[0].get('ready') or status[0].get('restartCount'):
        raise ValueError('guest is not continuously ready')
    if status[0].get('imageID') != guest_image:
        raise ValueError('running image differs from pinned guest tools image')
    identity = status[0]['containerID'].removeprefix('containerd://')
    if not re.fullmatch('[0-9a-f]{64}', identity):
        raise ValueError('invalid container identity')
    return identity


def inspect_local_volumes(host, pod, sid, missing, mount_rows, server_rows):
    """Accept runtime-local emptyDirs only with fresh independent host identities."""
    prefix = '/var/lib/kubelet/pods/'+pod['metadata']['uid']+'/volumes/kubernetes.io~empty-dir/'
    volumes = {v['name']: v for v in pod['spec']['volumes']}
    mounts = pod['spec']['containers'][0].get('volumeMounts', [])
    evidence = host.get('local_volumes', [])
    if not isinstance(evidence, list):
        raise ValueError('invalid local volume observation')
    by_source = {}
    for row in evidence:
        if not isinstance(row, dict) or set(row) != {'source','destination','name','chains'} or row['source'] in by_source:
            raise ValueError('ambiguous local volume observation')
        name = row['name']
        volume = volumes.get(name, {})
        targets = [m for m in mounts if m['name']==name]
        if (row['source'] != prefix+name or 'emptyDir' not in volume
                or volume['emptyDir'].get('medium', '') != '' or len(targets)!=1
                or row['destination'] != targets[0]['mountPath']
                or any(k in targets[0] for k in ('subPath','subPathExpr'))):
            raise ValueError('local observation differs from complete Pod emptyDir')
        by_source[row['source']] = row
    accepted = set()
    for source in missing:
        if not source.startswith(prefix):
            continue
        name = source.removeprefix(prefix)
        volume = volumes.get(name, {})
        candidates = [m for m in mounts if m['name']==name]
        if ('emptyDir' not in volume or volume['emptyDir'].get('medium', '') != ''
                or len(candidates)!=1 or any(k in candidates[0] for k in ('subPath','subPathExpr'))):
            raise ValueError('local volume must bind one complete disk emptyDir')
        row = by_source.get(source)
        if row is None or row['name'] != name or row['destination'] != candidates[0]['mountPath']:
            raise ValueError('missing exact local volume observation')
        chains = row['chains']
        if (not isinstance(chains,list) or len(chains)!=3
                or any(not isinstance(chain,list) or len(chain)!=3 for chain in chains)):
            raise ValueError('incomplete local volume identity chains')
        for chain in chains:
            for entry in chain:
                if (not isinstance(entry,dict) or set(entry)!={'kind','device','inode'}
                        or entry['kind']!=0o40000 or any(type(entry[k]) is not int or entry[k]<=0 for k in ('device','inode'))):
                    raise ValueError('local volume contains missing or redirected directories')
            if any(entry['device']!=chain[0]['device'] for entry in chain):
                raise ValueError('local volume crosses a filesystem boundary')
        if any(chain != chains[0] for chain in chains[1:]):
            raise ValueError('local volume views have different directory identities')
        paths = ['/passthrough/'+sid+'/rootfs/local',
                 '/run/kata-containers/shared/sandboxes/'+sid+'/ro/passthrough/'+sid+'/rootfs/local',
                 '/run/kata-containers/shared/sandboxes/'+sid+'/rw/passthrough/'+sid+'/rootfs/local',
                 '/run/containerd/io.containerd.runtime.v2.task/k8s.io/'+sid+'/rootfs/local']
        if any(any(fields[4]==path or fields[4].startswith(path+'/') for path in paths)
               for fields in mount_rows+server_rows):
            raise ValueError('local volume contains a nested mount')
        accepted.add(source)
    return accepted


def inspect_host(host, pod, pins):
    """Fail closed against observed devices, runtime and exact exported paths."""
    uid = pod['metadata']['uid']
    sid = host['sandbox']['ID']
    server = host['virtiofs']
    sock = '/run/kata/'+sid+'/root/virtiofsd.sock'
    shared = '/run/kata-containers/shared/sandboxes/'+sid+'/ro'
    if server['root_identity'] != server['export_identity'] or not server['socket_bound']:
        raise ValueError('virtiofs server is not bound to the inspected export')
    if server['argv'].count('--socket-path') != 1 or server['argv'][server['argv'].index('--socket-path')+1] != sock or server['argv'].count('--shared-dir') != 1 or server['argv'][server['argv'].index('--shared-dir')+1] != shared:
        raise ValueError('virtiofs backend path mismatch')
    chardevs = [host['argv'][i+1] for i,x in enumerate(host['argv']) if x == '-chardev']
    if chardevs != ['socket,id=virtiofsd-chardev,path='+sock, 'socket,id=charconsole0,server=on,wait=off,path=/run/kata/'+sid+'/root/console.sock']:
        raise ValueError('QEMU virtiofs backend mismatch')
    for obj in (host['sandbox'], host['container']):
        if obj['Labels'].get('io.kubernetes.pod.uid') != uid or obj['Runtime']['Name'] != 'io.containerd.kata-qemu-runtime-rs.v2':
            raise ValueError('runtime identity mismatch')
        if obj['spec']['linux'].get('devices') or any(k.startswith('io.katacontainers.') for k in obj['spec']['annotations']):
            raise ValueError('device passthrough or runtime override')
        options = base64.b64decode(obj['Runtime']['Options']['value'])
        expected = b'/opt/kata/share/defaults/kata-containers/runtime-rs/runtimes/qemu-runtime-rs/configuration-qemu-runtime-rs.toml'
        if options != bytes((18, len(expected)))+expected:
            raise ValueError('unexpected effective runtime ConfigPath')
    if host['hashes'] != pins:
        raise ValueError('runtime, kernel, firmware or guest image differs from reviewed pins')
    if not pins or any(not re.fullmatch('[0-9a-f]{64}', value) for value in pins.values()):
        raise ValueError('invalid expected host file digest')
    argv = host['argv']
    if any(x in ' '.join(argv).lower() for x in ('vfio', 'tpm', 'pflash', 'nvram')):
        raise ValueError('unexpected firmware or device backend')
    if argv.count('-blockdev') != 1 or '-drive' in argv:
        raise ValueError('unexpected block backend count')
    image = '/opt/kata/share/kata-containers/kata-ubuntu-noble.image'
    if 'filename='+image not in argv[argv.index('-blockdev')+1].split(','):
        raise ValueError('unexpected guest block image')
    allowed_devices = {'/dev/null', '/dev/urandom', '/dev/kvm', '/dev/vhost-vsock', '/dev/vhost-net', '/dev/net/tun'}
    for fd in host['fds']:
        path = fd['path']
        if fd['kind'] == 0o60000:
            raise ValueError('live QEMU has a host block device open')
        if fd['kind'] == 0o20000 and path not in allowed_devices:
            raise ValueError('unexpected host character device')
        if fd['kind'] == 0o100000 and path != image and not re.fullmatch(r'/dev/shm/qemu_back_mem\.entire-guest-memory-share\.[A-Za-z0-9]+ \(deleted\)', path):
            raise ValueError('unexpected host file exposed to QEMU')
    root = '/var/lib/kubelet/pods/'+uid
    sandbox = '/var/lib/containerd/io.containerd.grpc.v1.cri/sandboxes/'+sid
    allowed = {root+'/etc-hosts', sandbox+'/hostname', sandbox+'/resolv.conf', '/run/containerd/io.containerd.grpc.v1.cri/sandboxes/'+sid+'/shm'}
    for volume in pod['spec']['volumes']:
        kind = 'empty-dir' if 'emptyDir' in volume else 'configmap'
        allowed.add(root+'/volumes/kubernetes.io~'+kind+'/'+volume['name'])
    mounts = host['container']['spec']['mounts']
    for mount in mounts:
        if mount['type'] != 'bind':
            continue
        path = mount['source']
        termination = re.fullmatch(re.escape(root+'/containers/'+pod['spec']['containers'][0]['name']+'/')+r'[a-z0-9]+', path)
        if path not in allowed and not termination:
            raise ValueError('unexpected OCI bind mount')
        allowed.add(path)
    # Check actual virtiofs exports too, rather than relying on the OCI intent.
    exported = '/run/kata-containers/shared/sandboxes/'+sid+'/'
    mount_rows = [line.split() for line in host['mountinfo'].splitlines()]
    server_rows = [line.split() for line in server['mountinfo'].splitlines()]
    if not mount_rows or not server_rows or any(len(row)<10 or '-' not in row for row in mount_rows+server_rows):
        raise ValueError('missing or malformed mount inventory')
    if not any(row[4] == shared for row in mount_rows) or not any(row[4]=='/' and row[3].endswith('/'+sid+'/rw') for row in server_rows):
        raise ValueError('wrong shared mount namespace')
    required = {exported+'ro/passthrough/'+identity+'/rootfs' for identity in (sid,host['container']['ID'])}
    if not required.issubset({row[4] for row in mount_rows}):
        raise ValueError('missing live rootfs export')
    observed_sources = set()
    for fields in mount_rows:
        destination = fields[4]
        if not destination.startswith(exported):
            continue
        if destination in (exported+'ro', exported+'rw'):
            continue
        if '/passthrough/' not in destination:
            raise ValueError('unexpected actual virtiofs export')
        if destination.endswith('/rootfs') and destination.split('/')[-2] in (sid, host['container']['ID']):
            identity = destination.split('/')[-2]
            source = '/run/containerd/io.containerd.runtime.v2.task/k8s.io/'+identity+'/rootfs'
            if not any(row[4] == source and row[2:4] == fields[2:4] for row in mount_rows):
                raise ValueError('exported rootfs does not match container mount')
            continue
        if fields[3] not in allowed:
            raise ValueError('unexpected actual virtiofs export')
        observed_sources.add(fields[3])
    required_sources = {mount['source'] for mount in mounts if mount['type']=='bind'}
    # Kata materializes memory emptyDirs and /dev/shm within the guest; they
    # must not be confused with ordinary shared host-backed emptyDir exports.
    guest_local = {'/run/containerd/io.containerd.grpc.v1.cri/sandboxes/'+sid+'/shm'}
    guest_local.update(root+'/volumes/kubernetes.io~empty-dir/'+v['name'] for v in pod['spec']['volumes'] if v.get('emptyDir',{}).get('medium')=='Memory')
    missing = required_sources-guest_local-observed_sources
    observed_sources.update(inspect_local_volumes(host, pod, sid, missing, mount_rows, server_rows))
    if not (required_sources-guest_local).issubset(observed_sources):
        raise ValueError('missing expected live volume export')
    host_exports = {row[4].removeprefix(shared):row[2:4] for row in mount_rows if row[4].startswith(shared+'/passthrough/')}
    actual_exports = {row[4]:row[2:4] for row in server_rows if row[4].startswith('/passthrough/')}
    if host_exports != actual_exports:
        raise ValueError('virtiofs server export inventory differs from inspected namespace')
    return {'node_uid': '', 'pod_uid': uid, 'sandbox_id': sid,
            'container_id': host['container']['ID'], 'kernel_sha256': pins[argv[argv.index('-kernel')+1]]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('context', 'namespace', 'pod', 'pod-uid', 'node', 'node-uid', 'host-agent', 'guest-image'):
        p.add_argument('--'+name, required=True)
    p.add_argument('--pins', type=Path, required=True, help='operator-reviewed exact host file SHA256 mapping')
    p.add_argument('--disk-args', type=Path, required=True, help='guest create args JSON; paths are inside guest')
    p.add_argument('--evidence', type=Path, required=True)
    p.add_argument('--creation-receipt', type=Path,
                   help='required for creation: private local copy of the accepted receipt')
    auth = p.add_mutually_exclusive_group()
    auth.add_argument('--registry-auth', type=Path,
                      help='optional bounded registry auth file streamed only through authenticated exec')
    auth.add_argument('--registry-auth-stdin', action='store_true',
                      help='read bounded registry auth only from this controller stdin')
    p.add_argument('--creator-job-receipt', type=Path,
                   help='private durable non-secret detached creator launch state')
    p.add_argument('--helper-revision', required=True,
                   help='full commit whose tracked os/dev Python helpers are executed')
    p.add_argument('--operation',
                   help='fresh DNS-safe operation binding for a detached creator job')
    p.add_argument('--inspect-only', action='store_true')
    args = p.parse_args()
    k = ['kubectl', '--context', args.context]
    def get(kind, name, namespace=None):
        for attempt in range(3):
            try:
                return json.loads(checked(k+(['-n',namespace] if namespace else [])+['get',kind,name,'-o','json','--request-timeout=15s']))
            except ValueError:
                if attempt == 2:
                    raise
                time.sleep(1)
    pod = get('pod', args.pod, args.namespace)
    identity = inspect_pod(pod, args.node, args.pod_uid, args.guest_image)
    node = get('node', args.node)
    if node['metadata']['uid'] != args.node_uid:
        raise ValueError('node identity changed')
    agent = get('pod', args.host_agent, 'kata-system')
    if agent['spec']['nodeName'] != args.node or not agent['spec'].get('hostPID') or agent['metadata'].get('labels',{}).get('name') != 'kata-deploy':
        raise ValueError('host inspection agent mismatch')
    host = json.loads(checked(k+['-n','kata-system','exec','-i',args.host_agent,'-c','kube-kata','--','/usr/bin/nsenter','--mount=/proc/1/ns/mnt','--pid=/proc/1/ns/pid','--','python3','-c',HOST_INSPECT], json.dumps({'container':identity})))
    observation = inspect_host(host, pod, json.loads(args.pins.read_text()))
    observation['node_uid'] = args.node_uid
    fresh = get('pod', args.pod, args.namespace)
    if inspect_pod(fresh,args.node,args.pod_uid,args.guest_image) != identity or fresh['spec'] != pod['spec']:
        raise ValueError('pod changed during inspection')
    args.evidence.write_text(json.dumps({'observation':observation,'host':host},indent=2))
    if args.inspect_only:
        print(json.dumps({'inspected':True,'disk_created':False}));return
    if args.creation_receipt is None:
        raise ValueError('--creation-receipt is required for disk creation')
    if args.operation is None:
        raise ValueError('--operation is required for disk creation')
    disk_args = json.loads(args.disk_args.read_text())
    bundle, bundle_sha256 = helper_bundle(args.helper_revision)
    if args.registry_auth_stdin:
        registry_auth = sys.stdin.buffer.read(1024*1024 + 1)
        if not 2 <= len(registry_auth) <= 1024 * 1024:
            raise ValueError('stdin registry auth is not a bounded document')
        try:
            parsed_auth = json.loads(registry_auth)
        except json.JSONDecodeError as error:
            raise ValueError('stdin registry auth is not JSON') from error
        if not isinstance(parsed_auth.get('auths'), dict) or not parsed_auth['auths']:
            raise ValueError('stdin registry auth has no registry entries')
    else:
        registry_auth = private_input(args.registry_auth, limit=1024*1024) if args.registry_auth else None
    binding, binding_sha256 = creator_job_binding(
        args.operation, args.pod_uid, args.node_uid, identity, args.guest_image,
        disk_args, observation, args.helper_revision, bundle_sha256)
    payload = {'binding':binding,'binding_sha256':binding_sha256,
               'observation':observation,'args':disk_args,
               'helpers':base64.b64encode(bundle).decode(),
               'helper_revision':args.helper_revision,
               'helper_bundle_sha256':bundle_sha256,
               # This value is only on the launch stdin and the memory-backed
               # /tmp credential file.  GUEST_LAUNCH removes it before writing
               # the durable request in /work.
               'registry_auth':base64.b64encode(registry_auth).decode() if registry_auth else ''}
    launch = checked(k+['-n',args.namespace,'exec','-i',args.pod,'-c',pod['spec']['containers'][0]['name'],'--',
                        '/usr/bin/unshare','--mount','--propagation','private','--',
                        'python3','-c',GUEST_LAUNCH,base64.b64encode(GUEST_JOB.encode()).decode()],
                     json.dumps(payload))
    try:
        validate_creator_job_status(json.loads(launch), binding)
    except json.JSONDecodeError as error:
        raise ValueError('creator launch did not return bounded JSON') from error
    launched = json.loads(launch)
    if args.creator_job_receipt is not None:
        persist_creator_job(args.creator_job_receipt, binding, launched)
    try:
        status = poll_creator_job(k, args.namespace, args.pod,
                                  pod['spec']['containers'][0]['name'], binding)
        output = remote_creator_receipt(k, args.namespace, args.pod,
                                        pod['spec']['containers'][0]['name'], status, binding).decode()
    except ValueError as error:
        if str(error) != 'detached creator process vanished':
            raise
        # A crash after the atomic creation.json rename but before the status
        # rename is recoverable only by independently revalidating that receipt.
        candidate = checked(k + ['-n', args.namespace, 'exec', args.pod, '-c',
                                 pod['spec']['containers'][0]['name'], '--',
                                 'python3', '-c', REMOTE_CREATOR_RECEIPT])
        try:
            raw = base64.b64decode(candidate.strip(), validate=True)
        except ValueError as decode_error:
            raise error from decode_error
        output = remote_creator_receipt(k, args.namespace, args.pod,
                                        pod['spec']['containers'][0]['name'],
                                        {'creation_sha256': hashlib.sha256(raw).hexdigest()}, binding).decode()
    del registry_auth
    after = get('pod', args.pod, args.namespace)
    if inspect_pod(after,args.node,args.pod_uid,args.guest_image) != identity or after['spec'] != pod['spec']:
        raise ValueError('pod changed during disk creation; result not accepted')
    persist_receipt(args.creation_receipt, output)
    print(output,end='')


if __name__ == '__main__':
    try:
        main()
    except ControllerTransportError:
        # This is deliberately constant and credential-free.  The lane needs
        # to preserve the admitted pod instead of treating a proxy loss as a
        # failed creator job.
        print('TILO_CREATOR_CONTROLLER_TRANSPORT', file=sys.stderr)
        raise SystemExit(75)
