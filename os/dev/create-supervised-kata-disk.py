#!/usr/bin/env python3
"""External operator launcher; Kubernetes credentials never enter the guest.

Requires an already provisioned, sleeping, dedicated Kata pod. Inspection is
performed against its live host processes immediately before authenticated exec.
No pod creation, credential provisioning, node mutation or disk publication.
"""
import argparse
import base64
import io
import json
from pathlib import Path
import re
import subprocess
import time
import zipfile


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
print(json.dumps({'container':c,'sandbox':s,'pid':int(d.name),'start':(d/'stat').read_text().split()[21],'argv':argv,'fds':fds,'mountinfo':(shims[0]/'mountinfo').read_text(),'virtiofs':servers[0],'hashes':hashes}))
'''

GUEST_RUN = r'''
import base64,importlib.util,io,json,pathlib,sys,tempfile,types,zipfile
p=json.load(sys.stdin)
with tempfile.TemporaryDirectory(prefix='tilo-supervised-helper-') as d:
 z=zipfile.ZipFile(io.BytesIO(base64.b64decode(p['helpers'])))
 for name in z.namelist():
  assert pathlib.PurePosixPath(name).name==name and name.endswith('.py')
  pathlib.Path(d,name).write_bytes(z.read(name))
 sys.path.insert(0,d)
 spec=importlib.util.spec_from_file_location('disk_creator',pathlib.Path(d,'create-development-disk.py'));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
 args=p['args']
 for name in ('public_bundle','output'):args[name]=pathlib.Path(args[name])
 print(json.dumps(m.create_from_supervised_controller(types.SimpleNamespace(**args),p['observation']),sort_keys=True))
'''


def checked(argv, payload=None):
    result = subprocess.run(argv, input=payload, text=True, capture_output=True)
    if result.returncode:
        # Kubernetes and registry errors can contain sensitive configuration.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout


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
    disk_args = json.loads(args.disk_args.read_text())
    bundle = io.BytesIO()
    with zipfile.ZipFile(bundle,'w',zipfile.ZIP_DEFLATED) as archive:
        for path in Path(__file__).parent.glob('*.py'):
            archive.write(path,path.name)
    payload = {'observation':observation,'args':disk_args,'helpers':base64.b64encode(bundle.getvalue()).decode()}
    output = checked(k+['-n',args.namespace,'exec','-i',args.pod,'-c',pod['spec']['containers'][0]['name'],'--','/usr/bin/unshare','--mount','--propagation','private','--','python3','-c',GUEST_RUN],json.dumps(payload))
    after = get('pod', args.pod, args.namespace)
    if inspect_pod(after,args.node,args.pod_uid,args.guest_image) != identity or after['spec'] != pod['spec']:
        raise ValueError('pod changed during disk creation; result not accepted')
    print(output,end='')


if __name__ == '__main__':
    main()
