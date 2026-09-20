#!/usr/bin/env python3
"""Create, verify and boot one finalized ARM installer without rebuilding it.

The controller owns only disposable Kubernetes Pods and their emptyDirs.  It
never mounts a host path or physical disk.  All image, helper and worker
identities are pinned in the retained operation receipts.
"""
import argparse
import atexit
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import tarfile
import tempfile
import time


DISK_IMAGE = ('code.podesta.ai/qos/tilo-development-finalization-tools-aarch64@'
              'sha256:a8481a3265c2f4e47287a11163ab8cc13f31b070bba3566d62191c173b54d612')
VM_IMAGE = ('code.podesta.ai/qos/tilo-development-arm-qemu-tools@'
            'sha256:b0509e08595c64c149bb96382e4fad5ca4e4ef2120ee372c966463892b4270bc')
PROFILE_ENCODER_SHA256 = '423fdd3a5cde30972cad0dd809cf8b278b2b7950ee3609ac08a2166e10e99b25'
FIRMWARE = {
    '/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw':
        'cb71ab34e6c4b542ca4a4a3ba5a3a2158f418716edb065a4074a55cf755749f8',
    '/usr/share/edk2/aarch64/vars-template-pflash.raw':
        'b3b855c5a80310168051164986855692d1bdb06e67619856177965cd87c6774f',
}


def command(argv, payload=None, *, timeout=120, text=True):
    result = subprocess.run(argv, input=payload, capture_output=True, timeout=timeout, text=text)
    if result.returncode:
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout


def quiet_command(argv, *, timeout):
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout)
    if result.returncode:
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')


def kubectl(context, *args):
    return ['kubectl', '--context', context, *args]


def api_json(context, *args):
    return json.loads(command(kubectl(context, *args, '-o', 'json'), timeout=30))


def digest_reference(value):
    return isinstance(value, str) and re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', value)


def ready(node):
    return (not node['spec'].get('unschedulable')
            and not node['metadata'].get('deletionTimestamp')
            and any(row['type'] == 'Ready' and row['status'] == 'True'
                    for row in node['status'].get('conditions', [])))


def select_nodes(document, disk_name=None, vm_name=None):
    nodes = [node for node in document['items']
             if node['metadata'].get('labels', {}).get('kubernetes.io/arch') == 'arm64' and ready(node)]
    by_name = {node['metadata']['name']: node for node in nodes}
    if disk_name:
        disk = by_name.get(disk_name)
        if disk is None:
            raise ValueError('requested disk node is not a ready ARM node')
    else:
        choices = [node for node in nodes
                   if node['metadata'].get('labels', {}).get('agent-sandbox.podesta.ai/disk-kernel')]
        if not choices:
            raise ValueError('no ready ARM disk-kernel node exists')
        disk = sorted(choices, key=lambda row: row['metadata']['name'])[0]
    if vm_name:
        vm = by_name.get(vm_name)
        if vm is None:
            raise ValueError('requested VM node is not a ready ARM node')
    else:
        choices = [node for node in nodes
                   if node['metadata']['name'] != disk['metadata']['name']
                   and int(node['status'].get('allocatable', {}).get('devic.es/kvm', '0')) >= 1]
        if not choices:
            raise ValueError('no separate ready ARM KVM node exists')
        vm = sorted(choices, key=lambda row: (
          bool(row['metadata'].get('labels',{}).get('agent-sandbox.podesta.ai/disk-kernel')),
          row['metadata']['name']))[0]
    if vm['metadata']['name'] == disk['metadata']['name']:
        raise ValueError('disk and KVM acceptance workers must use separate nodes')
    return disk, vm


def select_verifier_node(document, disk, vm, verifier_name=None):
    nodes=[node for node in document['items'] if ready(node)
           and node['metadata'].get('labels',{}).get('kubernetes.io/arch')=='arm64']
    kernel=disk['metadata'].get('labels',{}).get('agent-sandbox.podesta.ai/disk-kernel')
    if verifier_name:
        choices=[node for node in nodes if node['metadata']['name']==verifier_name
                 and node['metadata'].get('labels',{}).get('agent-sandbox.podesta.ai/disk-kernel')==kernel]
    else:
        excluded={disk['metadata']['name'],vm['metadata']['name']}
        choices=[node for node in nodes
                 if node['metadata']['name'] not in excluded
                 and node['metadata'].get('labels',{}).get('agent-sandbox.podesta.ai/disk-kernel')==kernel]
    if len(choices)!=1 and verifier_name:
        raise ValueError('requested verifier node is not a ready ARM disk-kernel node')
    if not choices:
        raise ValueError('no separate ready ARM verifier node exists')
    return sorted(choices,key=lambda row:row['metadata']['name'])[0]


def disk_manifest(name, node, image=DISK_IMAGE, deadline=21600):
    if not digest_reference(image) or not re.fullmatch(r'[a-z0-9](?:[-a-z0-9]*[a-z0-9])?', name):
        raise ValueError('invalid disk worker identity')
    kernel = node['metadata'].get('labels', {}).get('agent-sandbox.podesta.ai/disk-kernel')
    if not kernel:
        raise ValueError('disk node lacks the reviewed kernel label')
    return {'apiVersion':'v1','kind':'Pod','metadata':{'name':name,'namespace':'kata-system'},
      'spec':{'runtimeClassName':'kata-qemu-runtime-rs','restartPolicy':'Never',
       'activeDeadlineSeconds':deadline,'automountServiceAccountToken':False,
       'nodeSelector':{'kubernetes.io/hostname':node['metadata']['name'],
                       'kubernetes.io/arch':'arm64','agent-sandbox.podesta.ai/disk-kernel':kernel,
                       'agent-sandbox.podesta.ai/kata':'true','katacontainers.io/kata-runtime':'true'},
       'tolerations':[{'key':'agent-sandbox.podesta.ai/dedicated','operator':'Equal','value':'true','effect':'NoSchedule'},
                      {'key':'ci-cgroup-enforced','operator':'Equal','value':'v1','effect':'NoSchedule'},
                      {'key':'node.kubernetes.io/not-ready','operator':'Exists','effect':'NoExecute','tolerationSeconds':300},
                      {'key':'node.kubernetes.io/unreachable','operator':'Exists','effect':'NoExecute','tolerationSeconds':300}],
       'imagePullSecrets':[{'name':'forgejo-pull-secret'}],
       'containers':[{'name':'disk','image':image,'command':['python3','-c','import time;time.sleep(21600)'],
        'env':[{'name':'REGISTRY_AUTH_FILE','value':'/work/registry-auth.json'}],
        'securityContext':{'privileged':True},
        'resources':{kind:{'cpu':'2','memory':'8Gi','ephemeral-storage':'136Gi'} for kind in ('requests','limits')},
        'volumeMounts':[{'name':'work','mountPath':'/work'},{'name':'tmp','mountPath':'/tmp'}]}],
       'volumes':[{'name':'work','emptyDir':{'sizeLimit':'128Gi'}},
                  {'name':'tmp','emptyDir':{'sizeLimit':'2Gi'}}]}}


def vm_manifest(name, node, image=VM_IMAGE):
    if not digest_reference(image) or not re.fullmatch(r'[a-z0-9](?:[-a-z0-9]*[a-z0-9])?', name):
        raise ValueError('invalid VM worker identity')
    return {'apiVersion':'v1','kind':'Pod','metadata':{'name':name,'namespace':'agent-sandboxes',
      'labels':{'app':'tilo-arm-install-acceptance'}},'spec':{
       'automountServiceAccountToken':False,'restartPolicy':'Never','activeDeadlineSeconds':21600,
       'nodeSelector':{'kubernetes.io/arch':'arm64','kubernetes.io/hostname':node['metadata']['name']},
       'tolerations':[{'operator':'Exists'}],'imagePullSecrets':[{'name':'forgejo-pull-secret'}],
       'containers':[{'name':'qemu','image':image,'command':['sleep','21000'],
        'securityContext':{'allowPrivilegeEscalation':False,'privileged':False,
          'readOnlyRootFilesystem':True,'capabilities':{'drop':['ALL']},
          'seccompProfile':{'type':'RuntimeDefault'}},
        'resources':{kind:{'cpu':'2','memory':'6Gi','ephemeral-storage':'128Gi','devic.es/kvm':'1'}
                     for kind in ('requests','limits')},
        'volumeMounts':[{'name':'work','mountPath':'/work'},{'name':'tmp','mountPath':'/tmp'}]}],
       'volumes':[{'name':'work','emptyDir':{'sizeLimit':'120Gi'}},
                  {'name':'tmp','emptyDir':{'medium':'Memory','sizeLimit':'64Mi'}}]}}


def write_private(path, value):
    path = Path(path); path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.' + path.name + '-', dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(value); stream.flush(); os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try: os.unlink(temporary)
        except FileNotFoundError: pass


def binding(pod, node, expected_manifest=None):
    status = pod['status'].get('containerStatuses', [])
    if (pod['status']['phase'] != 'Running' or len(status) != 1 or not status[0].get('ready')
            or status[0].get('restartCount') or pod['metadata'].get('deletionTimestamp')):
        raise ValueError('worker is not continuously ready')
    expected = pod['spec']['containers'][0]['image']
    if status[0].get('imageID') != expected or not digest_reference(expected):
        raise ValueError('running worker image differs from its pinned image')
    if expected_manifest is not None:
        wanted=expected_manifest['spec']; actual=pod['spec']
        scalar=('runtimeClassName','restartPolicy','activeDeadlineSeconds','automountServiceAccountToken',
                'nodeSelector','tolerations','imagePullSecrets','volumes')
        if any(actual.get(key)!=wanted.get(key) for key in scalar):
            raise ValueError('admitted worker specification differs from requested isolation')
        if (actual.get('hostPID') or actual.get('hostIPC') or actual.get('hostNetwork')
                or actual.get('initContainers') or actual.get('ephemeralContainers')
                or len(actual.get('containers',[]))!=1):
            raise ValueError('admitted worker contains an unexpected execution surface')
        keys=('name','image','command','env','securityContext','resources','volumeMounts')
        if any(actual['containers'][0].get(key)!=wanted['containers'][0].get(key) for key in keys):
            raise ValueError('admitted worker container differs from requested isolation')
    return {'pod':pod['metadata']['name'],'pod_uid':pod['metadata']['uid'],
            'namespace':pod['metadata']['namespace'],'node':pod['spec']['nodeName'],
            'node_uid':node['metadata']['uid'],'spec':pod['spec'],
            'image_id':status[0]['imageID']}


def require_same_worker(context, expected):
    pod = api_json(context, '-n', expected['namespace'], 'get', 'pod', expected['pod'],
                   '--request-timeout=15s')
    node = api_json(context, 'get', 'node', expected['node'], '--request-timeout=15s')
    if binding(pod, node) != expected:
        raise ValueError('worker identity or specification changed')


def create_worker(context, manifest):
    command(kubectl(context, 'create', '-f', '-'), json.dumps(manifest), timeout=30)
    namespace, name = manifest['metadata']['namespace'], manifest['metadata']['name']
    command(kubectl(context, '-n', namespace, 'wait', '--for=condition=Ready',
                    '--timeout=10m', 'pod/' + name), timeout=620)
    pod = api_json(context, '-n', namespace, 'get', 'pod', name, '--request-timeout=15s')
    node = api_json(context, 'get', 'node', pod['spec']['nodeName'], '--request-timeout=15s')
    return binding(pod, node, manifest)


def stream_file(context, namespace, pod, container, destination, content):
    script = 'umask 077; d=${1%/*}; mkdir -p "$d"; t="$1.tmp.$$"; trap \'rm -f "$t"\' EXIT; cat >"$t"; test -s "$t"; mv "$t" "$1"'
    command(kubectl(context, '-n', namespace, 'exec', '-i', pod, '-c', container,
                    '--', 'sh', '-ceu', script, 'sh', destination), content, timeout=120, text=False)


def stream_tree(context, namespace, pod, container, destination, archive):
    script = 'umask 077; mkdir -p "$1"; tar --no-same-owner -C "$1" -xf -'
    command(kubectl(context, '-n', namespace, 'exec', '-i', pod, '-c', container,
                    '--', 'sh', '-ceu', script, 'sh', destination), archive, timeout=300, text=False)


def public_archive(root):
    root = Path(root).resolve()
    if not root.is_dir():
        raise ValueError('public bundle must be a directory')
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode='w') as archive:
        for path in sorted(root.rglob('*')):
            info = path.lstat()
            if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                raise ValueError('public bundle contains a non-regular path')
            archive.add(path, arcname=path.relative_to(root), recursive=False)
    return out.getvalue()


def git_archive(root, revision):
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('helper revision must be a full Git commit')
    actual = command(['git','-C',str(root),'rev-parse','HEAD']).strip()
    if actual != revision:
        raise ValueError('helper checkout differs from requested revision')
    if command(['git','-C',str(root),'status','--porcelain','--untracked-files=no','--',
                'os/dev','os/tests/test_supervised_install_lane.py']):
        raise ValueError('tracked install-lane controller or helpers differ from the requested revision')
    return subprocess.run(['git','-C',str(root),'archive','--format=tar',revision,'os'],
                          check=True,capture_output=True).stdout


def registry_auth(args):
    if args.registry_auth:
        path = args.registry_auth
        info = path.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid!=os.geteuid() or info.st_mode&0o077
                or not 2<=info.st_size<=1024*1024):
            raise ValueError('registry auth must be a bounded regular file')
        fd=os.open(path,os.O_RDONLY|getattr(os,'O_NOFOLLOW',0))
        try:
            opened=os.fstat(fd)
            if (opened.st_dev,opened.st_ino,opened.st_size)!=(info.st_dev,info.st_ino,info.st_size):
                raise ValueError('registry auth changed while opening')
            chunks=[]
            while True:
                chunk=os.read(fd,1024*1024+1-sum(map(len,chunks)))
                if not chunk:break
                chunks.append(chunk)
                if sum(map(len,chunks))>1024*1024:raise ValueError('registry auth exceeds bound')
            value=b''.join(chunks)
        finally: os.close(fd)
    else:
        secret = api_json(args.context, '-n', args.registry_secret_namespace, 'get', 'secret',
                          args.registry_secret, '--request-timeout=15s')
        try: value = base64.b64decode(secret['data']['.dockerconfigjson'], validate=True)
        except (KeyError, ValueError) as error: raise ValueError('registry Secret lacks docker config JSON') from error
    parsed = json.loads(value)
    if not isinstance(parsed.get('auths'), dict) or not parsed['auths']:
        raise ValueError('registry auth has no registry entries')
    return value


def validate_disk_args(value):
    required = {'source','target','architecture','revision','manifest_sha256','composefs_digest',
      'systemd_version','public_bundle','output','generation','size_bytes','offline_key_signature',
      'disposable_guest','boot_profile','source_live_uki'}
    if set(value) != required:
        raise ValueError('disk arguments contain missing or additional fields')
    if (not digest_reference(value['source']) or value['architecture'] != 'aarch64'
            or value['public_bundle'] != '/work/public-trust'
            or value['output'] != '/work/disk-output/disk.raw'
            or value['size_bytes'] != 64*1024**3 or value['boot_profile'] != 'installer-media'
            or value['disposable_guest'] is not True or value['offline_key_signature'] is not True):
        raise ValueError('disk arguments differ from the admitted ARM installer-media lane')


def host_agent(context, node):
    pods = api_json(context, '-n', 'kata-system', 'get', 'pods', '-l', 'name=kata-deploy',
                    '--request-timeout=15s')['items']
    matches = [pod for pod in pods if pod['spec'].get('nodeName') == node
               and pod['status']['phase'] == 'Running' and pod['spec'].get('hostPID')]
    if len(matches) != 1 or matches[0]['metadata'].get('deletionTimestamp'):
        raise ValueError('node lacks one live kata-deploy host agent')
    return matches[0]


def controller(root, args, disk, agent, inspect_only, evidence_name='creator-environment.json'):
    argv = ['python3', str(root/'os/dev/create-supervised-kata-disk.py'),
      '--context',args.context,'--namespace',disk['namespace'],'--pod',disk['pod'],
      '--pod-uid',disk['pod_uid'],'--node',disk['node'],'--node-uid',disk['node_uid'],
      '--host-agent',agent['metadata']['name'],'--guest-image',DISK_IMAGE,
      '--pins',str(args.host_pins),'--disk-args',str(args.disk_args),
      '--evidence',str(args.operation_dir/evidence_name),
      '--helper-revision',args.helper_revision]
    if inspect_only:
        argv.append('--inspect-only')
    else:
        argv += ['--registry-auth',str(args.operation_dir/'registry-auth.json'),
                 '--creation-receipt',str(args.operation_dir/'creation.json')]
    return command(argv, timeout=7200)


def verify_disk(context, disk, values, auth):
    argv = ['python3','-B','/work/verifier/os/dev/verify-installed-development-disk.py']
    mapping = [('source','source'),('target','target'),('architecture','architecture'),
      ('revision','revision'),('manifest-sha256','manifest_sha256'),('generation','generation'),
      ('composefs-digest','composefs_digest'),('systemd-version','systemd_version')]
    for option, key in mapping: argv += ['--'+option,str(values[key])]
    argv += ['--public-bundle','/work/public-trust','--disk','/work/disk-output/disk.raw',
      '--creation-receipt','/work/disk-output/creation.json','--boot-profile','installer-media',
      '--source-live-uki',values['source_live_uki'],'--offline-key-signature']
    cleanup="umask 077; trap 'rm -f -- /work/registry-auth.json' EXIT HUP INT TERM; cat > /work/registry-auth.json; test -s /work/registry-auth.json; \"$@\""
    output=command(kubectl(context,'-n',disk['namespace'],'exec','-i',disk['pod'],'-c','disk','--',
      'sh','-ceu',cleanup,'sh','/usr/bin/unshare','--mount','--propagation','private','--',*argv),
      auth,timeout=7200,text=False).decode()
    command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk','--',
                    'sh','-ceu','test ! -e /work/registry-auth.json'),timeout=30)
    return output


def verify_finalized_source(context, verifier, values, creation):
    argv=['python3','-B','/work/verifier/os/dev/verify-finalized-development-image.py',
      '--helpers','/work/verifier','--image-id',creation['source']['image_id'],
      '--target',values['target'],'--public-bundle','/work/public-trust',
      '--manifest-sha256',values['manifest_sha256'],'--architecture','aarch64',
      '--generation',str(values['generation']),'--composefs-digest',values['composefs_digest'],
      '--systemd-version',values['systemd_version'],'--boot-profile','installer-media']
    document=json.loads(command(kubectl(context,'-n',verifier['namespace'],'exec',verifier['pod'],
      '-c','disk','--',*argv),timeout=7200))
    validate_finalized_verification(document,creation)
    return document


def validate_finalized_verification(document, creation, composefs_digest=None):
    verification=document.get('verification',document)
    audit=verification.get('layer_material_audit') or {}
    source=creation.get('source') or {}
    if ('verification' in document and (document.get('image_id')!=source.get('image_id')
                                        or document.get('composefs_digest')!=composefs_digest)):
        raise ValueError('finalized verification wrapper differs from the created source')
    if (audit.get('status')!='passed-scoped-scan' or audit.get('image_id')!=source.get('image_id')
            or verification.get('hardware_profile')!=source.get('hardware_profile')
            or verification.get('prediction')!=creation.get('boot')
            or not isinstance(verification.get('selected_boot'),dict)):
        raise ValueError('finalized source-layer verification does not bind the created medium')


def raw_identity(context, disk, container='disk', path='/work/disk-output/disk.raw'):
    script = "import hashlib,json,os,sys; p=sys.argv[1]; s=os.stat(p); h=hashlib.sha256(); f=open(p,'rb'); [h.update(c) for c in iter(lambda:f.read(8*1024*1024),b'')]; print(json.dumps({'size':s.st_size,'sha256':h.hexdigest()}))"
    value = json.loads(command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c',container,
                                       '--','python3','-c',script,path),timeout=7200))
    if value.get('size') != 64*1024**3 or not re.fullmatch(r'[0-9a-f]{64}',value.get('sha256','')):
        raise ValueError('created raw media identity is invalid')
    return value


def freeze_raw(context, disk):
    command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk','--',
                    'chmod','0400','/work/disk-output/disk.raw'),timeout=30)


def sparse_transfer(context, source_worker, target_worker, *, target_container, target_directory, target_name):
    source = subprocess.Popen(kubectl(context,'-n',source_worker['namespace'],'exec',source_worker['pod'],'-c','disk',
      '--','tar','--sparse','-C','/work/disk-output','-cf','-','disk.raw'),stdout=subprocess.PIPE)
    script = 'set -eu; umask 077; mkdir -p "$1"; test ! -e "$1/$2"; tar --sparse --no-same-owner -C "$1" -xf -; if [ "$2" != disk.raw ]; then mv "$1/disk.raw" "$1/$2"; fi'
    target = subprocess.Popen(kubectl(context,'-n',target_worker['namespace'],'exec','-i',target_worker['pod'],'-c',target_container,
      '--','sh','-ceu',script,'sh',target_directory,target_name),stdin=source.stdout,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    source.stdout.close()
    try:
        target.communicate(timeout=7200);source_rc=source.wait(timeout=30)
    except subprocess.TimeoutExpired:
        target.kill();source.kill();target.wait();source.wait()
        raise ValueError('sparse media transfer exceeded deadline')
    if source_rc or target.returncode:
        raise ValueError(f'sparse media transfer failed ({source_rc}, {target.returncode})')


def convert_and_cycle(context, vm, identity, acceptance_id):
    fixed = '\n'.join(f'{digest}  {path}' for path,digest in FIRMWARE.items())
    script = r'''set -euo pipefail
test "$(stat -c %s /work/verified/installer.raw)" = "$MEDIA_BYTES"
printf '%s  %s\n' "$MEDIA_SHA256" /work/verified/installer.raw | sha256sum -c -
printf '%s\n' "$FIRMWARE_ROWS" | sha256sum -c -
printf '%s  %s\n' "$TILO_PROFILE_ENCODER_SHA256" "$TILO_PROFILE_ENCODER" | sha256sum -c -
chmod 0400 /work/verified/installer.raw
test ! -e /work/verified/installer.qcow2
qemu-img convert -p -f raw -O qcow2 /work/verified/installer.raw /work/verified/installer.qcow2
qemu-img compare -f raw -F qcow2 /work/verified/installer.raw /work/verified/installer.qcow2
qemu-img info --output=json /work/verified/installer.qcow2 > /work/verified/conversion-info.json
sha256sum /work/verified/installer.raw /work/verified/installer.qcow2 > /work/verified/conversion.sha256
chmod 0400 /work/verified/installer.qcow2
test ! -e "$TILO_RUN" && test ! -e "$TILO_INSTALLER_CYCLE_STORAGE"
bash /work/source/os/dev/test-installer-cycle.sh /work/verified/installer.qcow2 --prepared-alongside --graphics software --out "$EVIDENCE"
grep -Fxq PASS "$EVIDENCE/verdict.txt"
grep -Fxq 0 "$EVIDENCE/exit-status.txt"
'''
    cycle = acceptance_id + '-prepared-alongside'
    env = ['env',f'MEDIA_BYTES={identity["size"]}',f'MEDIA_SHA256={identity["sha256"]}',
      f'FIRMWARE_ROWS={fixed}','TILO_VM_ARCH=aarch64','TILO_ACCEL=kvm','TILO_DISTROBOX=',
      'TILO_MEM=4096','TILO_SMP=2','TILO_SECBOOT=0','TILO_SECBOOT_KEYS=none',
      'TILO_PROFILE_ENCODER=/usr/local/bin/tilo-imager-profile',
      'TILO_PROFILE_ENCODER_SHA256='+PROFILE_ENCODER_SHA256,'TILO_VM_CYCLE='+cycle,
      'TILO_RUN=/work/'+cycle+'-run','TILO_INSTALLER_CYCLE_STORAGE=/work/'+cycle+'-storage',
      'TILO_INSTALLER_OUTPUT=/work/'+cycle+'-storage','EVIDENCE=/work/'+cycle+'-evidence']
    quiet_command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',*env,
                          'bash','-ceu',script),timeout=14400)
    return cycle


def acceptance_evidence(context, vm, cycle):
    paths = {'conversion_info':'/work/verified/conversion-info.json',
             'conversion_sha256':'/work/verified/conversion.sha256',
             'verdict':f'/work/{cycle}-evidence/verdict.txt',
             'exit_status':f'/work/{cycle}-evidence/exit-status.txt',
             'desktop_ocr':f'/work/{cycle}-evidence/15-desktop-config.txt',
             'browser_ocr':f'/work/{cycle}-evidence/18-browser-offline.txt',
             'browser_fixture':f'/work/{cycle}-evidence/browser-offline-fixture.txt',
             'browser_fixture_html':f'/work/{cycle}-evidence/browser-offline-fixture.html'}
    script = "import json,pathlib,sys;p=json.loads(sys.argv[1]);print(json.dumps({k:pathlib.Path(v).read_text() for k,v in p.items()},sort_keys=True))"
    evidence = json.loads(command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu',
      '--','python3','-c',script,json.dumps(paths)),timeout=60))
    if evidence.get('verdict') != 'PASS\n' or evidence.get('exit_status') != '0\n':
        raise ValueError('prepared-alongside evidence changed after the cycle')
    json.loads(evidence['conversion_info'])
    if len(evidence['conversion_sha256'].splitlines()) != 2:
        raise ValueError('conversion digest evidence is incomplete')
    validate_gui_evidence(evidence)
    return evidence


def validate_gui_evidence(evidence):
    if not re.search(r'cycle\W*tilo', evidence.get('desktop_ocr',''), re.IGNORECASE):
        raise ValueError('cycle evidence lacks the installed desktop hostname')
    if not re.search(r'tilo\s+browser\s+offline\s+ready', evidence.get('browser_ocr',''), re.IGNORECASE):
        raise ValueError('cycle evidence lacks rendered offline Browser content')
    rows={}
    for line in evidence.get('browser_fixture','').splitlines():
        if '=' not in line: raise ValueError('browser fixture evidence is malformed')
        key,value=line.split('=',1)
        if key in rows: raise ValueError('browser fixture evidence has duplicate fields')
        rows[key]=value
    url=re.fullmatch(r'http://10\.0\.2\.2:([0-9]{4,5})/index\.html',rows.get('url',''))
    if (set(rows)!={'url','sha256','transport','external_references'}
            or url is None or not 1024<=int(url.group(1))<=65535
            or not re.fullmatch(r'[0-9a-f]{64}',rows.get('sha256',''))
            or hashlib.sha256(evidence.get('browser_fixture_html','').encode()).hexdigest()!=rows.get('sha256')
            or rows.get('transport')!='qemu-user-local-gateway'
            or rows.get('external_references')!='none'):
        raise ValueError('browser fixture evidence does not describe the local offline page')


def collect_cycle_archive(context, vm, cycle, destination, limit=512*1024*1024):
    directory=cycle+'-evidence'
    process=subprocess.Popen(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu',
      '--','tar','-C','/work','-cf','-',directory),stdout=subprocess.PIPE,
      stderr=subprocess.DEVNULL)
    fd,temporary=tempfile.mkstemp(prefix='.cycle-evidence-',suffix='.tar',dir=destination.parent)
    size=0;digest=hashlib.sha256()
    try:
        os.fchmod(fd,0o600)
        selector=selectors.DefaultSelector();selector.register(process.stdout,selectors.EVENT_READ)
        deadline=time.monotonic()+600
        with os.fdopen(fd,'wb') as output:
            while True:
                remaining=deadline-time.monotonic()
                if remaining<=0:
                    process.kill();raise ValueError('cycle evidence export exceeded deadline')
                if not selector.select(min(1,remaining)):
                    if process.poll() is not None:break
                    continue
                chunk=os.read(process.stdout.fileno(),1024*1024)
                if not chunk:break
                size+=len(chunk)
                if size>limit:
                    process.kill();raise ValueError('cycle evidence archive exceeds bound')
                output.write(chunk);digest.update(chunk)
            output.flush();os.fsync(output.fileno())
        selector.close()
        if process.wait()!=0:raise ValueError('cycle evidence export failed')
        with tarfile.open(temporary,'r:') as archive:
            names=set()
            for member in archive:
                path=Path(member.name)
                if (path.is_absolute() or '..' in path.parts or path.parts[0]!=directory
                        or not (member.isfile() or member.isdir())):
                    raise ValueError('cycle evidence archive contains an unsafe member')
                names.add('/'.join(path.parts[1:]))
            required={'verdict.txt','exit-status.txt','boot-timing.txt',
                      'alongside-preservation-before-target-boot.txt',
                      '07-target-greeter.png','15-desktop-config.png',
                      '17-browser-open.png','18-browser-offline.png',
                      'browser-offline-fixture.txt','browser-offline-fixture.html'}
            if not required.issubset(names):
                raise ValueError('cycle evidence archive lacks cold-boot, desktop or Browser proof')
        os.replace(temporary,destination)
    finally:
        try:os.unlink(temporary)
        except FileNotFoundError:pass
        if process.poll() is None:process.kill();process.wait()
    return {'path':destination.name,'sha256':digest.hexdigest(),'bytes':size,
            'required_files':sorted(required)}


def delete_worker(context,worker):
    command(kubectl(context,'-n',worker['namespace'],'delete','pod',worker['pod'],
                    '--wait=true','--timeout=2m'),timeout=150)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--context',required=True);p.add_argument('--operation-dir',type=Path,required=True)
    p.add_argument('--disk-args',type=Path,required=True);p.add_argument('--public-bundle',type=Path,required=True)
    p.add_argument('--host-pins',type=Path,required=True);p.add_argument('--helper-revision',required=True)
    p.add_argument('--acceptance-id',required=True);p.add_argument('--disk-node');p.add_argument('--verifier-node');p.add_argument('--vm-node')
    auth=p.add_mutually_exclusive_group(required=True)
    auth.add_argument('--registry-auth',type=Path);auth.add_argument('--registry-secret',default=None)
    p.add_argument('--registry-secret-namespace',default='kata-system')
    args=p.parse_args()
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}',args.acceptance_id):
        p.error('acceptance ID must be a short DNS-safe fresh identifier')
    args.registry_secret = args.registry_secret or 'forgejo-pull-secret'
    args.operation_dir.mkdir(mode=0o700,parents=True,exist_ok=False)
    values=json.loads(args.disk_args.read_text());validate_disk_args(values)
    root=Path(__file__).resolve().parents[2]; helpers=git_archive(root,args.helper_revision)
    public=public_archive(args.public_bundle); auth_bytes=registry_auth(args)
    write_private(args.operation_dir/'registry-auth.json',auth_bytes)
    auth_path=args.operation_dir/'registry-auth.json'
    cleanup_auth=lambda: auth_path.unlink(missing_ok=True)
    atexit.register(cleanup_auth)
    nodes=api_json(args.context,'get','nodes','-l','kubernetes.io/arch=arm64','--request-timeout=20s')
    disk_node,vm_node=select_nodes(nodes,args.disk_node,args.vm_node)
    verifier_node=select_verifier_node(nodes,disk_node,vm_node,args.verifier_node)
    disk=create_worker(args.context,disk_manifest('tilo-disk-'+args.acceptance_id,disk_node))
    write_private(args.operation_dir/'worker-bindings.json',
                  (json.dumps({'creator':disk},indent=2,sort_keys=True)+'\n').encode())
    agent=host_agent(args.context,disk['node'])
    stream_tree(args.context,disk['namespace'],disk['pod'],'disk','/work/helpers',helpers)
    stream_tree(args.context,disk['namespace'],disk['pod'],'disk','/work/public-trust',public)
    controller(root,args,disk,agent,True)
    controller(root,args,disk,agent,False)
    creation=(args.operation_dir/'creation.json').read_bytes()
    freeze_raw(args.context,disk)
    identity=raw_identity(args.context,disk)
    write_private(args.operation_dir/'raw-identity.json',(json.dumps(identity,indent=2,sort_keys=True)+'\n').encode())
    # Only now provision the verifier. It has a distinct new Kata sandbox and
    # never coexisted with the privileged image execution.
    verifier=create_worker(args.context,disk_manifest('tilo-verify-'+args.acceptance_id,verifier_node,deadline=7200))
    write_private(args.operation_dir/'worker-bindings.json',
                  (json.dumps({'creator':disk,'verifier':verifier},indent=2,sort_keys=True)+'\n').encode())
    verifier_agent=host_agent(args.context,verifier['node'])
    controller(root,args,verifier,verifier_agent,True,'verifier-environment.json')
    require_same_worker(args.context,disk);require_same_worker(args.context,verifier)
    sparse_transfer(args.context,disk,verifier,target_container='disk',
                    target_directory='/work/disk-output',target_name='disk.raw')
    if raw_identity(args.context,verifier)!=identity:
        raise ValueError('independent verifier received different raw media')
    delete_worker(args.context,disk)
    stream_tree(args.context,verifier['namespace'],verifier['pod'],'disk','/work/verifier',helpers)
    stream_tree(args.context,verifier['namespace'],verifier['pod'],'disk','/work/public-trust',public)
    stream_file(args.context,verifier['namespace'],verifier['pod'],'disk','/work/disk-output/creation.json',creation)
    # One authenticated exec installs its cleanup trap before reading the
    # credential stream, then removes the file on success, error or disconnect.
    verification=verify_disk(args.context,verifier,values,auth_bytes)
    cleanup_auth();atexit.unregister(cleanup_auth);del auth_bytes
    parsed=json.loads(verification)
    if (parsed.get('purpose') != 'tilo-installed-boot-and-root-verification'
            or not parsed.get('checks') or not all(parsed['checks'].values())):
        raise ValueError('independent installed-disk verification did not pass')
    write_private(args.operation_dir/'installed-verification.json',(json.dumps(parsed,indent=2,sort_keys=True)+'\n').encode())
    finalized=verify_finalized_source(args.context,verifier,values,json.loads(creation))
    finalized_bytes=(json.dumps(finalized,indent=2,sort_keys=True)+'\n').encode()
    write_private(args.operation_dir/'finalized-verification.json',finalized_bytes)
    finalization_verification_sha256=hashlib.sha256(finalized_bytes).hexdigest()
    vm=create_worker(args.context,vm_manifest('tilo-vm-'+args.acceptance_id,vm_node))
    stream_tree(args.context,vm['namespace'],vm['pod'],'qemu','/work/source',helpers)
    write_private(args.operation_dir/'worker-bindings.json',
                  (json.dumps({'creator':disk,'verifier':verifier,'vm':vm},indent=2,sort_keys=True)+'\n').encode())
    require_same_worker(args.context,verifier);require_same_worker(args.context,vm)
    sparse_transfer(args.context,verifier,vm,target_container='qemu',
                    target_directory='/work/verified',target_name='installer.raw')
    if raw_identity(args.context,vm,'qemu','/work/verified/installer.raw')!=identity:
        raise ValueError('KVM worker received different verified raw media')
    delete_worker(args.context,verifier)
    require_same_worker(args.context,vm)
    cycle=convert_and_cycle(args.context,vm,identity,args.acceptance_id)
    require_same_worker(args.context,vm)
    cycle_evidence=acceptance_evidence(args.context,vm,cycle)
    write_private(args.operation_dir/'cycle-evidence.json',
                  (json.dumps(cycle_evidence,indent=2,sort_keys=True)+'\n').encode())
    archive=collect_cycle_archive(args.context,vm,cycle,args.operation_dir/'cycle-evidence.tar')
    require_same_worker(args.context,vm)
    result={'schema':1,'purpose':'tilo-arm-installer-prepared-alongside-acceptance',
      'helper_revision':args.helper_revision,'source':values['source'],'raw_media':identity,
      'finalized_verification_sha256':finalization_verification_sha256,
      'creator_worker':disk,'verifier_worker':verifier,'vm_worker':vm,
      'creator_and_verifier_released':True,'cycle':cycle,'cold_boot_verified':True,
      'prepared_alongside_verified':True,'desktop_verified':True,
      'browser_offline_page_verified':True,'cycle_evidence_file':'cycle-evidence.json',
      'cycle_evidence_archive':archive,
      'workers_retained_for_followup':True}
    write_private(args.operation_dir/'acceptance.json',(json.dumps(result,indent=2,sort_keys=True)+'\n').encode())
    print(json.dumps(result,sort_keys=True))


if __name__=='__main__':
    try: main()
    except (ValueError,OSError,KeyError,TypeError,subprocess.TimeoutExpired) as error:
        raise SystemExit('supervised install lane: '+str(error))
