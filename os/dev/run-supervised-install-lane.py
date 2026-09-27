#!/usr/bin/env python3
"""Create, verify and boot one finalized ARM installer without rebuilding it.

The controller owns only disposable Kubernetes Pods and their emptyDirs.  It
never mounts a host path or physical disk.  All image, helper and worker
identities are pinned in the retained operation receipts.
"""
import argparse
import atexit
import base64
from contextlib import contextmanager
import hashlib
import ipaddress
import io
import json
import os
from pathlib import Path
import re
import secrets
import selectors
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from typing import Any, TypedDict

import installer_bootc_helper


DISK_IMAGE = ('registry.tilo.so/tilo/tilo-development-finalization-tools-aarch64@'
              'sha256:454c4bbf89adc975ffb7030d1bbef8a6ce06f8a28eb4df38ec671ab358c4d9c2')
VM_IMAGE = ('registry.tilo.so/tilo/tilo-development-arm-qemu-tools@'
            'sha256:b0509e08595c64c149bb96382e4fad5ca4e4ef2120ee372c966463892b4270bc')
PROFILE_ENCODER_SHA256 = '423fdd3a5cde30972cad0dd809cf8b278b2b7950ee3609ac08a2166e10e99b25'
FIRMWARE = {
    '/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw':
        'cb71ab34e6c4b542ca4a4a3ba5a3a2158f418716edb065a4074a55cf755749f8',
    '/usr/share/edk2/aarch64/vars-template-pflash.raw':
        'b3b855c5a80310168051164986855692d1bdb06e67619856177965cd87c6774f',
}
PAUSE_TTL_SECONDS = 12 * 60 * 60
OWNER_LABEL = 'tilo.so/owner'
OPERATION_LABEL = 'tilo.so/operation'
EXPIRES_LABEL = 'tilo.so/expires-at'
OWNER = 'supervised-install-lane'


class ControllerTransportError(ValueError):
    """The local controller lost its path to Kubernetes."""


class CreatorControllerFailure(ValueError):
    """The creator controller ran and failed before producing remote evidence."""


class CreatorJobFailed(ValueError):
    """The exact detached creator wrote a terminal failure record."""


def kubectl_transport_error(stderr):
    """Whether kubectl's bounded, nonzero response is safe to resume."""
    text = stderr.lower()
    return any(marker in text for marker in (
        'unable to connect', 'connection refused', 'connection reset',
        'context deadline', 'i/o timeout', 'tls handshake timeout',
        'server has asked for the client to provide', 'timed out waiting',
        'unexpected eof', 'client.timeout exceeded', 'request canceled',
        'server closed idle connection', 'http2: client connection lost',
        'transport is closing', 'network is unreachable', 'no route to host',
        'the request timed out', 'timeout: request did not complete',
        'creator status poll lost transport', 'kubectl transport failed'))


class CreatorInputReconciliationError(ValueError):
    """Pending input state cannot be reconciled safely in place."""


class CreatorBinding(TypedDict):
    pod: str
    pod_uid: str
    namespace: str
    node: str
    node_uid: str
    spec: dict[str, Any]
    image_id: str


class RawIdentity(TypedDict):
    size: int
    sha256: str


class CreatorCheckpoint(TypedDict):
    schema: int
    purpose: str
    helper_revision: str
    acceptance_id: str
    creator: CreatorBinding
    creator_node: dict[str, Any]
    creation_sha256: str
    raw_identity: RawIdentity
    expires_at: int


TIMING_STAGES = ('creator_admission', 'input_transfer', 'create_install',
                 'freeze_identity', 'verifier_provision_transfer', 'verification',
                 'vm_transfer', 'conversion_cycle', 'final_identity')


class StageTiming(TypedDict):
    name: str
    status: str
    started_offset_ns: int
    duration_ns: int


class LaneTimingReceipt(TypedDict):
    schema: int
    purpose: str
    clock: str
    status: str
    total_duration_ns: int
    stages: list[StageTiming]


def validate_lane_timings(receipt: LaneTimingReceipt) -> None:
    if (set(receipt) != {'schema','purpose','clock','status','total_duration_ns','stages'}
            or receipt['schema'] != 1
            or receipt['purpose'] != 'tilo-supervised-install-lane-timings'
            or receipt['clock'] != 'monotonic_ns'
            or receipt['status'] not in {'running','succeeded','failed'}
            or type(receipt['total_duration_ns']) is not int
            or receipt['total_duration_ns'] < 0
            or not isinstance(receipt['stages'], list)):
        raise ValueError('invalid supervised lane timing receipt')
    previous_end = 0
    for index, stage in enumerate(receipt['stages']):
        if (not isinstance(stage, dict)
                or set(stage) != {'name','status','started_offset_ns','duration_ns'}
                or index >= len(TIMING_STAGES) or stage['name'] != TIMING_STAGES[index]
                or stage['status'] not in {'succeeded','skipped','failed'}
                or type(stage['started_offset_ns']) is not int
                or type(stage['duration_ns']) is not int
                or stage['started_offset_ns'] < previous_end or stage['duration_ns'] < 0
                or (stage['status'] == 'skipped' and stage['duration_ns'] != 0)):
            raise ValueError('invalid supervised lane timing stage')
        previous_end = stage['started_offset_ns'] + stage['duration_ns']
    if previous_end > receipt['total_duration_ns']:
        raise ValueError('supervised lane timing total precedes its stages')
    failed = [stage for stage in receipt['stages'] if stage['status'] == 'failed']
    if (receipt['status'] == 'failed') != (len(failed) == 1):
        raise ValueError('supervised lane timing failure status differs from its stages')
    if failed and receipt['stages'][-1] is not failed[0]:
        raise ValueError('supervised lane timing continued after failure')
    if receipt['status'] == 'succeeded' and len(receipt['stages']) != len(TIMING_STAGES):
        raise ValueError('successful supervised lane timing receipt is incomplete')


class LaneTimer:
    def __init__(self, path: Path, clock=time.monotonic_ns):
        self.path = path
        self.clock = clock
        self.started = clock()
        self.stages: list[StageTiming] = []
        self.status = 'running'

    def receipt(self) -> LaneTimingReceipt:
        value: LaneTimingReceipt = {
          'schema':1, 'purpose':'tilo-supervised-install-lane-timings',
          'clock':'monotonic_ns', 'status':self.status,
          'total_duration_ns':self.clock()-self.started, 'stages':list(self.stages)}
        validate_lane_timings(value)
        return value

    def persist(self) -> None:
        write_private(self.path,
          (json.dumps(self.receipt(),indent=2,sort_keys=True)+'\n').encode())

    def skip(self, name: str) -> None:
        self._expect(name)
        offset = self.clock()-self.started
        self.stages.append({'name':name,'status':'skipped',
                            'started_offset_ns':offset,'duration_ns':0})
        print(f'lane timing: {name}=skipped', file=sys.stderr, flush=True)

    def _expect(self, name: str) -> None:
        if len(self.stages) >= len(TIMING_STAGES) or TIMING_STAGES[len(self.stages)] != name:
            raise ValueError('supervised lane timing stages are out of order')

    @contextmanager
    def stage(self, name: str):
        self._expect(name)
        started = self.clock(); offset = started-self.started
        try:
            yield
        except BaseException:
            duration = self.clock()-started
            self.stages.append({'name':name,'status':'failed',
                                'started_offset_ns':offset,'duration_ns':duration})
            self.status = 'failed'; self.persist()
            print(f'lane timing: {name}=failed {duration/1_000_000_000:.3f}s',
                  file=sys.stderr,flush=True)
            raise
        duration = self.clock()-started
        self.stages.append({'name':name,'status':'succeeded',
                            'started_offset_ns':offset,'duration_ns':duration})
        print(f'lane timing: {name}={duration/1_000_000_000:.3f}s',
              file=sys.stderr,flush=True)

    def finish(self) -> LaneTimingReceipt:
        if len(self.stages) != len(TIMING_STAGES):
            raise ValueError('cannot finish incomplete supervised lane timings')
        self.status = 'succeeded'; self.persist()
        return self.receipt()


def command(argv, payload=None, *, timeout=120, text=True):
    try:
        result = subprocess.run(argv, input=payload, capture_output=True, timeout=timeout, text=text)
    except (OSError, subprocess.TimeoutExpired) as error:
        if Path(argv[0]).name == 'kubectl':
            raise ControllerTransportError('kubectl transport failed') from error
        raise
    if result.returncode:
        stderr = result.stderr if isinstance(result.stderr, str) else result.stderr.decode(errors='replace')
        if Path(argv[0]).name == 'kubectl' and kubectl_transport_error(stderr):
            raise ControllerTransportError(f'kubectl failed (exit {result.returncode})')
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout


def quiet_command(argv, *, timeout):
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout)
    if result.returncode:
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')


def canonical_sha256(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True,
                                     separators=(',', ':')).encode()).hexdigest()


def kubectl(context, *args):
    return ['kubectl', '--context', context, *args]


def api_json(context, *args):
    return json.loads(command(kubectl(context, *args, '-o', 'json'), timeout=30))


def optional_api_json(context, *args):
    try:
        result=subprocess.run(kubectl(context,*args,'-o','json'),capture_output=True,
                              text=True,timeout=30)
    except (OSError,subprocess.TimeoutExpired) as error:
        raise ControllerTransportError('kubectl transport failed') from error
    if result.returncode:
        stderr=result.stderr.lower()
        if 'notfound' in stderr or 'not found' in stderr:
            return None
        if kubectl_transport_error(stderr):
            raise ControllerTransportError(f'kubectl failed (exit {result.returncode})')
        raise ValueError(f'kubectl failed (exit {result.returncode})')
    return json.loads(result.stdout)


def digest_reference(value):
    return isinstance(value, str) and re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', value)


def ready(node):
    return (not node['spec'].get('unschedulable')
            and not node['metadata'].get('deletionTimestamp')
            and any(row['type'] == 'Ready' and row['status'] == 'True'
                    for row in node['status'].get('conditions', [])))


def select_nodes(document, disk_name=None, vm_name=None, excluded_vm_names=()):
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
        if vm is None or vm_name in set(excluded_vm_names):
            raise ValueError('requested VM node is not a ready ARM node')
    else:
        choices = [node for node in nodes
                   if node['metadata']['name'] != disk['metadata']['name']
                   and node['metadata']['name'] not in set(excluded_vm_names)
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
                 and node['metadata']['name'] not in {disk['metadata']['name'],vm['metadata']['name']}
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


def disk_manifest(name, node, image=DISK_IMAGE, deadline=43200, *, operation=None,
                  expires_at=None):
    if not digest_reference(image) or not re.fullmatch(r'[a-z0-9](?:[-a-z0-9]*[a-z0-9])?', name):
        raise ValueError('invalid disk worker identity')
    kernel = node['metadata'].get('labels', {}).get('agent-sandbox.podesta.ai/disk-kernel')
    if not kernel:
        raise ValueError('disk node lacks the reviewed kernel label')
    labels = {}
    if operation is not None:
        if (not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}', operation)
                or type(expires_at) is not int or expires_at <= 0):
            raise ValueError('invalid resumable creator ownership')
        labels = {OWNER_LABEL: OWNER, OPERATION_LABEL: operation,
                  EXPIRES_LABEL: str(expires_at)}
    return {'apiVersion':'v1','kind':'Pod','metadata':{'name':name,'namespace':'kata-system',
      'labels':labels},
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
                  # Registry credentials for the detached creator exist only
                  # below /tmp and therefore disappear with the Kata guest.
                  {'name':'tmp','emptyDir':{'medium':'Memory','sizeLimit':'64Mi'}}]}}


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


def validate_creator_binding(value):
    required = {'pod','pod_uid','namespace','node','node_uid','spec','image_id'}
    if (not isinstance(value, dict) or set(value) != required
            or any(not isinstance(value[key], str) or not value[key]
                   for key in required - {'spec'})
            or not isinstance(value['spec'], dict)
            or value['namespace'] != 'kata-system'
            or not digest_reference(value['image_id'])):
        raise ValueError('creator checkpoint has an invalid worker binding')


def validate_creator_node(value, creator):
    try:
        metadata = value['metadata']; labels = metadata['labels']
        valid = (isinstance(value, dict) and metadata['name'] == creator['node']
                 and metadata['uid'] == creator['node_uid']
                 and labels.get('kubernetes.io/arch') == 'arm64'
                 and isinstance(labels.get('agent-sandbox.podesta.ai/disk-kernel'), str)
                 and bool(labels['agent-sandbox.podesta.ai/disk-kernel']))
    except (KeyError, TypeError):
        valid = False
    if not valid:
        raise ValueError('creator checkpoint has an invalid full node document')


def validate_raw_identity(value):
    if (not isinstance(value, dict) or set(value) != {'size','sha256'}
            or type(value['size']) is not int or value['size'] != 64 * 1024**3
            or not isinstance(value['sha256'], str)
            or not re.fullmatch(r'[0-9a-f]{64}', value['sha256'])):
        raise ValueError('creator checkpoint has an invalid raw identity')


def validate_creator_checkpoint(value):
    required = {'schema','purpose','helper_revision','acceptance_id','creator','creator_node',
                'creation_sha256','raw_identity','expires_at'}
    if (not isinstance(value, dict) or set(value) != required or value['schema'] != 1
            or value['purpose'] != 'tilo-supervised-creator-checkpoint'
            or not re.fullmatch(r'[0-9a-f]{40}', value['helper_revision'])
            or not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}', value['acceptance_id'])
            or not re.fullmatch(r'[0-9a-f]{64}', value['creation_sha256'])
            or type(value['expires_at']) is not int or value['expires_at'] <= 0):
        raise ValueError('creator checkpoint has invalid metadata')
    validate_creator_binding(value['creator'])
    validate_creator_node(value['creator_node'], value['creator'])
    validate_raw_identity(value['raw_identity'])


def checkpoint_creator(args, creator, creator_node, creation, identity, expires_at):
    if (args.operation_dir / 'registry-auth.json').exists():
        raise ValueError('creator checkpoint cannot retain registry credentials')
    document: CreatorCheckpoint = {
      'schema':1, 'purpose':'tilo-supervised-creator-checkpoint',
      'helper_revision':args.helper_revision, 'acceptance_id':args.acceptance_id,
      'creator':creator, 'creator_node':creator_node,
      'creation_sha256':hashlib.sha256(creation).hexdigest(),
      'raw_identity':identity, 'expires_at':expires_at}
    validate_creator_checkpoint(document)
    write_private(args.operation_dir/'creator-checkpoint.json',
                  (json.dumps(document,indent=2,sort_keys=True)+'\n').encode())
    return document


def load_creator_checkpoint(args, now=None):
    path=args.operation_dir/'creator-checkpoint.json'
    document=json.loads(path.read_text())
    validate_creator_checkpoint(document)
    if (document['helper_revision']!=args.helper_revision
            or document['acceptance_id']!=args.acceptance_id):
        raise ValueError('creator checkpoint belongs to a different operation')
    creation=(args.operation_dir/'creation.json').read_bytes()
    identity=json.loads((args.operation_dir/'raw-identity.json').read_text())
    if (hashlib.sha256(creation).hexdigest()!=document['creation_sha256']
            or identity!=document['raw_identity']):
        raise ValueError('creator checkpoint durable media evidence changed')
    if (now if now is not None else int(time.time())) >= document['expires_at']:
        raise ValueError('creator checkpoint expired and must be cleaned up')
    if (args.operation_dir/'registry-auth.json').exists():
        raise ValueError('paused creator retained registry credentials')
    return document,creation,identity


def paused_cleanup_command(context, operation_dir):
    return [os.fspath(Path(__file__).resolve()),'--context',context,
            '--operation-dir',os.fspath(operation_dir),'--cleanup-paused']


def pause_creator(args, checkpoint, reason):
    if (args.operation_dir/'registry-auth.json').exists():
        raise ValueError('refusing to pause a creator while credentials remain')
    document={'schema':1,'purpose':'tilo-supervised-creator-paused','status':'paused',
      'acceptance_id':args.acceptance_id,'creator':checkpoint['creator'],
      'creator_checkpoint_sha256':hashlib.sha256(
        (args.operation_dir/'creator-checkpoint.json').read_bytes()).hexdigest(),
      'expires_at':checkpoint['expires_at'],'reason':reason,
      'cleanup_command':paused_cleanup_command(args.context,args.operation_dir)}
    write_private(args.operation_dir/'paused-creator.json',
                  (json.dumps(document,indent=2,sort_keys=True)+'\n').encode())
    return document


def validate_pause_receipt(value, checkpoint):
    required={'schema','purpose','status','acceptance_id','creator',
              'creator_checkpoint_sha256','expires_at','reason','cleanup_command'}
    if (not isinstance(value,dict) or set(value)!=required or value['schema']!=1
            or value['purpose']!='tilo-supervised-creator-paused' or value['status']!='paused'
            or value['acceptance_id']!=checkpoint['acceptance_id']
            or value['creator']!=checkpoint['creator']
            or value['expires_at']!=checkpoint['expires_at']
            or not re.fullmatch(r'[0-9a-f]{64}',value['creator_checkpoint_sha256'])
            or not isinstance(value['reason'],str) or not value['reason']
            or not isinstance(value['cleanup_command'],list)
            or not all(isinstance(item,str) and item for item in value['cleanup_command'])):
        raise ValueError('paused creator receipt has an invalid type or binding')


def current_creator_node(nodes, checkpoint):
    matches=[node for node in nodes.get('items',[])
             if node.get('metadata',{}).get('name')==checkpoint['creator']['node']]
    if len(matches)!=1:
        raise ValueError('paused creator node no longer exists')
    validate_creator_node(matches[0],checkpoint['creator'])
    return matches[0]


def binding(pod, node, expected_manifest=None):
    status = pod['status'].get('containerStatuses', [])
    if (pod['status']['phase'] != 'Running' or len(status) != 1 or not status[0].get('ready')
            or status[0].get('restartCount') or pod['metadata'].get('deletionTimestamp')):
        raise ValueError('worker is not continuously ready')
    expected = pod['spec']['containers'][0]['image']
    if status[0].get('imageID') != expected or not digest_reference(expected):
        raise ValueError('running worker image differs from its pinned image')
    if expected_manifest is not None:
        wanted_metadata=expected_manifest['metadata'];actual_metadata=pod['metadata']
        if (actual_metadata.get('name')!=wanted_metadata.get('name')
                or actual_metadata.get('namespace')!=wanted_metadata.get('namespace')
                or any(actual_metadata.get('labels',{}).get(key)!=value
                       for key,value in wanted_metadata.get('labels',{}).items())):
            raise ValueError('admitted worker metadata differs from requested ownership')
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


def optional_same_worker(context, expected, expected_manifest=None):
    pod=optional_api_json(context,'-n',expected['namespace'],'get','pod',expected['pod'],
                          '--request-timeout=15s')
    if pod is None:
        return None
    node=api_json(context,'get','node',expected['node'],'--request-timeout=15s')
    observed=binding(pod,node,expected_manifest)
    if observed!=expected:
        raise ValueError('worker identity or specification changed')
    return observed


def adopt_worker(context, manifest):
    namespace,name=manifest['metadata']['namespace'],manifest['metadata']['name']
    pod=optional_api_json(context,'-n',namespace,'get','pod',name,'--request-timeout=15s')
    if pod is None:
        return None
    command(kubectl(context,'-n',namespace,'wait','--for=condition=Ready',
                    '--timeout=10m','pod/'+name),timeout=620)
    pod=api_json(context,'-n',namespace,'get','pod',name,'--request-timeout=15s')
    node=api_json(context,'get','node',pod['spec']['nodeName'],'--request-timeout=15s')
    return binding(pod,node,manifest)


def create_worker(context, manifest):
    command(kubectl(context, 'create', '-f', '-'), json.dumps(manifest), timeout=30)
    namespace, name = manifest['metadata']['namespace'], manifest['metadata']['name']
    command(kubectl(context, '-n', namespace, 'wait', '--for=condition=Ready',
                    '--timeout=10m', 'pod/' + name), timeout=620)
    pod = api_json(context, '-n', namespace, 'get', 'pod', name, '--request-timeout=15s')
    node = api_json(context, 'get', 'node', pod['spec']['nodeName'], '--request-timeout=15s')
    return binding(pod, node, manifest)


def create_or_adopt_worker(context, manifest):
    existing=adopt_worker(context,manifest)
    if existing is not None:
        return existing
    try:
        return create_worker(context,manifest)
    except ValueError:
        # A controller restart can race the original create after the API has
        # accepted the Pod.  Adopt only the exact deterministic manifest.
        existing=adopt_worker(context,manifest)
        if existing is None:
            raise
        return existing


def persist_creator_create_intent(operation_dir, operation, manifest):
    value={'schema':1,'purpose':'tilo-supervised-creator-create-intent','operation':operation,
           'manifest_sha256':canonical_sha256(manifest),'manifest':manifest}
    write_private(Path(operation_dir)/'creator-create-intent.json',
                  (json.dumps(value,indent=2,sort_keys=True)+'\n').encode())
    return value


def load_creator_create_intent(operation_dir, operation):
    value=json.loads((Path(operation_dir)/'creator-create-intent.json').read_text())
    if (set(value)!={'schema','purpose','operation','manifest_sha256','manifest'}
            or value['schema']!=1 or value['purpose']!='tilo-supervised-creator-create-intent'
            or value['operation']!=operation or value['manifest_sha256']!=canonical_sha256(value['manifest'])):
        raise ValueError('creator create intent differs from its deterministic manifest')
    manifest=value['manifest']
    if (not isinstance(manifest,dict) or manifest.get('metadata',{}).get('name')!='tilo-disk-'+operation
            or manifest.get('metadata',{}).get('namespace')!='kata-system'):
        raise ValueError('creator create intent has an invalid target')
    return value


def stream_file(context, namespace, pod, container, destination, content):
    script = 'umask 077; d=${1%/*}; mkdir -p "$d"; t="$1.tmp.$$"; trap \'rm -f "$t"\' EXIT; cat >"$t"; test -s "$t"; mv "$t" "$1"'
    command(kubectl(context, '-n', namespace, 'exec', '-i', pod, '-c', container,
                    '--', 'sh', '-ceu', script, 'sh', destination), content, timeout=120, text=False)


def stream_tree(context, namespace, pod, container, destination, archive):
    script = 'umask 077; mkdir -p "$1"; tar --no-same-owner -C "$1" -xf -'
    command(kubectl(context, '-n', namespace, 'exec', '-i', pod, '-c', container,
                    '--', 'sh', '-ceu', script, 'sh', destination), archive, timeout=300, text=False)


def creator_input_components(helpers, public, installer_bootc):
    return {'helpers': ('helpers.tar', helpers),
            'public': ('public-trust.tar', public),
            'installer': ('installer-bootc-helper.tar', installer_bootc)}


def creator_input_archive(helpers, public, installer_bootc, selected=None):
    """Wrap independently authenticated creator inputs for one remote exec."""
    components = creator_input_components(helpers, public, installer_bootc)
    selected = tuple(components) if selected is None else tuple(selected)
    inputs = {components[name][0]: components[name][1] for name in selected}
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode='w') as archive:
        for name, content in inputs.items():
            info = tarfile.TarInfo(name)
            info.size = len(content); info.mode = 0o600
            archive.addfile(info, io.BytesIO(content))
    return out.getvalue(), {name: hashlib.sha256(content).hexdigest()
                            for name, content in inputs.items()}


REMOTE_CREATOR_INPUT_STATUS = r'''import hashlib,json,os,pathlib,re,stat,sys
work=pathlib.Path('/work'); job=work/'disk-output/.creator-job'; receipt=work/'.creator-inputs.json'
marker=work/'.creator-inputs-reconcile.json'
names={'helpers':'helpers','public':'public-trust','installer':'installer-bootc-helper'}
expected_pod_uid=sys.argv[1]; expected_node_uid=sys.argv[2]; expected_operation=sys.argv[3]
def result(state,**extra):
 print(json.dumps({'state':state,**extra},sort_keys=True)); raise SystemExit
def read_private(path,limit):
 try:i=path.lstat()
 except FileNotFoundError:return None
 except OSError:result('invalid')
 if (not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1
     or i.st_mode&0o077 or i.st_size<2 or i.st_size>limit):result('invalid')
 try:
  fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW); o=os.fstat(fd)
  if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):result('invalid')
  value=os.read(fd,limit+1); os.close(fd)
  if len(value)!=i.st_size:result('invalid')
  return value
 except BaseException:result('invalid')
def exact_job(path):
 if path.is_symlink() or not path.is_dir():result('active')
 info=path.lstat()
 if info.st_uid or info.st_gid or stat.S_IMODE(info.st_mode)!=0o700:result('active')
 try:status=json.loads(read_private(path/'status.json',65536))
 except BaseException:result('active')
 binding=status.get('binding') if isinstance(status,dict) else None
 if (not isinstance(status,dict) or status.get('schema')!=1
     or status.get('purpose')!='tilo-supervised-creator-job'
     or not isinstance(binding,dict)
     or binding.get('operation')!=expected_operation
     or binding.get('pod_uid')!=expected_pod_uid
     or binding.get('node_uid')!=expected_node_uid
     or binding.get('output')!='/work/disk-output/disk.raw'):
  result('active')
 state=status.get('state'); base={'schema','purpose','state','binding'}
 if state in ('running','succeeded'):
  binding_fields={'schema','purpose','operation','pod_uid','node_uid','container_id','image','output',
                  'helper_revision','helper_bundle_sha256','job_sha256','disk_args_sha256',
                  'observation_sha256'}
  expected=base|{'process'}
  if state=='succeeded':expected|={'creation_sha256','log_bytes','log_truncated'}
  process=status.get('process')
  if (set(binding)!=binding_fields or binding.get('schema')!=1
      or binding.get('purpose')!='tilo-supervised-creator-job-binding'
      or not isinstance(binding.get('container_id'),str) or not binding['container_id']
      or not isinstance(binding.get('image'),str) or not binding['image']
      or not re.fullmatch(r'[0-9a-f]{40}',binding.get('helper_revision',''))
      or any(not re.fullmatch(r'[0-9a-f]{64}',binding.get(name,'')) for name in
             ('helper_bundle_sha256','job_sha256','disk_args_sha256','observation_sha256'))
      or set(status)!=expected or not isinstance(process,dict)
      or set(process)!={'pid','start','argv_sha256','cgroup_sha256','request_sha256'}
      or type(process.get('pid')) is not int or process['pid']<=1
      or not isinstance(process.get('start'),str) or not process['start'].isdigit()
      or any(not re.fullmatch(r'[0-9a-f]{64}',process.get(name,'')) for name in
             ('argv_sha256','cgroup_sha256','request_sha256'))):result('active')
  if state=='succeeded' and (not re.fullmatch(r'[0-9a-f]{64}',status.get('creation_sha256',''))
      or type(status.get('log_bytes')) is not int or not 0<=status['log_bytes']<=256*1024
      or type(status.get('log_truncated')) is not bool):result('active')
  return 'adoptable',None,None,None
 log_bytes=status.get('log_bytes')
 if state!='failed' or type(log_bytes) is not int or not 0<=log_bytes<=256*1024:result('active')
 if log_bytes:
  log_path=path/'log.txt'; log_raw=read_private(log_path,256*1024)
  if len(log_raw)!=log_bytes:result('active')
  log_sha256=hashlib.sha256(log_raw).hexdigest()
 else:log_sha256=hashlib.sha256(b'').hexdigest()
 return 'retryable-failed',binding,log_sha256,log_bytes
job_state=None
job_binding=None; job_log_sha256=None; job_log_bytes=None
backup_job=work/'disk-output/.creator-job.failed-backup'
if job.exists() or job.is_symlink():
 if backup_job.exists() or backup_job.is_symlink():result('active')
 job_state,job_binding,job_log_sha256,job_log_bytes=exact_job(job)
elif backup_job.exists() or backup_job.is_symlink():
 failed_state,job_binding,job_log_sha256,job_log_bytes=exact_job(backup_job)
 if failed_state!='retryable-failed':result('active')
 job_state='retryable-cleanup'
raw=read_private(receipt,4096)
if raw is None:
 if job_state is not None: result('invalid')
 result('missing')
try:document=json.loads(raw)
except BaseException:result('invalid')
if (set(document)!={'schema','purpose','helpers','public','installer'}
    or document.get('schema')!=1
    or document.get('purpose')!='tilo-supervised-creator-inputs'
    or any(not isinstance(document.get(name),str) or not re.fullmatch(r'[0-9a-f]{64}',document[name])
           for name in ('helpers','public','installer'))): result('invalid')
receipt_sha256=hashlib.sha256(raw).hexdigest()
if job_state=='adoptable':result('adoptable',receipt=document,receipt_sha256=receipt_sha256)
marker_value=None
marker_raw=read_private(marker,4096)
if marker_raw is not None:
 try:marker_value=json.loads(marker_raw)
 except BaseException:result('invalid')
 if (set(marker_value)!= {'schema','purpose','old_receipt_sha256','changed','helpers','public','installer'}
     or marker_value.get('schema')!=1
     or marker_value.get('purpose')!='tilo-supervised-creator-input-reconciliation'
     or not re.fullmatch(r'[0-9a-f]{64}',marker_value.get('old_receipt_sha256',''))
     or not isinstance(marker_value.get('changed'),str)
     or not marker_value['changed']):result('invalid')
 changed=marker_value['changed'].split(',')
 if len(set(changed))!=len(changed) or any(name not in names for name in changed):result('invalid')
 if any(not re.fullmatch(r'[0-9a-f]{64}',marker_value.get(name,'')) for name in names):result('invalid')
 for name in changed:
  path=work/names[name]; backup=work/('.creator-inputs-backup.'+names[name])
  if backup.exists() or backup.is_symlink():
   try:backup_info=backup.lstat()
   except OSError:result('invalid')
   if (not stat.S_ISDIR(backup_info.st_mode) or backup_info.st_uid or backup_info.st_gid
       or backup_info.st_mode&0o077):result('invalid')
  if not path.exists() and not backup.exists():result('invalid')
if marker_value is not None and receipt_sha256!=marker_value['old_receipt_sha256']:
 for name in marker_value['changed'].split(','):
  if not (work/names[name]).exists():result('invalid')
 if any(document[name]!=marker_value[name] for name in names):result('invalid')
 result('reconcile-complete',receipt=document,receipt_sha256=receipt_sha256,marker=marker_value)
for name in names:
 path=work/names[name]
 try:info=path.lstat()
 except OSError:
  if marker_value is None or name not in marker_value['changed']:result('invalid')
  continue
 if (not stat.S_ISDIR(info.st_mode) or info.st_uid or info.st_gid or
     info.st_mode&0o077):result('invalid')
result(job_state or ('reconcile' if marker_value is not None else 'ready'),receipt=document,
       receipt_sha256=receipt_sha256,**({'marker':marker_value} if marker_value is not None else {}),
       **({'job_binding':job_binding,'job_log_sha256':job_log_sha256,
           'job_log_bytes':job_log_bytes} if job_state in ('retryable-failed','retryable-cleanup') else {}))'''


def validate_creator_job_failure_receipt(path, status):
    path=Path(path)
    try:info=path.lstat()
    except OSError as error:
        raise CreatorInputReconciliationError('creator job failure receipt is missing') from error
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink!=1 or info.st_mode&0o077
            or info.st_size<2 or info.st_size>1024*1024):
        raise CreatorInputReconciliationError('creator job failure receipt is not private')
    try:document=json.loads(path.read_text())
    except (OSError,ValueError) as error:
        raise CreatorInputReconciliationError('creator job failure receipt is invalid') from error
    if (not isinstance(document,dict) or document.get('schema')!=1
            or document.get('purpose')!='tilo-supervised-creator-job-failure-log'
            or document.get('binding')!=status['job_binding']):
        raise CreatorInputReconciliationError('creator job failure receipt binding differs')
    try:
        encoded=base64.b64decode(document['log_base64'],validate=True)
    except (KeyError,TypeError,ValueError) as error:
        raise CreatorInputReconciliationError('creator job failure log encoding is invalid') from error
    expected_sha=hashlib.sha256(encoded).hexdigest()
    if len(encoded)!=status['job_log_bytes'] or expected_sha!=status['job_log_sha256']:
        raise CreatorInputReconciliationError('creator job failure log digest differs')
    return document


def creator_input_status(context, disk, acceptance_id='run1', operation_dir=None):
    """Read the exact private receipt and reject unsafe pending state."""
    try:
        value=json.loads(command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk',
                                         '--','python3','-c',REMOTE_CREATOR_INPUT_STATUS,
                                         disk.get('pod_uid',''),disk.get('node_uid',''),
                                         acceptance_id),timeout=30))
    except (ValueError,TypeError,json.JSONDecodeError) as error:
        raise CreatorInputReconciliationError('creator input status is not valid JSON') from error
    state=value.get('state') if isinstance(value,dict) else None
    if state=='missing':
        return None
    if state=='active':
        raise CreatorInputReconciliationError('cannot reconcile creator inputs while creator job is active')
    retry_fields={'state','receipt','receipt_sha256','job_binding','job_log_sha256','job_log_bytes'}
    if state not in ('ready','reconcile','reconcile-complete','retryable-failed','retryable-cleanup',
                     'adoptable') \
            or set(value) != (retry_fields if state in ('retryable-failed','retryable-cleanup') else
                               {'state','receipt','receipt_sha256'}
                               if state in ('ready','adoptable') else
                               {'state','receipt','receipt_sha256','marker'}):
        raise CreatorInputReconciliationError('creator input receipt or component state is invalid')
    receipt=value['receipt']
    marker=value.get('marker')
    if (not isinstance(receipt,dict) or set(receipt)!={'schema','purpose','helpers','public','installer'}
            or any(not isinstance(receipt.get(name),str) or
                   not re.fullmatch(r'[0-9a-f]{64}',receipt[name])
                   for name in ('helpers','public','installer'))
            or receipt['schema']!=1 or receipt['purpose']!='tilo-supervised-creator-inputs'
            or not re.fullmatch(r'[0-9a-f]{64}',value['receipt_sha256'])
            or (state in ('reconcile','reconcile-complete') and (not isinstance(marker,dict)
                or set(marker)!={'schema','purpose','old_receipt_sha256','changed',
                                 'helpers','public','installer'}
                or marker['schema']!=1
                or marker['purpose']!='tilo-supervised-creator-input-reconciliation'
                or not re.fullmatch(r'[0-9a-f]{64}',marker['old_receipt_sha256'])
                or not isinstance(marker['changed'],str) or not marker['changed']))):
        raise CreatorInputReconciliationError('creator input receipt or component state is invalid')
    if state in ('retryable-failed','retryable-cleanup'):
        if (not isinstance(value['job_binding'],dict)
                or not re.fullmatch(r'[0-9a-f]{64}',value['job_log_sha256'])
                or type(value['job_log_bytes']) is not int or not 0<=value['job_log_bytes']<=256*1024):
            raise CreatorInputReconciliationError('creator job failure status is invalid')
        if value['job_log_bytes']:
            if operation_dir is None:
                raise CreatorInputReconciliationError('creator job failure receipt is unavailable')
            validate_creator_job_failure_receipt(Path(operation_dir)/'creator-job.failure.json',value)
    return value


REMOTE_CREATOR_FAILED_JOB_RESET = r'''import hashlib,json,os,pathlib,shutil,stat,sys
work=pathlib.Path('/work'); output=work/'disk-output'; job=output/'.creator-job'; backup=output/'.creator-job.failed-backup'
expected_pod_uid=sys.argv[1]; expected_node_uid=sys.argv[2]; expected_operation=sys.argv[3]
expected_log_sha256=sys.argv[4]
def read(path):
 i=path.lstat()
 if (not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077
     or i.st_size<2 or i.st_size>65536):raise ValueError('failed creator status is invalid')
 fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  value=os.read(fd,65537)
 finally:os.close(fd)
 if len(value)!=i.st_size:raise ValueError('failed creator status changed')
 return json.loads(value)
def validate(path):
 if path.is_symlink() or not path.is_dir():raise ValueError('failed creator job is not a private directory')
 i=path.lstat()
 if i.st_uid or i.st_gid or stat.S_IMODE(i.st_mode)!=0o700:raise ValueError('failed creator job permissions differ')
 value=read(path/'status.json'); binding=value.get('binding')
 log_bytes=value.get('log_bytes')
 if (value.get('schema')!=1 or value.get('purpose')!='tilo-supervised-creator-job'
     or value.get('state')!='failed' or type(log_bytes) is not int or not 0<=log_bytes<=256*1024
     or not isinstance(binding,dict) or binding.get('operation')!=expected_operation
     or binding.get('pod_uid')!=expected_pod_uid or binding.get('node_uid')!=expected_node_uid
     or binding.get('output')!='/work/disk-output/disk.raw'):
  raise ValueError('creator job is not an exact retryable failure')
 if log_bytes:
  log_path=path/'log.txt'; log_info=log_path.lstat()
  if (not stat.S_ISREG(log_info.st_mode) or log_info.st_uid or log_info.st_gid
      or log_info.st_nlink!=1 or log_info.st_mode&0o077 or log_info.st_size!=log_bytes):
   raise ValueError('creator failure log is invalid')
  if hashlib.sha256(log_path.read_bytes()).hexdigest()!=expected_log_sha256:
   raise ValueError('creator failure log differs')
 elif expected_log_sha256!=hashlib.sha256(b'').hexdigest():
  raise ValueError('empty creator failure log digest differs')
if not output.is_dir() or output.is_symlink() or output.stat().st_uid or output.stat().st_gid \
   or stat.S_IMODE(output.stat().st_mode)!=0o700:raise ValueError('creator output directory is invalid')
for child in output.iterdir():
 if child.name not in (job.name,backup.name):raise ValueError('creator output is not retry-empty')
if job.exists() or job.is_symlink():
 if backup.exists() or backup.is_symlink():raise ValueError('creator failed-job quarantine is ambiguous')
 validate(job); os.rename(job,backup)
elif not (backup.exists() or backup.is_symlink()):
 raise ValueError('creator failed-job quarantine is absent')
validate(backup)
shutil.rmtree(backup)
if job.exists() or backup.exists():raise ValueError('creator failed-job quarantine remained')
print('TILO_CREATOR_FAILED_JOB_RESET')'''


def reset_failed_creator_job(context, disk, acceptance_id, status):
    argv=kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk','--','python3','-c',
                 REMOTE_CREATOR_FAILED_JOB_RESET,disk['pod_uid'],disk['node_uid'],acceptance_id,
                 status['job_log_sha256'])
    try:
        output=command(argv,timeout=30)
    except ControllerTransportError:
        raise
    except ValueError as error:
        raise CreatorInputReconciliationError('failed creator job could not be reset safely') from error
    if output.strip()!='TILO_CREATOR_FAILED_JOB_RESET':
        raise CreatorInputReconciliationError('failed creator job reset receipt is invalid')


def stream_creator_inputs(context, namespace, pod, container, helpers, public,
                          installer_bootc):
    """Stage all non-secret creator inputs in one fail-closed round trip."""
    archive, identities = creator_input_archive(helpers, public, installer_bootc)
    script = r'''set -euo pipefail
umask 077
test ! -e /work/disk-output/.creator-job
for destination in /work/helpers /work/public-trust /work/installer-bootc-helper; do
  if test -e "$destination" || test -L "$destination"; then
    test ! -L "$destination"
    test "$(stat -c '%u:%g:%a:%F' "$destination")" = '0:0:700:directory'
    rm -rf -- "$destination"
  fi
done
if test -e /work/.creator-inputs.json || test -L /work/.creator-inputs.json; then
  test ! -L /work/.creator-inputs.json
  test "$(stat -c '%u:%g:%a:%F' /work/.creator-inputs.json)" = '0:0:600:regular file'
  rm -f -- /work/.creator-inputs.json
fi
stage=$(mktemp -d /work/.creator-inputs.XXXXXX)
own_helpers=0; own_public=0; own_installer=0
cleanup() {
  rm -rf -- "$stage"
  test "$own_helpers" = 0 || rm -rf -- /work/helpers
  test "$own_public" = 0 || rm -rf -- /work/public-trust
  test "$own_installer" = 0 || rm -rf -- /work/installer-bootc-helper
}
trap cleanup EXIT HUP INT TERM
tar --no-same-owner -C "$stage" -xf -
test "$(find "$stage" -mindepth 1 -maxdepth 1 -type f -printf x | wc -c)" = 3
test "$(find "$stage" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" = ''
printf '%s  %s\n' "$1" "$stage/helpers.tar" | sha256sum -c - >/dev/null
printf '%s  %s\n' "$2" "$stage/public-trust.tar" | sha256sum -c - >/dev/null
printf '%s  %s\n' "$3" "$stage/installer-bootc-helper.tar" | sha256sum -c - >/dev/null
mkdir -m 0700 "$stage/helpers" "$stage/public-trust" "$stage/installer-bootc-helper"
tar --no-same-owner -C "$stage/helpers" -xf "$stage/helpers.tar"
tar --no-same-owner -C "$stage/public-trust" -xf "$stage/public-trust.tar"
tar --no-same-owner -C "$stage/installer-bootc-helper" -xf "$stage/installer-bootc-helper.tar"
test -n "$(find "$stage/helpers" -mindepth 1 -print -quit)"
test -n "$(find "$stage/public-trust" -mindepth 1 -print -quit)"
test -n "$(find "$stage/installer-bootc-helper" -mindepth 1 -print -quit)"
mv -T -n "$stage/helpers" /work/helpers
test ! -e "$stage/helpers"; own_helpers=1
mv -T -n "$stage/public-trust" /work/public-trust
test ! -e "$stage/public-trust"; own_public=1
mv -T -n "$stage/installer-bootc-helper" /work/installer-bootc-helper
test ! -e "$stage/installer-bootc-helper"; own_installer=1
receipt=$(mktemp /work/.creator-inputs.XXXXXX)
printf '{"schema":1,"purpose":"tilo-supervised-creator-inputs","helpers":"%s","public":"%s","installer":"%s"}\n' "$1" "$2" "$3" > "$receipt"
chmod 0600 "$receipt"
mv -T "$receipt" /work/.creator-inputs.json
own_helpers=0; own_public=0; own_installer=0
'''
    names = ('helpers.tar', 'public-trust.tar', 'installer-bootc-helper.tar')
    command(kubectl(context, '-n', namespace, 'exec', '-i', pod, '-c', container,
                    '--', 'sh', '-ceu', script, 'sh', *(identities[name] for name in names)),
            archive, timeout=300, text=False)


def stream_creator_input_delta(context, namespace, pod, container, helpers, public,
                               installer_bootc, status):
    """Replace only changed pending inputs, with a final atomic receipt commit."""
    components=creator_input_components(helpers,public,installer_bootc)
    identities={name:hashlib.sha256(content).hexdigest()
                for name,(_,content) in components.items()}
    state=status.get('state','ready')
    if state=='reconcile-complete':
        cleanup=r'''set -euo pipefail
test ! -e /work/disk-output/.creator-job
test "$(sha256sum /work/.creator-inputs.json | cut -d' ' -f1)" = "$1"
for backup in /work/.creator-inputs-backup.*; do
  if test -e "$backup" || test -L "$backup"; then rm -rf -- "$backup"; fi
done
rm -f -- /work/.creator-inputs-reconcile.json
test ! -e /work/.creator-inputs-reconcile.json
'''
        command(kubectl(context,'-n',namespace,'exec',pod,'-c',container,'--','sh','-ceu',cleanup,
                        'sh',status['receipt_sha256']),timeout=30)
        return True
    forced=tuple(status.get('marker',{}).get('changed','').split(',')) \
        if state=='reconcile' else ()
    changed=tuple(dict.fromkeys(forced+tuple(name for name in components
                                              if status['receipt'][name]!=identities[name])))
    if not changed:
        return False
    archive,_=creator_input_archive(helpers,public,installer_bootc,changed)
    script=r'''set -euo pipefail
umask 077
test ! -e /work/disk-output/.creator-job
test -f /work/.creator-inputs.json
test "$(stat -c '%u:%g:%a:%F' /work/.creator-inputs.json)" = '0:0:600:regular file'
test "$(sha256sum /work/.creator-inputs.json | cut -d' ' -f1)" = "$1"
old_receipt=$1; helpers_hash=$2; public_hash=$3; installer_hash=$4; changed_count=$5
shift 5
test "$changed_count" -ge 1 -a "$changed_count" -le 3
stage=$(mktemp -d /work/.creator-inputs.XXXXXX)
marker=/work/.creator-inputs-reconcile.json
committed=0
rollback(){
  if test "$committed" = 0; then
    for target in helpers public-trust installer-bootc-helper; do
      backup=/work/.creator-inputs-backup.$target
      if test -e "$backup" || test -L "$backup"; then
        if test -e "/work/$target" || test -L "/work/$target"; then rm -rf -- "/work/$target"; fi
        mv -T "$backup" "/work/$target"
      fi
    done
    rm -f -- "$marker"
  fi
  rm -rf -- "$stage"
}
trap rollback EXIT HUP INT TERM
tar --no-same-owner -C "$stage" -xf -
test "$(find "$stage" -mindepth 1 -maxdepth 1 -type f -printf x | wc -c)" = "$changed_count"
test "$(find "$stage" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" = ''
seen=0
changed_csv=
for component in "$@"; do
  case "$component" in
    helpers) archive=helpers.tar; target=helpers; expected=$helpers_hash;;
    public) archive=public-trust.tar; target=public-trust; expected=$public_hash;;
    installer) archive=installer-bootc-helper.tar; target=installer-bootc-helper; expected=$installer_hash;;
    *) exit 24;;
  esac
  test -f "$stage/$archive"
  printf '%s  %s\n' "$expected" "$stage/$archive" | sha256sum -c - >/dev/null
  mkdir -m 0700 "$stage/$target"
  tar --no-same-owner -C "$stage/$target" -xf "$stage/$archive"
  test -n "$(find "$stage/$target" -mindepth 1 -print -quit)"
  if test -n "$changed_csv"; then changed_csv="$changed_csv,$component"; else changed_csv="$component"; fi
  seen=$((seen+1))
done
test "$seen" = "$changed_count"
for target in helpers public-trust installer-bootc-helper; do
  backup=/work/.creator-inputs-backup.$target
  if test -e "$backup" || test -L "$backup"; then
    test ! -L "$backup"
    if test -e "/work/$target" || test -L "/work/$target"; then rm -rf -- "$backup"; else mv -T "$backup" "/work/$target"; fi
  fi
done
marker_tmp=$(mktemp /work/.creator-inputs-reconcile.XXXXXX)
printf '{"schema":1,"purpose":"tilo-supervised-creator-input-reconciliation","old_receipt_sha256":"%s","changed":"%s","helpers":"%s","public":"%s","installer":"%s"}\n' "$old_receipt" "$changed_csv" "$helpers_hash" "$public_hash" "$installer_hash" > "$marker_tmp"
chmod 0600 "$marker_tmp"
mv -T "$marker_tmp" "$marker"
for component in "$@"; do
  case "$component" in
    helpers) target=helpers;; public) target=public-trust;; installer) target=installer-bootc-helper;; *) exit 24;;
  esac
  backup=/work/.creator-inputs-backup.$target
  test ! -e "$backup" && test ! -L "$backup"
  test -d "/work/$target" && test ! -L "/work/$target"
  test "$(stat -c '%u:%g:%a:%F' "/work/$target")" = '0:0:700:directory'
  mv -T "/work/$target" "$backup"
  mv -T "$stage/$target" "/work/$target"
  test ! -e "$stage/$target"
done
test "$seen" = "$changed_count"
receipt=$(mktemp /work/.creator-inputs.XXXXXX)
printf '{"schema":1,"purpose":"tilo-supervised-creator-inputs","helpers":"%s","public":"%s","installer":"%s"}\n' "$helpers_hash" "$public_hash" "$installer_hash" > "$receipt"
chmod 0600 "$receipt"
mv -T "$receipt" /work/.creator-inputs.json
committed=1
for target in helpers public-trust installer-bootc-helper; do
  backup=/work/.creator-inputs-backup.$target
  if test -e "$backup" || test -L "$backup"; then rm -rf -- "$backup"; fi
done
rm -f -- "$marker"
'''
    names=tuple(components[name][0] for name in changed)
    hashes=(identities['helpers'],identities['public'],identities['installer'])
    command(kubectl(context,'-n',namespace,'exec','-i',pod,'-c',container,'--','sh','-ceu',script,
                    'sh',status['receipt_sha256'],*hashes,str(len(changed)),*changed),
            archive,timeout=300,text=False)
    return True


def creator_inputs_ready(context, disk, helpers, public, installer_bootc):
    expected={'schema':1,'purpose':'tilo-supervised-creator-inputs',
              'helpers':hashlib.sha256(helpers).hexdigest(),
              'public':hashlib.sha256(public).hexdigest(),
              'installer':hashlib.sha256(installer_bootc).hexdigest()}
    script=r'''import json,os,stat
p='/work/.creator-inputs.json';i=os.lstat(p)
if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or not 2<=i.st_size<=4096:raise SystemExit(2)
fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
try:
 o=os.fstat(fd)
 if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):raise SystemExit(2)
 print(os.read(fd,4097).decode())
finally:os.close(fd)'''
    try:
        value=json.loads(command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk',
                                         '--','python3','-c',script),timeout=30))
    except ControllerTransportError:
        raise
    except (ValueError,json.JSONDecodeError):
        return False
    return value==expected


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


def installer_bootc_archive(root, expected_sha256):
    root = Path(root).resolve()
    if not root.is_dir() or {path.name for path in root.iterdir()} != {'bootc', 'receipt.json'}:
        raise ValueError('installer bootc artifact must contain exactly bootc and receipt.json')
    binary, receipt_path = root / 'bootc', root / 'receipt.json'
    info, receipt_info = binary.lstat(), receipt_path.lstat()
    if not stat.S_ISREG(receipt_info.st_mode) or receipt_info.st_mode & 0o022:
        raise ValueError('installer bootc receipt is not a protected regular file')
    receipt = json.loads(receipt_path.read_text())
    if (not stat.S_ISREG(info.st_mode) or info.st_mode & 0o022
            or not info.st_mode & stat.S_IXUSR
            or installer_bootc_helper.elf_architecture(binary) != 'aarch64'
            or installer_bootc_helper.elf_build_id(binary)
               != receipt.get('binary_build_id')
            or installer_bootc_helper.sha256_file(binary) != expected_sha256):
        raise ValueError('installer bootc artifact differs from admitted native binary')
    expected = {'schema': 1, 'purpose': installer_bootc_helper.PURPOSE,
                'architecture': 'aarch64',
                'bootc_version': installer_bootc_helper.BOOTC_VERSION,
                'source_tag': installer_bootc_helper.SOURCE_TAG,
                'source_commit': installer_bootc_helper.SOURCE_COMMIT,
                'source_cargo_lock_sha256': installer_bootc_helper.SOURCE_CARGO_LOCK_SHA256,
                'patch_sha256': installer_bootc_helper.PATCH_SHA256,
                'rust_toolchain': installer_bootc_helper.RUST_TOOLCHAIN,
                'build_image': installer_bootc_helper.BUILD_IMAGE,
                'binary_sha256': expected_sha256,
                'build_target': installer_bootc_helper.TARGETS['aarch64'],
                'build_command': installer_bootc_helper.BUILD_COMMAND}
    if (set(receipt) != installer_bootc_helper.RECEIPT_KEYS
            or any(receipt.get(key) != value for key, value in expected.items())):
        raise ValueError('installer bootc artifact receipt differs from admitted provenance')
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode='w') as archive:
        archive.add(binary, arcname='bootc', recursive=False)
        archive.add(receipt_path, arcname='receipt.json', recursive=False)
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


def remove_legacy_registry_auth(operation_dir):
    """Erase only the old controller-side credential file before any resume."""
    path=Path(operation_dir)/'registry-auth.json'
    if not path.exists() and not path.is_symlink():
        return
    info=path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink!=1 or info.st_uid!=os.geteuid()
            or info.st_mode&0o077):
        raise ValueError('legacy registry auth residue is not a private regular file')
    path.unlink()


def validate_disk_args(value):
    required = {'source','target','architecture','revision','manifest_sha256','composefs_digest',
      'systemd_version','public_bundle','output','generation','size_bytes','offline_key_signature',
      'disposable_guest','boot_profile','source_live_uki','installer_bootc_directory',
      'installer_bootc_sha256'}
    if set(value) != required:
        raise ValueError('disk arguments contain missing or additional fields')
    if (not digest_reference(value['source']) or value['architecture'] != 'aarch64'
            or value['public_bundle'] != '/work/public-trust'
            or value['output'] != '/work/disk-output/disk.raw'
            or value['installer_bootc_directory'] != '/work/installer-bootc-helper'
            or not re.fullmatch(r'[0-9a-f]{64}', value['installer_bootc_sha256'])
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


def validate_creator_failure(document):
    required = {'schema','purpose','stage','argv_sha256','returncode',
                'stdout_base64','stderr_base64','stdout_truncated','stderr_truncated'}
    if (set(document) != required or document['schema'] != 1
            or document['purpose'] != 'tilo-supervised-bootc-install-failure'
            or document['stage'] != 'bootc-install-to-filesystem'
            or not re.fullmatch(r'[0-9a-f]{64}', document['argv_sha256'])
            or type(document['returncode']) is not int or document['returncode'] == 0
            or type(document['stdout_truncated']) is not bool
            or type(document['stderr_truncated']) is not bool):
        raise ValueError('creator failure evidence has invalid metadata')
    for name in ('stdout_base64', 'stderr_base64'):
        try: value = base64.b64decode(document[name], validate=True)
        except (ValueError, TypeError) as error:
            raise ValueError('creator failure evidence has invalid stream encoding') from error
        if len(value) > 256 * 1024:
            raise ValueError('creator failure evidence stream exceeds bound')


CONTROLLER_STDERR_LIMIT = 64 * 1024


def sanitize_controller_stderr(value):
    """Keep useful controller diagnostics without persisting credentials."""
    text = value if isinstance(value, str) else value.decode(errors='replace')
    text = re.sub(
        r'(?i)(authorization|bearer|token|password|passwd|secret|credential|'
        r'dockerconfigjson)(?:\s*[:=]\s*|\s+)(?:bearer\s+)?[^\s,;]+',
        r'\1=[REDACTED]', text)
    text = ''.join(character if character in '\t\n\r' or ord(character) >= 32
                   else '\ufffd' for character in text)
    encoded = text.encode('utf-8', errors='replace')
    return encoded[:CONTROLLER_STDERR_LIMIT], len(encoded) > CONTROLLER_STDERR_LIMIT


def validate_creator_controller_failure(document):
    required = {'schema', 'purpose', 'acceptance_id', 'creator', 'returncode',
                'stderr_sha256', 'stderr_base64', 'stderr_truncated'}
    if (set(document) != required or document['schema'] != 1
            or document['purpose'] != 'tilo-supervised-creator-controller-failure'
            or not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}', document['acceptance_id'])
            or type(document['creator']) is not dict
            or type(document['returncode']) is not int or document['returncode'] == 0
            or not re.fullmatch(r'[0-9a-f]{64}', document['stderr_sha256'])
            or type(document['stderr_truncated']) is not bool):
        raise ValueError('creator controller failure has invalid metadata')
    try:
        stderr = base64.b64decode(document['stderr_base64'], validate=True)
    except (ValueError, TypeError) as error:
        raise ValueError('creator controller failure has invalid stderr encoding') from error
    if len(stderr) > CONTROLLER_STDERR_LIMIT:
        raise ValueError('creator controller failure stderr exceeds bound')


def write_creator_controller_failure(args, disk, returncode, stderr):
    """Persist bounded diagnostics so a generic pre-job failure is resumable."""
    excerpt, truncated = sanitize_controller_stderr(stderr)
    document = {
        'schema': 1,
        'purpose': 'tilo-supervised-creator-controller-failure',
        'acceptance_id': args.acceptance_id,
        'creator': disk,
        'returncode': returncode,
        'stderr_sha256': hashlib.sha256(
            stderr if isinstance(stderr, bytes) else stderr.encode()).hexdigest(),
        'stderr_base64': base64.b64encode(excerpt).decode('ascii'),
        'stderr_truncated': truncated,
    }
    validate_creator_controller_failure(document)
    write_private(args.operation_dir/'creator-controller-failure.json',
                  (json.dumps(document, indent=2, sort_keys=True) + '\n').encode())


def collect_creator_failure(args, disk):
    remote = '/work/disk-output/bootc-install.failure.json'
    script = r'''import os,stat,sys
p=sys.argv[1]
try:i=os.lstat(p)
except FileNotFoundError:sys.exit(3)
if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or not 2<=i.st_size<=1024*1024:sys.exit(4)
fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
try:
 o=os.fstat(fd)
 if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size):sys.exit(4)
 d=os.read(fd,1024*1024+1)
 if len(d)!=i.st_size:sys.exit(4)
 sys.stdout.buffer.write(d)
finally:os.close(fd)
'''
    result = subprocess.run(kubectl(args.context, '-n', disk['namespace'], 'exec', disk['pod'],
        '-c', 'disk', '--', 'python3', '-c', script, remote), capture_output=True)
    if result.returncode == 3:
        return False
    if result.returncode:
        raise ValueError('failed to collect bounded creator failure evidence')
    document = json.loads(result.stdout)
    validate_creator_failure(document)
    destination = args.operation_dir / 'creator-install-failure.json'
    fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write((json.dumps(document, indent=2, sort_keys=True) + '\n').encode())
            output.flush(); os.fsync(output.fileno())
    except BaseException:
        try: destination.unlink()
        except FileNotFoundError: pass
        raise
    unlink = "import os,sys; p=sys.argv[1]; i=os.lstat(p); fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW); o=os.fstat(fd); os.close(fd); assert (i.st_dev,i.st_ino)==(o.st_dev,o.st_ino); os.unlink(p)"
    command(kubectl(args.context, '-n', disk['namespace'], 'exec', disk['pod'], '-c', 'disk',
                    '--', 'python3', '-c', unlink, remote), timeout=30)
    return True


def persist_creator_failure_log(args, stderr):
    marker = 'TILO_CREATOR_JOB_FAILURE_LOG '
    rows = [row.removeprefix(marker) for row in stderr.splitlines()
            if row.startswith(marker)]
    if not rows:
        return False
    if len(rows) != 1:
        raise ValueError('creator failure log marker is ambiguous')
    try:
        log = base64.b64decode(rows[0], validate=True)
    except ValueError as error:
        raise ValueError('creator failure log marker is not base64') from error
    if len(log) > 256 * 1024:
        raise ValueError('creator failure log exceeds its bound')
    receipt = json.loads((args.operation_dir/'creator-job.json').read_text())
    if (receipt.get('schema') != 1 or receipt.get('purpose') != 'tilo-supervised-creator-job-launch'
            or not isinstance(receipt.get('binding'), dict)):
        raise ValueError('creator failure log has no bound launch receipt')
    document = {'schema':1,'purpose':'tilo-supervised-creator-job-failure-log',
                'binding':receipt['binding'],'log_base64':base64.b64encode(log).decode()}
    write_private(args.operation_dir/'creator-job.failure.json',
                  (json.dumps(document,indent=2,sort_keys=True)+'\n').encode())
    return True


def controller(root, args, disk, agent, inspect_only, evidence_name='creator-environment.json',
               registry_auth_bytes=None, retry_failed=False):
    argv = ['python3', str(root/'os/dev/create-supervised-kata-disk.py'),
      '--context',args.context,'--namespace',disk['namespace'],'--pod',disk['pod'],
      '--pod-uid',disk['pod_uid'],'--node',disk['node'],'--node-uid',disk['node_uid'],
      '--host-agent',agent['metadata']['name'],'--guest-image',DISK_IMAGE,
      '--pins',str(args.host_pins),'--disk-args',str(args.disk_args),
      '--evidence',str(args.operation_dir/evidence_name),
      '--helper-revision',args.helper_revision,'--operation',args.acceptance_id]
    if inspect_only:
        argv.append('--inspect-only')
    else:
        argv += ['--creation-receipt',str(args.operation_dir/'creation.json'),
                 '--creator-job-receipt',str(args.operation_dir/'creator-job.json')]
        if retry_failed:
            argv.append('--retry-failed-creator-job')
        if registry_auth_bytes is not None:
            argv.append('--registry-auth-stdin')
    try:
        result=subprocess.run(argv,input=registry_auth_bytes,capture_output=True,
                              text=registry_auth_bytes is None,timeout=7200)
    except (OSError,subprocess.TimeoutExpired) as error:
        raise ControllerTransportError('creator controller process lost transport') from error
    if result.returncode:
        stderr=result.stderr if isinstance(result.stderr,str) else result.stderr.decode(errors='replace')
        if result.returncode==75 and stderr.strip().endswith('TILO_CREATOR_CONTROLLER_TRANSPORT'):
            raise ControllerTransportError('creator controller reported transport loss')
        if not inspect_only and persist_creator_failure_log(args, stderr):
            raise CreatorJobFailed('detached creator job failed')
        if not inspect_only and collect_creator_failure(args, disk):
            raise ValueError(f'creator controller failed (exit {result.returncode})')
        write_creator_controller_failure(args, disk, result.returncode, stderr)
        raise CreatorControllerFailure(f'creator controller failed before remote evidence (exit {result.returncode})')
    return result.stdout if isinstance(result.stdout,str) else result.stdout.decode()


def prepare_disk_output(context, disk):
    script = r'''set -eu
umask 077
test ! -L /work
test "$(stat -c '%u:%g:%F' /work)" = '0:0:directory'
test "$(stat -c '%d' /work)" != "$(stat -c '%d' /)"
chmod 0700 /work
test "$(stat -c '%u:%g:%a:%F' /work)" = '0:0:700:directory'
if test -e /work/disk-output || test -L /work/disk-output; then
    test ! -L /work/disk-output
    test "$(stat -c '%u:%g:%a:%F' /work/disk-output)" = '0:0:700:directory'
    test -z "$(find /work/disk-output -mindepth 1 -maxdepth 1 -print -quit)"
    rmdir -- /work/disk-output
fi
test ! -e /work/disk-output
test ! -L /work/disk-output
mkdir -m 0700 /work/disk-output
test "$(stat -c '%u:%g:%a:%F' /work/disk-output)" = '0:0:700:directory'
'''
    command(kubectl(context, '-n', disk['namespace'], 'exec', disk['pod'], '-c', 'disk',
                    '--', 'sh', '-ceu', script), timeout=30)


def create_on_worker(root, args, disk, agent, evidence_name='creator-environment.json',
                     registry_auth_bytes=None, retry_failed=False):
    controller(root, args, disk, agent, True, evidence_name)
    controller(root, args, disk, agent, False, evidence_name,
               registry_auth_bytes=registry_auth_bytes, retry_failed=retry_failed)


def verify_disk(context, disk, values, auth, operation=None):
    argv = ['python3','-B','/work/verifier/os/dev/verify-installed-development-disk.py']
    mapping = [('source','source'),('target','target'),('architecture','architecture'),
      ('revision','revision'),('manifest-sha256','manifest_sha256'),('generation','generation'),
      ('composefs-digest','composefs_digest'),('systemd-version','systemd_version')]
    for option, key in mapping: argv += ['--'+option,str(values[key])]
    argv += ['--public-bundle','/work/public-trust','--disk','/work/disk-output/disk.raw',
      '--creation-receipt','/work/disk-output/creation.json','--boot-profile','installer-media',
      '--copy-receipt','/work/disk-output/copy.json',
      '--source-live-uki',values['source_live_uki'],'--offline-key-signature']
    cleanup=r'''set -euo pipefail
umask 077
source="${TILO_DETACHED_SECRET_0:?}"; destination=/work/registry-auth.json
trap 'rm -f -- "$source" "$destination"' EXIT HUP INT TERM
test ! -e "$destination" && test ! -L "$destination"
install -m 0600 -- "$source" "$destination"
test -s "$destination"
set +e
"$@"
status=$?
exit "$status"
'''
    inputs={'registry-auth':auth,'verification-inputs':_canonical_json(values)}
    output=detached_job(context,disk,'disk','installed-verification',
      ['sh','-ceu',cleanup,'sh','/usr/bin/unshare','--mount','--propagation','private','--',*argv],
      operation=operation or 'verify-'+hashlib.sha256(disk['pod_uid'].encode()).hexdigest()[:12],
      inputs=inputs,cleanup_paths=('/work/registry-auth.json',),timeout=7200)
    return output.decode()


def verify_finalized_source(context, verifier, values, creation, operation=None):
    argv=['python3','-B','/work/verifier/os/dev/verify-finalized-development-image.py',
      '--helpers','/work/verifier','--image-id',creation['source']['image_id'],
      '--target',values['target'],'--public-bundle','/work/public-trust',
      '--manifest-sha256',values['manifest_sha256'],'--architecture','aarch64',
      '--generation',str(values['generation']),'--composefs-digest',values['composefs_digest'],
      '--systemd-version',values['systemd_version'],'--boot-profile','installer-media']
    document=json.loads(detached_job(context,verifier,'disk','finalized-source-verification',argv,
      operation=operation or 'finalized-'+hashlib.sha256(verifier['pod_uid'].encode()).hexdigest()[:12],
      inputs={'creation-receipt':_canonical_json(creation),'verification-inputs':_canonical_json(values)},
      timeout=7200))
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


def raw_identity(context, disk, container='disk', path='/work/disk-output/disk.raw', operation=None):
    script = "import hashlib,json,os,sys; p=sys.argv[1]; s=os.stat(p); h=hashlib.sha256(); f=open(p,'rb'); [h.update(c) for c in iter(lambda:f.read(8*1024*1024),b'')]; print(json.dumps({'size':s.st_size,'sha256':h.hexdigest()}))"
    if operation is None:
        value=json.loads(command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c',container,
          '--','python3','-c',script,path),timeout=7200))
    else:
        value=json.loads(detached_job(context,disk,container,'raw-identity',
          ['python3','-c',script,path],operation=operation,inputs={},timeout=7200).decode())
    if value.get('size') != 64*1024**3 or not re.fullmatch(r'[0-9a-f]{64}',value.get('sha256','')):
        raise ValueError('created raw media identity is invalid')
    return value


# A long kubectl exec couples a multi-hour hash or verifier to the controller's
# relay.  These receipts make the worker own that process; the controller only
# starts it and polls its durable state with short execs.
DETACHED_JOB_LOG_BYTES = 128 * 1024
DETACHED_JOB_OUTPUT_BYTES = 512 * 1024
DETACHED_JOB_POLL_SECONDS = 10


DETACHED_JOB_RUNNER = r'''import base64, hashlib, json, os, stat, subprocess, sys, tempfile, time
root = sys.argv[1]
contract_path = root + '/contract.json'
receipt_path = root + '/receipt.json'
state_path = root + '/state.json'
limit = int(sys.argv[2])
def private_regular(path):
 value=os.lstat(path)
 if (not stat.S_ISREG(value.st_mode) or stat.S_ISLNK(value.st_mode) or value.st_uid != 0 or value.st_gid != 0 or value.st_nlink != 1 or stat.S_IMODE(value.st_mode) != 0o600): raise RuntimeError('unsafe detached job file')
 return value
def atomic(path, value):
 fd, temporary=tempfile.mkstemp(prefix='.'+os.path.basename(path)+'-',dir=root)
 try:
  os.fchmod(fd,0o600)
  with os.fdopen(fd,'wb') as stream: stream.write(value); stream.flush(); os.fsync(stream.fileno())
  os.replace(temporary,path)
 finally:
  try: os.unlink(temporary)
  except FileNotFoundError: pass
contract=None; contract_bytes=b''; contract_sha256=''; started=time.time_ns(); stdout=b''; stderr=b''; returncode=125; error=None; secrets=[]
try:
 private_regular(contract_path)
 with open(contract_path,'rb') as stream: contract_bytes=stream.read()
 contract=json.loads(contract_bytes); contract_sha256=hashlib.sha256(contract_bytes).hexdigest()
 pid=os.getpid()
 start=open('/proc/'+str(pid)+'/stat').read().split()[21]
 cmd=hashlib.sha256(open('/proc/'+str(pid)+'/cmdline','rb').read()).hexdigest()
 cgroup=hashlib.sha256(open('/proc/'+str(pid)+'/cgroup','rb').read()).hexdigest()
 atomic(state_path,(json.dumps({'pid':pid,'start_time':start,'cmdline_sha256':cmd,'cgroup_sha256':cgroup},sort_keys=True)+'\n').encode())
 with open(sys.argv[0],'rb') as stream:
  if hashlib.sha256(stream.read()).hexdigest()!=contract['runner_sha256']: raise RuntimeError('detached runner differs from contract')
 for path in contract['secret_paths']:
  private_regular(path)
  with open(path,'rb') as stream: secrets.append(stream.read())
 environment=os.environ.copy()
 for index,path in enumerate(contract['secret_paths']): environment['TILO_DETACHED_SECRET_'+str(index)]=path
 completed=subprocess.run(contract['argv'],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False,env=environment)
 stdout,stderr,returncode=completed.stdout,completed.stderr,completed.returncode
except BaseException as caught:
 error=type(caught).__name__+': '+str(caught)
finally:
 cleaned=True
 for path in (contract or {}).get('secret_paths',[]):
  try:
   value=os.lstat(path)
   if stat.S_ISREG(value.st_mode) and not stat.S_ISLNK(value.st_mode): os.unlink(path)
   else: cleaned=False
  except FileNotFoundError: pass
  except OSError: cleaned=False
 for path in (contract or {}).get('cleanup_paths',[]):
  try:
   value=os.lstat(path)
   if stat.S_ISREG(value.st_mode) and not stat.S_ISLNK(value.st_mode) and value.st_uid == 0 and value.st_gid == 0 and value.st_nlink == 1 and stat.S_IMODE(value.st_mode) == 0o600: os.unlink(path)
   else: cleaned=False
  except FileNotFoundError: pass
  except OSError: cleaned=False
 for path in (contract or {}).get('cleanup_paths',[]):
  if os.path.lexists(path): cleaned=False
 for secret in secrets:
  if secret and (secret in stdout or secret in stderr): error='worker output contained a staged secret'; returncode=125; stdout=b''; stderr=b''
 out,out_truncated=stdout[:limit],len(stdout)>limit
 err,err_truncated=stderr[:limit],len(stderr)>limit
 receipt={'schema':1,'purpose':'tilo-supervised-detached-job','contract_sha256':contract_sha256,'operation':contract['operation'],'worker':contract['worker'],'container':contract['container'],'argv_sha256':contract['argv_sha256'],'runner_sha256':contract['runner_sha256'],'input_identities':contract['input_identities'],'output_sha256':hashlib.sha256(stdout).hexdigest(),'output_bytes':len(stdout),'stdout_base64':base64.b64encode(out).decode(),'stderr_base64':base64.b64encode(err).decode(),'stdout_truncated':out_truncated,'stderr_truncated':err_truncated,'returncode':returncode,'secrets_cleaned':cleaned,'started_at_ns':started,'finished_at_ns':time.time_ns(),'status':'succeeded' if returncode == 0 and error is None and cleaned else 'failed'}
 if error is not None: receipt['error']=error
 atomic(receipt_path,(json.dumps(receipt,sort_keys=True,separators=(',',':'))+'\n').encode())
 try: os.unlink(state_path)
 except FileNotFoundError: pass
'''


DETACHED_JOB_STATUS = r'''import hashlib, json, os, stat, sys, time
root=sys.argv[1]
def regular(path):
 try:
  value=os.lstat(path)
  return stat.S_ISREG(value.st_mode) and not stat.S_ISLNK(value.st_mode) and value.st_nlink==1 and value.st_size<=1048576
 except FileNotFoundError:return False
receipt=root+'/receipt.json'; state=root+'/state.json'; claim=root+'/claim'
if regular(receipt): print(json.dumps({'state':'finished','receipt':open(receipt).read()})); raise SystemExit
if regular(state):
 value=json.loads(open(state).read()); pid=value.get('pid')
 if set(value)!={'pid','start_time','cmdline_sha256','cgroup_sha256'} or type(pid) is not int or pid <= 1 or not isinstance(value['start_time'],str) or not isinstance(value['cmdline_sha256'],str) or not isinstance(value['cgroup_sha256'],str): raise SystemExit(2)
 try: os.kill(pid,0)
 except ProcessLookupError: print(json.dumps({'state':'dead'}))
 else:
  try:
   start=open('/proc/'+str(pid)+'/stat').read().split()[21]
   cmd=hashlib.sha256(open('/proc/'+str(pid)+'/cmdline','rb').read()).hexdigest()
   cgroup=hashlib.sha256(open('/proc/'+str(pid)+'/cgroup','rb').read()).hexdigest()
   print(json.dumps({'state':'running' if (start,cmd,cgroup)==(value['start_time'],value['cmdline_sha256'],value['cgroup_sha256']) else 'dead'}))
  except (FileNotFoundError,IndexError): print(json.dumps({'state':'dead'}))
 raise SystemExit
if os.path.isdir(claim): print(json.dumps({'state':'starting' if time.time()-os.stat(claim).st_mtime < 60 else 'stale'}))
else: print(json.dumps({'state':'absent'}))
'''


def _canonical_json(value):
    return json.dumps(value,sort_keys=True,separators=(',',':')).encode()


def _detached_job_contract(operation, worker, container, argv, inputs, root, cleanup_paths=()):
    required={'pod','pod_uid','namespace','node','node_uid','spec','image_id'}
    if (not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,64}',operation) or set(worker)!=required
            or not digest_reference(worker['image_id']) or not isinstance(argv,list) or not argv
            or not all(isinstance(row,str) and row for row in argv)):
        raise ValueError('invalid detached job contract')
    identities={}
    for name,value in inputs.items():
        if (not re.fullmatch(r'[a-z][a-z0-9-]{0,63}',name) or not isinstance(value,bytes)
                or len(value)>1024*1024): raise ValueError('invalid detached job input')
        identities[name]={'sha256':hashlib.sha256(value).hexdigest(),'size':len(value)}
    if (not isinstance(cleanup_paths,tuple) or len(set(cleanup_paths))!=len(cleanup_paths)
            or any(path!='/work/registry-auth.json' for path in cleanup_paths)):
        raise ValueError('invalid detached job cleanup path')
    binding={key:worker[key] for key in ('pod','pod_uid','namespace','node','node_uid','image_id')}
    paths=[root+'/private/'+name for name in sorted(inputs)]
    contract={'schema':1,'purpose':'tilo-supervised-detached-job','operation':operation,
      'worker':binding,'container':container,'argv':argv,
      'argv_sha256':hashlib.sha256(_canonical_json(argv)).hexdigest(),
      'runner_sha256':hashlib.sha256(DETACHED_JOB_RUNNER.encode()).hexdigest(),
      'input_identities':identities,'secret_paths':paths,'cleanup_paths':list(cleanup_paths)}
    return contract,_canonical_json(contract)+b'\n'


def _detached_job_root(operation, worker, container, argv, inputs, cleanup_paths=()):
    stable={'operation':operation,
      'worker':{key:worker[key] for key in ('pod','pod_uid','namespace','node','node_uid','image_id')},
      'container':container,'argv':argv,
      'inputs':{name:hashlib.sha256(value).hexdigest() for name,value in sorted(inputs.items())},
      'cleanup_paths':list(cleanup_paths),'runner_sha256':hashlib.sha256(DETACHED_JOB_RUNNER.encode()).hexdigest()}
    return '/work/.tilo-detached-jobs/'+hashlib.sha256(_canonical_json(stable)).hexdigest()


def _stage_detached_input(context, worker, container, path, content, identity):
    script = r'''set -euo pipefail
umask 077
path="$1"; expected="$2"; directory=${path%/*}
test "$directory" != "$path"
mkdir -p -m 0700 -- "$directory"
test ! -L "$directory"
if test -e "$path" || test -L "$path"; then
  test ! -L "$path"
  test "$(stat -c '%u:%g:%a:%F' "$path")" = '0:0:600:regular file'
  printf '%s  %s\n' "$expected" "$path" | sha256sum -c - >/dev/null
  exit 0
fi
temporary=$(mktemp "$directory/.input.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
cat > "$temporary"
chmod 0600 "$temporary"
printf '%s  %s\n' "$expected" "$temporary" | sha256sum -c - >/dev/null
mv -n -- "$temporary" "$path"
test ! -e "$temporary"
test ! -L "$path"
test "$(stat -c '%u:%g:%a:%F' "$path")" = '0:0:600:regular file'
printf '%s  %s\n' "$expected" "$path" | sha256sum -c - >/dev/null
'''
    command(kubectl(context,'-n',worker['namespace'],'exec','-i',worker['pod'],'-c',container,
      '--','sh','-ceu',script,'sh',path,identity),content,timeout=30,text=False)


def _detached_job_status(context, worker, container, root):
    output=command(kubectl(context,'-n',worker['namespace'],'exec',worker['pod'],'-c',container,
      '--','python3','-c',DETACHED_JOB_STATUS,root),timeout=30)
    try:value=json.loads(output)
    except (TypeError,json.JSONDecodeError) as error:
        raise ValueError('detached job status is invalid') from error
    if set(value)=={'state'} and value['state'] in {'absent','starting','running','dead','stale'}: return value
    if (set(value)=={'state','receipt'} and value['state']=='finished'
            and isinstance(value['receipt'],str)): return value
    raise ValueError('detached job status is invalid')


def _validate_detached_receipt(value, contract, contract_bytes):
    expected={'schema':1,'purpose':'tilo-supervised-detached-job',
      'contract_sha256':hashlib.sha256(contract_bytes).hexdigest(),'operation':contract['operation'],
      'worker':contract['worker'],'container':contract['container'],
      'argv_sha256':contract['argv_sha256'],'runner_sha256':contract['runner_sha256'],
      'input_identities':contract['input_identities']}
    permitted=set(expected)|{'output_sha256','output_bytes','stdout_base64','stderr_base64',
      'stdout_truncated','stderr_truncated','returncode','secrets_cleaned','started_at_ns',
      'finished_at_ns','status','error'}
    if (not isinstance(value,dict) or not set(value).issubset(permitted) or not set(expected).issubset(value)
            or any(value.get(key)!=item for key,item in expected.items())
            or value.get('status') not in {'succeeded','failed'} or type(value.get('returncode')) is not int
            or type(value.get('secrets_cleaned')) is not bool
            or type(value.get('output_bytes')) is not int or not 0<=value['output_bytes']<=DETACHED_JOB_OUTPUT_BYTES
            or not re.fullmatch(r'[0-9a-f]{64}',value.get('output_sha256',''))
            or type(value.get('stdout_truncated')) is not bool or type(value.get('stderr_truncated')) is not bool
            or type(value.get('started_at_ns')) is not int or type(value.get('finished_at_ns')) is not int
            or value['finished_at_ns']<value['started_at_ns']):
        raise ValueError('detached job receipt does not bind its requested execution')
    try:
        stdout=base64.b64decode(value['stdout_base64'],validate=True)
        stderr=base64.b64decode(value['stderr_base64'],validate=True)
    except (KeyError,TypeError,ValueError) as error:
        raise ValueError('detached job receipt has invalid bounded logs') from error
    if len(stdout)>DETACHED_JOB_LOG_BYTES or len(stderr)>DETACHED_JOB_LOG_BYTES:
        raise ValueError('detached job receipt log exceeds bound')
    if value['status']!='succeeded' or value['returncode']!=0 or not value['secrets_cleaned']:
        raise ValueError('detached job failed or did not clean private inputs')
    if (value['stdout_truncated'] or value['output_bytes']!=len(stdout)
            or hashlib.sha256(stdout).hexdigest()!=value['output_sha256']):
        raise ValueError('detached job output is incomplete or differs from its receipt')
    return stdout


def _launch_detached_job(context, worker, container, root):
    script=r'''set -euo pipefail
umask 077
root="$1"; runner="$root/runner.py"; contract="$root/contract.json"; claim="$root/claim"; log="$root/runner.log"
test ! -L "$root"; test -f "$runner"; test -f "$contract"
if ! mkdir -- "$claim" 2>/dev/null; then exit 0; fi
test ! -e "$log" && test ! -L "$log"
setsid python3 "$runner" "$root" 131072 </dev/null >"$log" 2>&1 &
'''
    command(kubectl(context,'-n',worker['namespace'],'exec',worker['pod'],'-c',container,
      '--','sh','-ceu',script,'sh',root),timeout=30)


def _reconcile_stale_detached_job(context, worker, container, root):
    script='test ! -L "$1"; test -d "$1/claim"; test ! -e "$1/state"; rm -rf -- "$1"'
    command(kubectl(context,'-n',worker['namespace'],'exec',worker['pod'],'-c',container,
      '--','sh','-ceu',script,'sh',root),timeout=30)


def detached_job(context, worker, container, purpose, argv, *, operation, inputs, timeout, cleanup_paths=(),
                 poll_seconds=DETACHED_JOB_POLL_SECONDS, sleeper=time.sleep):
    """Run a worker-owned process and adopt the bound receipt after reconnect."""
    if (not re.fullmatch(r'[a-z][a-z0-9-]{0,63}',purpose)
            or not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,64}',operation)
            or not isinstance(inputs,dict) or timeout<=0 or poll_seconds<1):
        raise ValueError('invalid detached job parameters')
    full_operation=operation+'-'+purpose
    root=_detached_job_root(full_operation,worker,container,argv,inputs,cleanup_paths)
    contract,contract_bytes=_detached_job_contract(full_operation,worker,container,argv,inputs,root,cleanup_paths)
    require_same_worker(context,worker)
    started=time.monotonic(); retry_delay=1
    while True:
        try:
            require_same_worker(context,worker)
            status=_detached_job_status(context,worker,container,root)
            retry_delay=1
        except ControllerTransportError:
            if time.monotonic()-started>=timeout: raise
            sleeper(retry_delay);retry_delay=min(retry_delay*2,10)
            continue
        if status['state']=='finished':
            try:receipt=json.loads(status['receipt'])
            except json.JSONDecodeError as error:raise ValueError('detached job receipt is invalid') from error
            require_same_worker(context,worker)
            return _validate_detached_receipt(receipt,contract,contract_bytes)
        if status['state']=='dead': raise ValueError('detached job died without a receipt')
        if status['state']=='stale':
            require_same_worker(context,worker)
            _reconcile_stale_detached_job(context,worker,container,root)
            continue
        if status['state']=='absent':
            try:
                require_same_worker(context,worker)
                stream_file(context,worker['namespace'],worker['pod'],container,root+'/contract.json',contract_bytes)
                require_same_worker(context,worker)
                stream_file(context,worker['namespace'],worker['pod'],container,root+'/runner.py',DETACHED_JOB_RUNNER.encode())
                for name in sorted(inputs):
                    require_same_worker(context,worker)
                    _stage_detached_input(context,worker,container,root+'/private/'+name,inputs[name],
                      contract['input_identities'][name]['sha256'])
                require_same_worker(context,worker)
                _launch_detached_job(context,worker,container,root)
            except ControllerTransportError:
                # The next short status probe adopts a started job, or safely
                # resumes only if the exact worker still reports it absent.
                continue
        if time.monotonic()-started>=timeout: raise ValueError('detached job receipt did not arrive before its deadline')
        sleeper(poll_seconds)


def remote_file_identity(context, worker, container='disk', path='/work/disk-output/disk.raw'):
    script = r'''import json,os,stat,sys
p=sys.argv[1]
i=os.lstat(p)
if (not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or stat.S_IMODE(i.st_mode) not in (0o400,0o600)
        or i.st_nlink!=1 or i.st_size<=0):
 raise SystemExit(2)
print(json.dumps({'device':i.st_dev,'inode':i.st_ino,'size':i.st_size}))
'''
    value=json.loads(command(kubectl(context,'-n',worker['namespace'],'exec',worker['pod'],
      '-c',container,'--','python3','-c',script,path),timeout=30))
    if (set(value)!={'device','inode','size'}
            or any(type(item) is not int or item<=0 for item in value.values())):
        raise ValueError('copied raw media file identity is invalid')
    return value


def transfer_receipt(creation_bytes, identity, destination_identity,
                     path='/work/disk-output/disk.raw'):
    creation=json.loads(creation_bytes)
    source_identity=creation.get('file_identity')
    if (creation.get('path')!=path or creation.get('disk_created') is not True
            or creation.get('size_bytes')!=identity.get('size')
            or not isinstance(source_identity,dict)
            or set(source_identity)!={'device','inode','size'}
            or source_identity.get('size')!=identity.get('size')
            or not re.fullmatch(r'[0-9a-f]{64}',identity.get('sha256',''))
            or set(destination_identity)!={'device','inode','size'}
            or destination_identity.get('size')!=identity.get('size')):
        raise ValueError('raw media transfer cannot bind the creation receipt')
    return {'schema':1,'purpose':'tilo-supervised-raw-media-copy',
      'source_creation_sha256':hashlib.sha256(creation_bytes).hexdigest(),
      'source_path':path,'source_file_identity':source_identity,
      'raw_identity':identity,'path':path,'file_identity':destination_identity}


def freeze_raw(context, disk):
    command(kubectl(context,'-n',disk['namespace'],'exec',disk['pod'],'-c','disk','--',
                    'chmod','0400','/work/disk-output/disk.raw'),timeout=30)


def prepare_verifier_devices(context, verifier):
    script = r'''import importlib.util,pathlib,sys
p=pathlib.Path('/work/verifier/os/dev/create-development-disk.py')
sys.path.insert(0,str(p.parent))
s=importlib.util.spec_from_file_location('tilo_verifier_devices',p)
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
m.ensure_loop_devices()
'''
    command(kubectl(context,'-n',verifier['namespace'],'exec',verifier['pod'],'-c','disk',
                    '--','python3','-c',script),timeout=30)


def preflight_vm(context, vm):
    firmware=' '.join(f'{digest}:{path}' for path,digest in FIRMWARE.items())
    tools=('qemu-img qemu-system-aarch64 tesseract sfdisk mkfs.vfat mkfs.ntfs mkfs.ext4 '
           'mattrib mcopy mdir mmd mtype ntfscp debugfs socat python3 sha256sum')
    script=r'''set -euo pipefail
umask 077
test "$(uname -m)" = aarch64
test -c /dev/kvm && test -r /dev/kvm && test -w /dev/kvm
for tool in $REQUIRED_TOOLS; do command -v "$tool" >/dev/null; done
for row in $FIRMWARE_IDENTITIES; do
  digest=${row%%:*}; path=${row#*:}
  printf '%s  %s\n' "$digest" "$path" | sha256sum -c - >/dev/null
done
printf '%s  %s\n' "$PROFILE_SHA256" /usr/local/bin/tilo-imager-profile | sha256sum -c - >/dev/null
test -x /usr/local/bin/tilo-imager-profile
bash -n /work/source/os/dev/_common.sh /work/source/os/dev/vm.sh /work/source/os/dev/test-installer-cycle.sh
test ! -e /work/vm-preflight
install -d -m 0700 /work/vm-preflight
truncate -s 1M /work/vm-preflight/sparse
test "$(stat -c '%u:%g:%a:%s' /work/vm-preflight/sparse)" = '0:0:600:1048576'
rm -rf /work/vm-preflight
'''
    argv=['env','REQUIRED_TOOLS='+tools,'FIRMWARE_IDENTITIES='+firmware,
          'PROFILE_SHA256='+PROFILE_ENCODER_SHA256,'bash','-ceu',script]
    command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',*argv),timeout=120)


DIRECT_RECEIVER_SCRIPT = r'''import hashlib,json,os,secrets,signal,socket,stat,struct,sys,time
directory,name,bind_ip,source_ip,operation,source_uid,target_uid,size_text,digest=sys.argv[1:]
size=int(size_text);deadline=time.monotonic()+7200;complete=False;final_created=False
if not secrets.compare_digest(digest.lower(),digest) or len(digest)!=64:raise SystemExit('invalid expected digest')
token=sys.stdin.readline().strip()
if len(token)!=64:raise SystemExit('invalid capability')
parent,name_dir=os.path.split(directory)
if not os.path.isabs(directory) or not name or '/' in name or not name_dir:raise SystemExit('invalid destination')
parent_fd=os.open(parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
dir_fd=None;temp_name='.incoming-'+secrets.token_hex(12);fd=None;listener=None;connection=None
def alarm(*_):raise TimeoutError('transfer deadline exceeded')
signal.signal(signal.SIGALRM,alarm);signal.alarm(7230)
def exact(sock,count):
 value=bytearray()
 while len(value)<count:
  block=sock.recv(count-len(value))
  if not block:raise EOFError('truncated transfer')
  value.extend(block)
 return bytes(value)
try:
 parent_info=os.fstat(parent_fd)
 if parent_info.st_uid or not stat.S_ISDIR(parent_info.st_mode) or stat.S_IMODE(parent_info.st_mode)&0o022:
  raise SystemExit('unsafe destination parent')
 try:os.mkdir(name_dir,0o700,dir_fd=parent_fd)
 except FileExistsError:pass
 dir_fd=os.open(name_dir,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent_fd)
 info=os.fstat(dir_fd)
 if info.st_uid or stat.S_IMODE(info.st_mode)!=0o700:raise SystemExit('unsafe destination directory')
 try:os.stat(name,dir_fd=dir_fd,follow_symlinks=False);raise SystemExit('destination already exists')
 except FileNotFoundError:pass
 fd=os.open(temp_name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=dir_fd)
 os.ftruncate(fd,size)
 listener=socket.socket();listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,0)
 listener.bind((bind_ip,0));listener.listen(1);listener.settimeout(30)
 print(json.dumps({'schema':1,'status':'ready','port':listener.getsockname()[1]}),flush=True)
 connection,peer=listener.accept();listener.close();listener=None
 if peer[0]!=source_ip:raise PermissionError('source IP rejected')
 connection.settimeout(120)
 header_size=struct.unpack('>I',exact(connection,4))[0]
 if not 1<=header_size<=4096:raise ValueError('invalid handshake size')
 handshake=json.loads(exact(connection,header_size))
 expected={'version':1,'operation':operation,'source_uid':source_uid,'target_uid':target_uid,
           'size':size,'sha256':digest,'capability':token}
 if (set(handshake)!=set(expected)
     or any(handshake[key]!=value for key,value in expected.items() if key!='capability')
     or not isinstance(handshake.get('capability'),str)
     or not secrets.compare_digest(handshake['capability'],token)):
  raise PermissionError('handshake rejected')
 token=None;handshake['capability']=None;position=0;frames=0;written=0
 while True:
  kind=exact(connection,1)
  if kind==b'E':break
  if kind!=b'D':raise ValueError('invalid extent frame')
  offset,length=struct.unpack('>QQ',exact(connection,16));frames+=1
  if frames>1048576 or length<=0 or offset<position or offset+length>size:
   raise ValueError('invalid extent bounds')
  position=offset+length;remaining=length;cursor=offset
  while remaining:
   block=exact(connection,min(1024*1024,remaining))
   count=os.pwrite(fd,block,cursor)
   if count!=len(block):raise OSError('short destination write')
   cursor+=count;remaining-=count;written+=count
 os.fsync(fd);os.close(fd);fd=None
 check=os.open(temp_name,os.O_RDONLY|os.O_NOFOLLOW,dir_fd=dir_fd);actual=hashlib.sha256()
 try:
  while True:
   block=os.read(check,8*1024*1024)
   if not block:break
   actual.update(block)
 finally:os.close(check)
 if actual.hexdigest()!=digest:raise ValueError('destination digest differs')
 mode_fd=os.open(temp_name,os.O_RDONLY|os.O_NOFOLLOW,dir_fd=dir_fd)
 try:os.fchmod(mode_fd,0o400);os.fsync(mode_fd)
 finally:os.close(mode_fd)
 os.link(temp_name,name,src_dir_fd=dir_fd,dst_dir_fd=dir_fd,follow_symlinks=False);final_created=True
 os.unlink(temp_name,dir_fd=dir_fd);os.fsync(dir_fd)
 receipt={'schema':1,'purpose':'tilo-direct-sparse-transfer','operation':operation,
          'source_pod_uid':source_uid,'target_pod_uid':target_uid,'size':size,
          'sha256':digest,'path':directory+'/'+name,'data_bytes':written,'extent_frames':frames}
 encoded=json.dumps(receipt,sort_keys=True,separators=(',',':')).encode()
 connection.sendall(struct.pack('>I',len(encoded))+encoded);connection.shutdown(socket.SHUT_WR)
 print(json.dumps(receipt,sort_keys=True),flush=True);complete=True
finally:
 signal.alarm(0);token=None
 for item in (connection,listener):
  if item is not None:
   try:item.close()
   except OSError:pass
 if fd is not None:os.close(fd)
 if dir_fd is not None:
  try:os.unlink(temp_name,dir_fd=dir_fd)
  except FileNotFoundError:pass
  if final_created and not complete:
   try:os.unlink(name,dir_fd=dir_fd)
   except FileNotFoundError:pass
  os.close(dir_fd)
 os.close(parent_fd)
'''


DIRECT_SENDER_SCRIPT = r'''import errno,json,os,signal,socket,stat,struct,sys
path,target_ip,port_text,operation,source_uid,target_uid,size_text,digest,source_ip=sys.argv[1:]
size=int(size_text);token=sys.stdin.readline().strip()
if len(token)!=64:raise SystemExit('invalid capability')
fd=os.open(path,os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW);sock=None
def alarm(*_):raise TimeoutError('transfer deadline exceeded')
signal.signal(signal.SIGALRM,alarm);signal.alarm(7200)
def send(value):sock.sendall(value)
def exact(count):
 value=bytearray()
 while len(value)<count:
  block=sock.recv(count-len(value))
  if not block:raise EOFError('truncated transfer acknowledgement')
  value.extend(block)
 return bytes(value)
try:
 info=os.fstat(fd)
 if not stat.S_ISREG(info.st_mode) or info.st_uid or info.st_size!=size or stat.S_IMODE(info.st_mode)!=0o400:
  raise SystemExit('source file admission failed')
 extents=[];position=0;unsupported={errno.EINVAL,errno.ENOSYS}
 if hasattr(errno,'ENOTSUP'):unsupported.add(errno.ENOTSUP)
 sparse=hasattr(os,'SEEK_DATA') and hasattr(os,'SEEK_HOLE')
 if sparse:
  try:
   while position<size:
    try:start=os.lseek(fd,position,os.SEEK_DATA)
    except OSError as error:
     if error.errno==errno.ENXIO:break
     raise
    end=min(os.lseek(fd,start,os.SEEK_HOLE),size)
    if end<=start or len(extents)>=1048576:raise SystemExit('invalid source extent')
    extents.append((start,end));position=end
  except OSError as error:
   if error.errno not in unsupported:raise
   extents=[(0,size)]
 else:extents=[(0,size)]
 handshake={'version':1,'operation':operation,'source_uid':source_uid,'target_uid':target_uid,
            'size':size,'sha256':digest,'capability':token}
 encoded=json.dumps(handshake,sort_keys=True,separators=(',',':')).encode();token=None
 sock=socket.socket();sock.settimeout(120);sock.bind((source_ip,0));sock.connect((target_ip,int(port_text)))
 send(struct.pack('>I',len(encoded))+encoded);encoded=b''
 for start,end in extents:
  if end<=start or end>size:raise SystemExit('invalid source extent')
  send(b'D'+struct.pack('>QQ',start,end-start));cursor=start
  while cursor<end:
   block=os.pread(fd,min(1024*1024,end-cursor),cursor)
   if not block:raise SystemExit('short source read')
   send(block);cursor+=len(block)
 send(b'E');sock.shutdown(socket.SHUT_WR)
 length=struct.unpack('>I',exact(4))[0]
 if not 1<=length<=4096:raise SystemExit('invalid transfer acknowledgement')
 body=bytearray()
 while len(body)<length:
  body.extend(exact(length-len(body)))
 receipt=json.loads(body)
 expected={'schema':1,'purpose':'tilo-direct-sparse-transfer','operation':operation,
           'source_pod_uid':source_uid,'target_pod_uid':target_uid,'size':size,
           'sha256':digest,'path':None,'data_bytes':None,'extent_frames':None}
 if any(receipt.get(key)!=value for key,value in expected.items() if value is not None):
  raise SystemExit('transfer acknowledgement binding differs')
 print(json.dumps(receipt,sort_keys=True))
finally:
 signal.alarm(0);token=None
 if sock is not None:sock.close()
 os.close(fd)
'''


def transfer_pod_binding(context, worker):
    pod=api_json(context,'-n',worker['namespace'],'get','pod',worker['pod'],'--request-timeout=15s')
    node=api_json(context,'get','node',worker['node'],'--request-timeout=15s')
    if binding(pod,node)!=worker:
        raise ValueError('transfer worker identity changed')
    address=pod.get('status',{}).get('podIP','')
    try:parsed=ipaddress.ip_address(address)
    except ValueError as error:raise ValueError('transfer worker lacks a valid pod IP') from error
    if parsed.version!=4 or parsed.is_loopback or parsed.is_unspecified or parsed.is_multicast:
        raise ValueError('transfer worker pod IP is not routable')
    return {'pod_uid':worker['pod_uid'],'pod_ip':address}


def _control_process(argv, token):
    process=subprocess.Popen(argv,stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE,text=True)
    process.stdin.write(token+'\n');process.stdin.flush();process.stdin.close();process.stdin=None
    return process


def _ready_line(process, timeout=30):
    selector=selectors.DefaultSelector();selector.register(process.stdout,selectors.EVENT_READ)
    try:
        if not selector.select(timeout):raise ValueError('direct transfer receiver readiness timed out')
        line=process.stdout.readline()
    finally:selector.close()
    try:value=json.loads(line)
    except json.JSONDecodeError as error:raise ValueError('direct transfer receiver readiness is invalid') from error
    if set(value)!={'schema','status','port'} or value.get('schema')!=1 or value.get('status')!='ready' \
            or type(value.get('port')) is not int or not 1024<=value['port']<=65535:
        raise ValueError('direct transfer receiver readiness is invalid')
    return value['port']


def sparse_transfer(context, source_worker, target_worker, *, target_container,
                    target_directory, target_name, operation, identity):
    source=transfer_pod_binding(context,source_worker);target=transfer_pod_binding(context,target_worker)
    token=secrets.token_hex(32)
    receiver_argv=kubectl(context,'-n',target_worker['namespace'],'exec','-i',target_worker['pod'],
      '-c',target_container,'--','python3','-c',DIRECT_RECEIVER_SCRIPT,target_directory,target_name,
      target['pod_ip'],source['pod_ip'],operation,source['pod_uid'],target['pod_uid'],
      str(identity['size']),identity['sha256'])
    receiver=_control_process(receiver_argv,token);sender=None
    try:
        port=_ready_line(receiver)
        sender_argv=kubectl(context,'-n',source_worker['namespace'],'exec','-i',source_worker['pod'],
          '-c','disk','--','python3','-c',DIRECT_SENDER_SCRIPT,'/work/disk-output/disk.raw',
          target['pod_ip'],str(port),operation,source['pod_uid'],target['pod_uid'],
          str(identity['size']),identity['sha256'],source['pod_ip'])
        sender=_control_process(sender_argv,token);token=None
        sender_out,sender_error=sender.communicate(timeout=7200)
        receiver_out,receiver_error=receiver.communicate(timeout=60)
        if sender.returncode or receiver.returncode:
            raise ValueError(f'direct sparse transfer failed ({sender.returncode}, {receiver.returncode})')
        sent=json.loads(sender_out);received=json.loads(receiver_out)
        if sent!=received:
            raise ValueError('direct sparse transfer acknowledgements differ')
        expected_values={'schema':1,'purpose':'tilo-direct-sparse-transfer','operation':operation,
          'source_pod_uid':source['pod_uid'],'target_pod_uid':target['pod_uid'],
          'size':identity['size'],'sha256':identity['sha256'],'path':target_directory+'/'+target_name}
        if (set(received)!=(set(expected_values)|{'data_bytes','extent_frames'})
                or any(received.get(key)!=value for key,value in expected_values.items())
                or type(received.get('data_bytes')) is not int or not 0<=received['data_bytes']<=identity['size']
                or type(received.get('extent_frames')) is not int or not 0<=received['extent_frames']<=1048576):
            raise ValueError('direct sparse transfer receipt is invalid')
        if transfer_pod_binding(context,source_worker)!=source or transfer_pod_binding(context,target_worker)!=target:
            raise ValueError('transfer worker binding changed during copy')
        return received
    except BaseException:
        for process in (sender,receiver):
            if process is not None and process.poll() is None:process.kill();process.wait()
        raise
    finally:token=None


VM_CYCLE_JOB = r'''import hashlib,json,os,pathlib,subprocess,sys
job=pathlib.Path(sys.argv[1]); request_path=job/'request.json'; status_path=job/'status.json'; log_path=job/'log.txt'
os.umask(0o077)
MAX_LOG=256*1024
def read_private(path,limit):
 i=path.lstat()
 if not i.st_mode&0o100000 or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>limit: raise ValueError('cycle job file is invalid')
 fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  o=os.fstat(fd)
  if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size): raise ValueError('cycle job file changed')
  value=os.read(fd,limit+1)
  if len(value)!=i.st_size: raise ValueError('cycle job file changed')
  return value
 finally: os.close(fd)
def atom(path,value):
 data=(json.dumps(value,sort_keys=True,separators=(',',':'))+'\n').encode(); temporary=path.with_name('.'+path.name+'.tmp')
 fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 try:
  with os.fdopen(fd,'wb') as out: out.write(data); out.flush(); os.fsync(out.fileno())
  os.replace(temporary,path)
 finally:
  try: temporary.unlink()
  except FileNotFoundError: pass
def process_identity(request):
 proc=pathlib.Path('/proc/self'); start=(proc/'stat').read_text().split()[21]
 return {'pid':os.getpid(),'start':start,
         'argv_sha256':hashlib.sha256((proc/'cmdline').read_bytes()).hexdigest(),
         'cgroup_sha256':hashlib.sha256((proc/'cgroup').read_bytes()).hexdigest(),
         'request_sha256':hashlib.sha256(request).hexdigest()}
def state(binding,name,process,**extra):
 atom(status_path,{'schema':1,'purpose':'tilo-supervised-vm-cycle-job','state':name,
                   'binding':binding,'process':process,**extra})
try:
 request=read_private(request_path,4*1024*1024); value=json.loads(request); binding=value['binding']
 process=process_identity(request); state(binding,'running',process)
 environment=dict(value['environment'])
 if (not environment or any(not isinstance(k,str) or not isinstance(v,str)
                            for k,v in environment.items()) or 'PATH' not in environment
         or 'LC_ALL' not in environment):
  raise ValueError('cycle job environment is not minimal')
 child=subprocess.Popen(['bash','-ceu',value['script'],'tilo-cycle'],stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT,env=environment)
 count=log_path.stat().st_size if log_path.exists() else 0
 with log_path.open('ab',buffering=0) as log:
  while True:
   chunk=child.stdout.read(1024*1024)
   if not chunk: break
   take=max(0,MAX_LOG-count)
   if take: log.write(chunk[:take]); count+=min(len(chunk),take)
   if count>=MAX_LOG and child.poll() is None:
    # Keep the job alive while making its diagnostics bounded.
    child.stdout.read(0)
  returncode=child.wait()
 state(binding,'succeeded' if returncode==0 else 'failed',process,
       returncode=returncode,log_bytes=count,log_truncated=count>=MAX_LOG)
except BaseException:
 try:
  state(value['binding'],'failed',process,returncode=1,
        log_bytes=log_path.stat().st_size if log_path.exists() else 0,
        log_truncated=log_path.exists() and log_path.stat().st_size>=MAX_LOG)
 except BaseException: pass
'''


VM_CYCLE_LAUNCH = r'''import base64,hashlib,json,os,pathlib,stat,subprocess,sys,time
value=json.load(sys.stdin); source=base64.b64decode(sys.argv[1],validate=True)
job=pathlib.Path(sys.argv[2]); expected=value.get('binding'); expected_sha=value.get('binding_sha256')
def atom(path,data):
 temporary=path.with_name('.'+path.name+'.tmp'); fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 try:
  with os.fdopen(fd,'wb') as out: out.write(data); out.flush(); os.fsync(out.fileno())
  os.replace(temporary,path)
 finally:
  try: temporary.unlink()
  except FileNotFoundError: pass
def regular(path,limit):
 i=path.lstat()
 if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>limit: raise ValueError('cycle job state is invalid')
 return json.loads(path.read_bytes())
if (not isinstance(expected,dict) or hashlib.sha256(json.dumps(expected,sort_keys=True,separators=(',',':')).encode()).hexdigest()!=expected_sha): raise SystemExit('invalid cycle job binding')
if job.exists() or job.is_symlink():
 ji=job.lstat()
 if job.is_symlink() or not job.is_dir() or ji.st_uid or ji.st_gid or (ji.st_mode&0o777)!=0o700: raise SystemExit('cycle job path is invalid')
 state=regular(job/'status.json',65536)
 if state.get('binding')!=expected: raise SystemExit('cycle job binding differs')
 print(json.dumps(state,sort_keys=True));sys.exit(0)
job.mkdir(mode=0o700)
request=json.dumps(value,sort_keys=True,separators=(',',':')).encode()+b'\n'; atom(job/'request.json',request)
source_path=job/'job.py'; fd=os.open(source_path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o700)
try:
 with os.fdopen(fd,'wb') as out: out.write(source); out.flush(); os.fsync(out.fileno())
finally: pass
atom(job/'status.json',(json.dumps({'schema':1,'purpose':'tilo-supervised-vm-cycle-job','state':'launched',
                                    'binding':expected,'launched_at':int(time.time())},sort_keys=True)+'\n').encode())
subprocess.Popen(['/usr/bin/python3',str(source_path),str(job)],stdin=subprocess.DEVNULL,
                 stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True,close_fds=True)
print(json.dumps(regular(job/'status.json',65536),sort_keys=True))'''


VM_CYCLE_STATUS = r'''import hashlib,json,pathlib,sys
job=pathlib.Path(sys.argv[1]); expected=json.loads(sys.argv[2])
def read(path,limit):
 i=path.lstat()
 if not i.st_mode&0o100000 or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>limit: raise SystemExit('TILO_CYCLE_STATUS_INVALID')
 return path.read_bytes()
try: value=json.loads(read(job/'status.json',65536)); source=read(job/'job.py',1024*1024); request=read(job/'request.json',4*1024*1024)
except BaseException: print('TILO_CYCLE_STATUS_INVALID'); raise SystemExit(2)
if value.get('binding')!=expected or hashlib.sha256(source).hexdigest()!=expected.get('job_sha256'):
 print('TILO_CYCLE_STATUS_INVALID'); raise SystemExit(2)
process=value.get('process')
if process and process.get('request_sha256')!=hashlib.sha256(request).hexdigest():
 print('TILO_CYCLE_STATUS_INVALID'); raise SystemExit(2)
def completed():
 try:
  evidence=pathlib.Path('/work')/(expected['cycle']+'-evidence')
  if (evidence/'verdict.txt').read_text()!='PASS\n' or (evidence/'exit-status.txt').read_text()!='0\n': return False
  json.loads((pathlib.Path('/work')/'verified/conversion-info.json').read_text())
  if len((pathlib.Path('/work')/'verified/conversion.sha256').read_text().splitlines())!=2: return False
  return True
 except BaseException: return False
if value.get('state')=='running':
 try:
  proc=pathlib.Path('/proc')/str(process['pid']); start=(proc/'stat').read_text().split()[21]; argv=(proc/'cmdline').read_bytes(); cgroup=(proc/'cgroup').read_bytes()
 except (FileNotFoundError,IndexError):
  if completed():
   value=dict(value,state='succeeded',returncode=0); print(json.dumps(value,sort_keys=True)); raise SystemExit(0)
  print('TILO_CYCLE_PROCESS_GONE'); raise SystemExit(3)
 if (start!=process['start'] or hashlib.sha256(argv).hexdigest()!=process['argv_sha256'] or hashlib.sha256(cgroup).hexdigest()!=process['cgroup_sha256']):
  print('TILO_CYCLE_PROCESS_CHANGED'); raise SystemExit(4)
print(json.dumps(value,sort_keys=True))'''


def cycle_worker_identity(context, vm):
    pod=api_json(context,'-n',vm['namespace'],'get','pod',vm['pod'],'--request-timeout=15s')
    node=api_json(context,'get','node',vm['node'],'--request-timeout=15s')
    if binding(pod,node)!=vm:
        raise ValueError('VM worker identity or specification changed')
    containers=pod.get('status',{}).get('containerStatuses',[])
    specs=pod.get('spec',{}).get('containers',[])
    if len(containers)!=1 or len(specs)!=1 or containers[0].get('name')!='qemu' or specs[0].get('name')!='qemu':
        raise ValueError('VM cycle requires one qemu container')
    container_id=containers[0].get('containerID','').removeprefix('containerd://')
    if not re.fullmatch(r'[0-9a-f]{32,128}',container_id) or containers[0].get('imageID')!=vm['image_id']:
        raise ValueError('VM cycle container identity is not pinned')
    return {'namespace':vm['namespace'],'pod':vm['pod'],'pod_uid':vm['pod_uid'],
            'container':'qemu','container_id':container_id,'image':specs[0].get('image'),
            'image_id':vm['image_id']}


def validate_cycle_status(value, binding_value):
    base={'schema','purpose','state','binding'}
    if (not isinstance(value,dict) or value.get('schema')!=1
            or value.get('purpose')!='tilo-supervised-vm-cycle-job'
            or value.get('binding')!=binding_value
            or value.get('state') not in {'launched','running','succeeded','failed'}):
        raise ValueError('VM cycle status differs from its admitted request')
    if value['state']=='launched':
        if set(value)!=(base|{'launched_at'}) or type(value['launched_at']) is not int: raise ValueError('VM cycle launch status is invalid')
        return
    expected=base|{'process','returncode','log_bytes','log_truncated'}
    if set(value)!=expected: raise ValueError('VM cycle status has unexpected fields')
    process=value['process']
    if (not isinstance(process,dict) or set(process)!={'pid','start','argv_sha256','cgroup_sha256','request_sha256'}
            or type(process['pid']) is not int or process['pid']<=1
            or not isinstance(process['start'],str) or not process['start'].isdigit()
            or any(not isinstance(process[key],str) or not re.fullmatch(r'[0-9a-f]{64}',process[key]) for key in ('argv_sha256','cgroup_sha256','request_sha256'))
            or type(value['returncode']) is not int
            or (value['state']=='succeeded' and value['returncode']!=0)
            or (value['state']=='failed' and value['returncode']==0)
            or type(value['log_bytes']) is not int or not 0<=value['log_bytes']<=256*1024
            or type(value['log_truncated']) is not bool): raise ValueError('VM cycle process identity is invalid')


def remote_cycle_status(context, vm, worker, binding_value, job_path):
    try:
        result=subprocess.run(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--','python3','-c',VM_CYCLE_STATUS,job_path,json.dumps(binding_value,sort_keys=True)),capture_output=True,text=True,timeout=60)
    except (OSError,subprocess.TimeoutExpired) as error:
        raise ControllerTransportError('VM cycle status poll lost transport') from error
    marker=result.stdout.strip()
    if result.returncode:
        if marker=='TILO_CYCLE_PROCESS_GONE': raise ValueError('VM cycle process vanished')
        if marker in ('TILO_CYCLE_PROCESS_CHANGED','TILO_CYCLE_STATUS_INVALID'): raise ValueError('VM cycle process identity changed')
        transport=('unable to connect','connection refused','connection reset','context deadline',
                   'i/o timeout','tls handshake timeout','timed out waiting','unexpected eof')
        if any(row in result.stderr.lower() for row in transport):
            raise ControllerTransportError('VM cycle status poll lost transport')
        raise ValueError('VM cycle status command failed')
    value=json.loads(result.stdout); validate_cycle_status(value,binding_value); return value


def poll_cycle_job(context, vm, worker, binding_value, job_path, *, timeout=4*60*60, clock=time.monotonic, sleep=time.sleep):
    deadline=clock()+timeout; last_transport=None; delay=10
    while clock()<deadline:
        try:
            current=cycle_worker_identity(context,vm)
            if current!=worker: raise ValueError('VM cycle worker container identity changed')
            status=remote_cycle_status(context,vm,worker,binding_value,job_path)
        except ControllerTransportError as error:
            last_transport=error; sleep(delay); delay=min(60,delay*2); continue
        delay=10
        if status['state']=='succeeded': return status
        if status['state']=='failed': raise ValueError('detached VM cycle failed')
        if status['state']=='launched' and time.time()-status['launched_at']>60: raise ValueError('VM cycle launch did not establish a process')
        sleep(10)
    if last_transport is not None: raise ControllerTransportError('VM cycle polling timed out after transport loss') from last_transport
    raise ValueError('detached VM cycle timed out')


def validate_handoff_worker(value):
    required={'pod','pod_uid','namespace','node','node_uid','spec','image_id'}
    if (not isinstance(value,dict) or set(value)!=required
            or any(not isinstance(value[key],str) or not value[key] for key in required-{'spec'})
            or not isinstance(value['spec'],dict) or not digest_reference(value['image_id'])):
        raise ValueError('VM handoff worker binding is invalid')


def vm_handoff_document(operation, state, vm, identity, verifier=None):
    if state not in {'vm-created','media-ready','verifier-release-intent','verifier-released'}:
        raise ValueError('invalid VM handoff state')
    validate_handoff_worker(vm); validate_raw_identity(identity)
    value={'schema':1,'purpose':'tilo-supervised-vm-handoff','operation':operation,
           'state':state,'vm':vm,'raw_media':identity}
    if verifier is not None: validate_handoff_worker(verifier); value['verifier']=verifier
    if state in {'verifier-release-intent','verifier-released'} and verifier is None:
        raise ValueError('verifier release state lacks its worker binding')
    return value


def persist_vm_handoff(operation_dir, operation, state, vm, identity, verifier=None):
    value=vm_handoff_document(operation,state,vm,identity,verifier)
    write_private(Path(operation_dir)/'vm-handoff.json',
                  (json.dumps(value,indent=2,sort_keys=True)+'\n').encode())
    return value


def persist_vm_create_intent(operation_dir, operation, manifest):
    value={'schema':1,'purpose':'tilo-supervised-vm-create-intent','operation':operation,
           'manifest_sha256':canonical_sha256(manifest),'manifest':manifest}
    write_private(Path(operation_dir)/'vm-create-intent.json',
                  (json.dumps(value,indent=2,sort_keys=True)+'\n').encode())
    return value


def load_vm_create_intent(operation_dir, operation, manifest=None):
    value=json.loads((Path(operation_dir)/'vm-create-intent.json').read_text())
    if (set(value)!={'schema','purpose','operation','manifest_sha256','manifest'}
            or value['schema']!=1 or value['purpose']!='tilo-supervised-vm-create-intent'
            or value['operation']!=operation
            or value['manifest_sha256']!=canonical_sha256(value['manifest'])
            or (manifest is not None and value['manifest']!=manifest)):
        raise ValueError('VM create intent differs from the deterministic manifest')
    return value


def load_vm_handoff(operation_dir, operation, state=None, expected_vm=None, expected_identity=None):
    value=json.loads((Path(operation_dir)/'vm-handoff.json').read_text())
    required={'schema','purpose','operation','state','vm','raw_media'}
    if (set(value)-{'verifier'}!=required or value['schema']!=1
            or value['purpose']!='tilo-supervised-vm-handoff' or value['operation']!=operation
            or value['state'] not in {'vm-created','media-ready','verifier-release-intent','verifier-released'}):
        raise ValueError('VM handoff receipt is invalid')
    if state is not None and value['state']!=state: raise ValueError('VM handoff is at the wrong stage')
    validate_handoff_worker(value['vm']); validate_raw_identity(value['raw_media'])
    if expected_vm is not None and value['vm']!=expected_vm:
        raise ValueError('VM handoff worker differs from the expected VM')
    if expected_identity is not None and value['raw_media']!=expected_identity:
        raise ValueError('VM handoff media differs from the expected identity')
    if value['state'] in {'verifier-release-intent','verifier-released'}:
        if 'verifier' not in value: raise ValueError('VM handoff lacks verifier release binding')
        validate_handoff_worker(value['verifier'])
    return value


def adopt_vm_handoff(context, operation_dir, operation, identity):
    value=load_vm_handoff(operation_dir,operation,expected_identity=identity)
    vm=value['vm']; current=optional_same_worker(context,vm)
    if current is None or current!=vm:
        raise ValueError('VM handoff worker is no longer the exact live VM')
    return value


def repair_vm_handoff_bindings(operation_dir, handoff, creator, verifier, vm):
    if handoff.get('state')!='vm-created' or handoff.get('vm')!=vm \
            or handoff.get('verifier')!=verifier:
        raise ValueError('VM handoff is not eligible for binding repair')
    validate_creator_binding(creator); validate_handoff_worker(verifier)
    path=Path(operation_dir)/'worker-bindings.json'
    if path.is_file():
        value=json.loads(path.read_text())
        if not isinstance(value,dict) or value.get('creator')!=creator \
                or value.get('verifier')!=verifier:
            raise ValueError('worker bindings differ before VM binding repair')
    else:
        value={'creator':creator,'verifier':verifier}
    if 'vm' in value and value['vm']!=vm:
        raise ValueError('worker bindings contain a different VM')
    value['vm']=vm
    write_private(path,(json.dumps(value,indent=2,sort_keys=True)+'\n').encode())
    return value


def detached_media_identity(context, vm, expected, operation, operation_dir=None):
    worker=cycle_worker_identity(context,vm)
    path='/work/verified/installer.raw'; output='/work/'+operation+'-media-identity.json'
    script=r'''set -euo pipefail
test "$(stat -c %s /work/verified/installer.raw)" = "$MEDIA_BYTES"
python3 -c 'import hashlib,json,os; p="/work/verified/installer.raw"; h=hashlib.sha256(); f=open(p,"rb"); [h.update(c) for c in iter(lambda:f.read(8*1024*1024),b"")]; print(json.dumps({"size":os.stat(p).st_size,"sha256":h.hexdigest()}))' > "$MEDIA_IDENTITY.tmp"
chmod 0400 "$MEDIA_IDENTITY.tmp"; mv -f -- "$MEDIA_IDENTITY.tmp" "$MEDIA_IDENTITY"
test "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256"])' "$MEDIA_IDENTITY")" = "$MEDIA_SHA256"
'''
    environment={'PATH':'/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
                 'LC_ALL':'C','MEDIA_BYTES':str(expected['size']),'MEDIA_SHA256':expected['sha256'],
                 'MEDIA_IDENTITY':output}
    python_sha=command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',
                                'sha256sum','/usr/bin/python3'),timeout=30).split()[0]
    if not re.fullmatch(r'[0-9a-f]{64}',python_sha): raise ValueError('media identity helper is invalid')
    binding_value={'schema':1,'purpose':'tilo-supervised-vm-cycle-job','operation':operation+'-media',
      'cycle':operation+'-media-identity','vm':worker,'disk':expected,
      'helper':{'path':'/usr/bin/python3','sha256':python_sha},
      'script_sha256':hashlib.sha256(script.encode()).hexdigest(),
      'environment_sha256':canonical_sha256(environment),
      'args_sha256':canonical_sha256({'command':['bash','-ceu','tilo-cycle'],'environment':environment}),
      'job_sha256':hashlib.sha256(VM_CYCLE_JOB.encode()).hexdigest()}
    job_path='/work/'+operation+'-media-identity-job'
    payload={'binding':binding_value,'binding_sha256':canonical_sha256(binding_value),
             'environment':environment,'script':script}
    launch=command(kubectl(context,'-n',vm['namespace'],'exec','-i',vm['pod'],'-c','qemu','--',
      'python3','-c',VM_CYCLE_LAUNCH,base64.b64encode(VM_CYCLE_JOB.encode()).decode(),job_path),
      json.dumps(payload),timeout=60)
    validate_cycle_status(json.loads(launch),binding_value)
    status=poll_cycle_job(context,vm,worker,binding_value,job_path)
    result=json.loads(command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',
      'python3','-c','import json,stat,sys; p=sys.argv[1]; i=__import__("os").lstat(p); assert stat.S_ISREG(i.st_mode) and i.st_uid==0 and i.st_nlink==1 and i.st_size<=4096; print(open(p).read())',output),timeout=30))
    if result!=expected: raise ValueError('detached VM media identity differs from the transferred media')
    if operation_dir is not None:
        write_private(Path(operation_dir)/'media-ready-receipt.json',
          (json.dumps({'schema':1,'purpose':'tilo-supervised-media-ready','binding':binding_value,
                       'status':status,'identity':result},indent=2,sort_keys=True)+'\n').encode())
    return result


def remote_media_present(context, vm):
    script=r'''import json,os,stat,sys
p=sys.argv[1]
try: i=os.lstat(p)
except FileNotFoundError: print(json.dumps({'present':False})); raise SystemExit(0)
if (not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1
        or stat.S_IMODE(i.st_mode) not in (0o400,0o600) or i.st_size<=0): raise SystemExit(2)
print(json.dumps({'present':True,'size':i.st_size}))'''
    value=json.loads(command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',
      'python3','-c',script,'/work/verified/installer.raw'),timeout=30))
    if set(value)=={'present'} and value['present'] is False: return False
    if set(value)!={'present','size'} or value['present'] is not True or type(value['size']) is not int:
        raise ValueError('remote media destination identity is invalid')
    return True


def load_media_ready_receipt(operation_dir, operation, identity, expected_worker=None):
    value=json.loads((Path(operation_dir)/'media-ready-receipt.json').read_text())
    required={'schema','purpose','binding','status','identity'}
    if (set(value)!=required or value['schema']!=1
            or value['purpose']!='tilo-supervised-media-ready'
            or value['identity']!=identity):
        raise ValueError('media-ready receipt is invalid')
    binding_value=value['binding']
    if (not isinstance(binding_value,dict) or not isinstance(value['status'],dict)
            or binding_value.get('disk')!=identity
            or (expected_worker is not None and binding_value.get('vm')!=expected_worker)
            or value['status'].get('state')!='succeeded'):
        raise ValueError('media-ready receipt is not a completed exact result')
    validate_cycle_status(value['status'],binding_value)
    return value


def validate_local_cycle_receipts(operation_dir, operation, cycle, binding_value, job_path):
    operation_dir=Path(operation_dir)
    binding_path=operation_dir/'vm-cycle-binding.json'
    receipt_path=operation_dir/'vm-cycle-receipt.json'
    if binding_path.is_file():
        value=json.loads(binding_path.read_text())
        if (set(value)!={'schema','purpose','binding','job_path','cycle'}
                or value['schema']!=1 or value['purpose']!='tilo-supervised-vm-cycle-binding'
                or value['binding']!=binding_value or value['job_path']!=job_path
                or value['cycle']!=cycle):
            raise ValueError('local VM cycle binding differs from the admitted job')
    if receipt_path.is_file():
        value=json.loads(receipt_path.read_text())
        if (set(value)!={'schema','purpose','binding','status'} or value['schema']!=1
                or value['purpose']!='tilo-supervised-vm-cycle-receipt'
                or value['binding']!=binding_value):
            raise ValueError('local VM cycle receipt differs from the admitted job')
        validate_cycle_status(value['status'],binding_value)
        if value['status']['state']!='succeeded':
            raise ValueError('local VM cycle receipt is not successful')


def convert_and_cycle(context, vm, identity, acceptance_id, operation_dir=None):
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
    environment={'PATH':'/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin',
      'LC_ALL':'C','MEDIA_BYTES':str(identity['size']),'MEDIA_SHA256':identity['sha256'],
      'FIRMWARE_ROWS':fixed,'TILO_VM_ARCH':'aarch64','TILO_ACCEL':'kvm','TILO_DISTROBOX':'',
      'TILO_MEM':'4096','TILO_SMP':'2','TILO_SECBOOT':'0','TILO_SECBOOT_KEYS':'none',
      'TILO_PROFILE_ENCODER':'/usr/local/bin/tilo-imager-profile',
      'TILO_PROFILE_ENCODER_SHA256':PROFILE_ENCODER_SHA256,'TILO_VM_CYCLE':cycle,
      'TILO_RUN':'/work/'+cycle+'-run','TILO_INSTALLER_CYCLE_STORAGE':'/work/'+cycle+'-storage',
      'TILO_INSTALLER_OUTPUT':'/work/'+cycle+'-storage','EVIDENCE':'/work/'+cycle+'-evidence'}
    worker=cycle_worker_identity(context,vm)
    helper_path='/work/source/os/dev/test-installer-cycle.sh'
    helper_sha=command(kubectl(context,'-n',vm['namespace'],'exec',vm['pod'],'-c','qemu','--',
                                'sha256sum',helper_path),timeout=30).split()[0]
    if not re.fullmatch(r'[0-9a-f]{64}',helper_sha): raise ValueError('cycle helper identity is invalid')
    binding_value={'schema':1,'purpose':'tilo-supervised-vm-cycle-job','operation':acceptance_id,
      'cycle':cycle,'vm':worker,'disk':identity,'helper':{'path':helper_path,'sha256':helper_sha},
      'script_sha256':hashlib.sha256(script.encode()).hexdigest(),
      'environment_sha256':canonical_sha256(environment),
      'args_sha256':canonical_sha256({'command':['bash','-ceu','tilo-cycle'],'environment':environment}),
      'job_sha256':hashlib.sha256(VM_CYCLE_JOB.encode()).hexdigest()}
    job_path='/work/'+cycle+'-job'
    if operation_dir is not None:
        validate_local_cycle_receipts(operation_dir,acceptance_id,cycle,binding_value,job_path)
    payload={'binding':binding_value,'binding_sha256':canonical_sha256(binding_value),
             'environment':environment,'script':script}
    launch=command(kubectl(context,'-n',vm['namespace'],'exec','-i',vm['pod'],'-c','qemu','--',
      'python3','-c',VM_CYCLE_LAUNCH,base64.b64encode(VM_CYCLE_JOB.encode()).decode(),job_path),
      json.dumps(payload),timeout=60)
    launched=json.loads(launch); validate_cycle_status(launched,binding_value)
    if operation_dir is not None:
        write_private(Path(operation_dir)/'vm-cycle-binding.json',
          (json.dumps({'schema':1,'purpose':'tilo-supervised-vm-cycle-binding',
                       'binding':binding_value,'job_path':job_path,'cycle':cycle},indent=2,sort_keys=True)+'\n').encode())
    status=poll_cycle_job(context,vm,worker,binding_value,job_path)
    if operation_dir is not None:
        write_private(Path(operation_dir)/'vm-cycle-receipt.json',
          (json.dumps({'schema':1,'purpose':'tilo-supervised-vm-cycle-receipt',
                       'binding':binding_value,'status':status},indent=2,sort_keys=True)+'\n').encode())
    return cycle


def cycle_evidence_paths(cycle):
    return {'conversion_info':'/work/verified/conversion-info.json',
             'conversion_sha256':'/work/verified/conversion.sha256',
             'verdict':f'/work/{cycle}-evidence/verdict.txt',
             'exit_status':f'/work/{cycle}-evidence/exit-status.txt',
             'oobe_intro':f'/work/{cycle}-evidence/08-oobe-intro.txt',
             'oobe_backup':f'/work/{cycle}-evidence/09-oobe-backup.txt',
             'oobe_backup_resumed':f'/work/{cycle}-evidence/10-oobe-backup-resumed.txt',
             'oobe_ready':f'/work/{cycle}-evidence/11-oobe-ready.txt',
             'tour_move':f'/work/{cycle}-evidence/12-tour-move.txt',
             'tour_arrange':f'/work/{cycle}-evidence/13-tour-arrange.txt',
             'tour_share':f'/work/{cycle}-evidence/14-tour-share.txt',
             'tour_start':f'/work/{cycle}-evidence/15-tour-start.txt',
             'desktop_ocr':f'/work/{cycle}-evidence/16-desktop-config.txt',
             'browser_ocr':f'/work/{cycle}-evidence/18-browser-offline.txt',
             'reboot_greeter':f'/work/{cycle}-evidence/19-reboot-greeter.txt',
             'reboot_login':f'/work/{cycle}-evidence/20-reboot-login.txt',
             'browser_fixture':f'/work/{cycle}-evidence/browser-offline-fixture.txt',
             'browser_fixture_html':f'/work/{cycle}-evidence/browser-offline-fixture.html'}


def acceptance_evidence(context, vm, cycle):
    paths = cycle_evidence_paths(cycle)
    script = r'''import json,os,pathlib,stat,sys
p=json.loads(sys.argv[1]); total=0; result={}
for key,path in p.items():
 i=os.lstat(path)
 if not stat.S_ISREG(i.st_mode) or i.st_uid or i.st_gid or i.st_nlink!=1 or i.st_mode&0o077 or i.st_size>256*1024 or total+i.st_size>4*1024*1024: raise SystemExit(2)
 fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  o=os.fstat(fd)
  if (o.st_dev,o.st_ino,o.st_size)!=(i.st_dev,i.st_ino,i.st_size): raise SystemExit(2)
  data=os.read(fd,256*1024+1)
  if len(data)!=i.st_size: raise SystemExit(2)
 finally: os.close(fd)
 result[key]=data.decode(); total+=len(data)
print(json.dumps(result,sort_keys=True))'''
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
    required_ocr={
      'oobe_intro':r'create\s+your\s+local',
      'oobe_backup':r'back\s+up\s+your\s+tilo\s+identity',
      'oobe_backup_resumed':r'back\s+up\s+your\s+tilo\s+identity',
      'oobe_ready':r'your\s+identity\s+is\s+ready',
      'tour_move':r'everything\s+is\s+close\s+at\s+hand',
      'tour_arrange':r'a\s+place\s+for\s+each\s+kind\s+of\s+work',
      'tour_share':r'sharing\s+stays\s+deliberate',
      'tour_start':r'your\s+first\s+five\s+minutes'}
    for key,pattern in required_ocr.items():
        if not re.search(pattern,evidence.get(key,''),re.IGNORECASE):
            raise ValueError('cycle evidence lacks complete OOBE and tutorial proof')
    if not re.search(r'cycle\W*tilo', evidence.get('desktop_ocr',''), re.IGNORECASE):
        raise ValueError('cycle evidence lacks the installed desktop hostname')
    if not re.search(r'welcome\s+back|sign\s+in', evidence.get('reboot_greeter',''), re.IGNORECASE):
        raise ValueError('cycle evidence lacks the post-OOBE cold-boot greeter')
    if not re.search(r'cycle\W*tilo', evidence.get('reboot_login',''), re.IGNORECASE):
        raise ValueError('cycle evidence lacks the post-OOBE authenticated desktop')
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
        try:
            if process.wait(timeout=max(1,deadline-time.monotonic()))!=0:
                raise ValueError('cycle evidence export failed')
        except subprocess.TimeoutExpired as error:
            process.kill(); process.wait()
            raise ValueError('cycle evidence export exceeded deadline') from error
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
                      '07-first-boot-oobe.png','16-desktop-config.png',
                      '08-oobe-intro.png','09-oobe-backup.png',
                      '10-oobe-backup-resumed.png','11-oobe-ready.png',
                      '12-tour-move.png','13-tour-arrange.png','14-tour-share.png',
                      '15-tour-start.png','19-reboot-greeter.png','20-reboot-login.png',
                      '17-browser-open.png','18-browser-offline.png',
                      'browser-offline-fixture.txt','browser-offline-fixture.html'}
            allowed={''}
            for name in required:
                parts=Path(name).parts
                allowed.update('/'.join(parts[:index]) for index in range(1,len(parts)))
            if not required.issubset(names) or not names.issubset(allowed):
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


def release_exact_worker(context, worker):
    pod=optional_api_json(context,'-n',worker['namespace'],'get','pod',worker['pod'],
                          '--request-timeout=15s')
    if pod is None:
        return
    if pod.get('metadata',{}).get('uid')!=worker['pod_uid']:
        raise ValueError('release target worker UID changed')
    if pod.get('metadata',{}).get('deletionTimestamp'):
        command(kubectl(context,'-n',worker['namespace'],'wait','--for=delete',
                        '--timeout=2m','pod/'+worker['pod']),timeout=150)
        return
    delete_worker(context,worker)
    if optional_api_json(context,'-n',worker['namespace'],'get','pod',worker['pod'],
                         '--request-timeout=15s') is not None:
        raise ValueError('release target worker remained after deletion')


def cleanup_paused_creator(context, operation_dir, now=None):
    operation_dir=Path(operation_dir)
    checkpoint=json.loads((operation_dir/'creator-checkpoint.json').read_text())
    validate_creator_checkpoint(checkpoint)
    pause=json.loads((operation_dir/'paused-creator.json').read_text())
    validate_pause_receipt(pause,checkpoint)
    expected_hash=hashlib.sha256((operation_dir/'creator-checkpoint.json').read_bytes()).hexdigest()
    if pause['creator_checkpoint_sha256']!=expected_hash:
        raise ValueError('paused creator receipt does not bind its checkpoint bytes')
    if pause['cleanup_command']!=paused_cleanup_command(context,operation_dir):
        raise ValueError('paused creator cleanup command differs from this controller')
    current=int(time.time()) if now is None else now
    if type(current) is not int or current < checkpoint['expires_at']:
        raise ValueError('paused creator has not reached its cleanup expiry')
    worker=checkpoint['creator']
    pod=api_json(context,'-n',worker['namespace'],'get','pod',worker['pod'],
                 '--request-timeout=15s')
    labels=pod.get('metadata',{}).get('labels',{})
    if (pod.get('metadata',{}).get('uid')!=worker['pod_uid']
            or labels.get(OWNER_LABEL)!=OWNER
            or labels.get(OPERATION_LABEL)!=checkpoint['acceptance_id']
            or labels.get(EXPIRES_LABEL)!=str(checkpoint['expires_at'])):
        raise ValueError('paused creator cleanup ownership does not match the live pod')
    delete_worker(context,worker)
    write_private(operation_dir/'paused-creator-cleaned.json',
      (json.dumps({'schema':1,'purpose':'tilo-supervised-creator-expiry-cleanup',
                   'acceptance_id':checkpoint['acceptance_id'],'creator':worker,
                   'expired_at':checkpoint['expires_at'],'cleaned_at':current},
                  indent=2,sort_keys=True)+'\n').encode())


def resume_creator(args, nodes):
    checkpoint,creation,identity=load_creator_checkpoint(args)
    pause_path=args.operation_dir/'paused-creator.json'
    if pause_path.exists():
        pause=json.loads(pause_path.read_text())
    else:
        # A hard controller/WSL termination cannot write the pause receipt, but
        # the credential-free checkpoint was fsynced before verifier work.
        pause=pause_creator(args,checkpoint,'controller-restarted-after-checkpoint')
    validate_pause_receipt(pause,checkpoint)
    expected=hashlib.sha256((args.operation_dir/'creator-checkpoint.json').read_bytes()).hexdigest()
    if pause['creator_checkpoint_sha256']!=expected:
        raise ValueError('paused creator receipt does not bind its checkpoint bytes')
    if pause['cleanup_command']!=paused_cleanup_command(args.context,args.operation_dir):
        raise ValueError('paused creator cleanup command differs from this controller')
    disk=checkpoint['creator']
    handoff=load_verifier_handoff(args,checkpoint,required=False)
    live=optional_same_worker(args.context,disk)
    if live is None:
        attempt=args.operation_dir/'creator-release-attempt.json'
        if handoff is None or handoff['state'] not in ('started','ready','released') or not attempt.is_file():
            raise ValueError('paused creator disappeared before a durable release attempt')
        validate_release_attempt(json.loads(attempt.read_text()),checkpoint,handoff['verifier'])
        if handoff['state']=='started':
            handoff=complete_interrupted_creator_release(args,nodes,checkpoint,handoff)
    disk_node=current_creator_node(nodes,checkpoint)
    excluded=(() if handoff is None else (handoff['verifier']['node'],))
    _,vm_node=select_nodes(nodes,disk_node['metadata']['name'],args.vm_node,excluded)
    return checkpoint,creation,identity,disk,disk_node,vm_node


def resume_pending_creator(args, nodes):
    """Adopt only the exact pre-checkpoint creator after a control-plane loss."""
    bindings=json.loads((args.operation_dir/'worker-bindings.json').read_text())
    if set(bindings) != {'creator'}:
        raise ValueError('pre-checkpoint creator has unexpected worker bindings')
    disk=bindings['creator']; validate_creator_binding(disk)
    live=optional_same_worker(args.context,disk)
    if live is None:
        raise ValueError('pre-checkpoint creator disappeared and must be restarted fresh')
    pod=api_json(args.context,'-n',disk['namespace'],'get','pod',disk['pod'],'--request-timeout=15s')
    labels=pod.get('metadata',{}).get('labels',{})
    try: expires_at=int(labels[EXPIRES_LABEL])
    except (KeyError,TypeError,ValueError) as error: raise ValueError('pre-checkpoint creator expiry is invalid') from error
    if (labels.get(OWNER_LABEL)!=OWNER or labels.get(OPERATION_LABEL)!=args.acceptance_id
            or expires_at<=int(time.time())):
        raise ValueError('pre-checkpoint creator ownership or expiry differs')
    matches=[node for node in nodes.get('items',[]) if node.get('metadata',{}).get('name')==disk['node']]
    if len(matches)!=1 or matches[0].get('metadata',{}).get('uid')!=disk['node_uid']:
        raise ValueError('pre-checkpoint creator node changed')
    _,vm_node=select_nodes(nodes,disk['node'],args.vm_node)
    return disk,matches[0],vm_node,expires_at


def resume_creator_create_intent(args, nodes):
    intent=load_creator_create_intent(args.operation_dir,args.acceptance_id)
    disk=adopt_worker(args.context,intent['manifest'])
    if disk is None:
        raise ValueError('deterministic creator create intent has no live Pod to adopt')
    write_private(args.operation_dir/'worker-bindings.json',
      (json.dumps({'creator':disk},indent=2,sort_keys=True)+'\n').encode())
    return resume_pending_creator(args,nodes)


def verifier_manifest(args, node, checkpoint):
    return disk_manifest('tilo-verify-'+args.acceptance_id,node,deadline=7200,
                         operation=args.acceptance_id,expires_at=checkpoint['expires_at'])


def verifier_handoff_document(args, checkpoint, verifier, state):
    if state not in ('started','ready','released'):
        raise ValueError('invalid verifier handoff state')
    validate_creator_binding(verifier)
    return {'schema':1,'purpose':'tilo-supervised-verifier-handoff','state':state,
      'acceptance_id':args.acceptance_id,'verifier':verifier,
      'creator_checkpoint_sha256':hashlib.sha256(
        (args.operation_dir/'creator-checkpoint.json').read_bytes()).hexdigest()}


def write_verifier_handoff(args, checkpoint, verifier, state):
    document=verifier_handoff_document(args,checkpoint,verifier,state)
    write_private(args.operation_dir/'verifier-handoff.json',
                  (json.dumps(document,indent=2,sort_keys=True)+'\n').encode())
    return document


def load_verifier_handoff(args, checkpoint, required=True):
    path=args.operation_dir/'verifier-handoff.json'
    if not path.exists():
        if required: raise ValueError('verifier handoff receipt is missing')
        return None
    value=json.loads(path.read_text())
    required_keys={'schema','purpose','state','acceptance_id','verifier',
                   'creator_checkpoint_sha256'}
    expected=hashlib.sha256((args.operation_dir/'creator-checkpoint.json').read_bytes()).hexdigest()
    if (not isinstance(value,dict) or set(value)!=required_keys or value['schema']!=1
            or value['purpose']!='tilo-supervised-verifier-handoff'
            or value['state'] not in ('started','ready','released')
            or value['acceptance_id']!=checkpoint['acceptance_id']
            or value['creator_checkpoint_sha256']!=expected):
        raise ValueError('verifier handoff receipt has invalid metadata')
    validate_creator_binding(value['verifier'])
    return value


def validate_release_attempt(value, checkpoint, verifier):
    required={'schema','purpose','acceptance_id','creator_pod_uid','verifier_pod_uid',
              'creator_checkpoint_sha256'}
    expected=hashlib.sha256(json.dumps(checkpoint,sort_keys=True).encode()).hexdigest()
    if (not isinstance(value,dict) or set(value)!=required or value['schema']!=1
            or value['purpose']!='tilo-supervised-creator-release-attempt'
            or value['acceptance_id']!=checkpoint['acceptance_id']
            or value['creator_pod_uid']!=checkpoint['creator']['pod_uid']
            or value['verifier_pod_uid']!=verifier['pod_uid']
            or value['creator_checkpoint_sha256']!=expected):
        raise ValueError('creator release attempt has invalid binding')


def write_release_attempt(args, checkpoint, verifier):
    value={'schema':1,'purpose':'tilo-supervised-creator-release-attempt',
      'acceptance_id':args.acceptance_id,'creator_pod_uid':checkpoint['creator']['pod_uid'],
      'verifier_pod_uid':verifier['pod_uid'],
      'creator_checkpoint_sha256':hashlib.sha256(
        json.dumps(checkpoint,sort_keys=True).encode()).hexdigest()}
    validate_release_attempt(value,checkpoint,verifier)
    write_private(args.operation_dir/'creator-release-attempt.json',
                  (json.dumps(value,indent=2,sort_keys=True)+'\n').encode())


def complete_interrupted_creator_release(args, nodes, checkpoint, handoff):
    """Finish the exact release whose durable attempt outlived the creator."""
    verifier=handoff['verifier']
    matches=[node for node in nodes.get('items',[])
             if node.get('metadata',{}).get('name')==verifier['node']]
    if len(matches)!=1:
        raise ValueError('release-attempt verifier node no longer exists')
    verifier_node=matches[0]
    validate_creator_node(verifier_node,verifier)
    creator_kernel=checkpoint['creator_node']['metadata']['labels'].get(
      'agent-sandbox.podesta.ai/disk-kernel')
    verifier_kernel=verifier_node['metadata']['labels'].get(
      'agent-sandbox.podesta.ai/disk-kernel')
    if verifier['node']==checkpoint['creator']['node'] or verifier_kernel!=creator_kernel:
        raise ValueError('release-attempt verifier is not the separate reviewed worker')
    manifest=verifier_manifest(args,verifier_node,checkpoint)
    if optional_same_worker(args.context,verifier,manifest) is None:
        raise ValueError('release-attempt verifier disappeared')
    released=write_verifier_handoff(args,checkpoint,verifier,'released')
    paused=args.operation_dir/'paused-creator.json'
    if paused.exists(): os.replace(paused,args.operation_dir/'creator-released.json')
    return released


def acquire_verifier(args, nodes, checkpoint, disk_node, vm_node):
    handoff=load_verifier_handoff(args,checkpoint,required=False)
    retained_node=handoff['verifier']['node'] if handoff is not None else args.verifier_node
    verifier_node=select_verifier_node(nodes,disk_node,vm_node,retained_node)
    manifest=verifier_manifest(args,verifier_node,checkpoint)
    if handoff is not None:
        verifier=handoff['verifier']
        observed=optional_same_worker(args.context,verifier,manifest)
        if handoff['state'] in ('ready','released'):
            if observed is None:
                raise ValueError('durable ready verifier disappeared')
            return verifier,True
        if observed is not None:
            delete_worker(args.context,verifier)
        (args.operation_dir/'verifier-handoff.json').unlink()
    verifier=adopt_worker(args.context,manifest)
    if verifier is None:
        try:
            verifier=create_worker(args.context,manifest)
        except (ValueError,OSError,subprocess.TimeoutExpired) as original:
            try: verifier=adopt_worker(args.context,manifest)
            except (ValueError,OSError,subprocess.TimeoutExpired): raise original
            if verifier is None: raise original
    try:
        write_verifier_handoff(args,checkpoint,verifier,'started')
    except BaseException as error:
        try: delete_worker(args.context,verifier)
        except BaseException as cleanup_error:
            error.add_note('unrecorded verifier cleanup failed: '+str(cleanup_error))
        raise
    return verifier,False


def release_creator_after_handoff(args, checkpoint, disk, verifier):
    write_release_attempt(args,checkpoint,verifier)
    live=optional_same_worker(args.context,disk)
    if live is not None:
        delete_worker(args.context,disk)
    write_verifier_handoff(args,checkpoint,verifier,'released')
    paused=args.operation_dir/'paused-creator.json'
    if paused.exists(): os.replace(paused,args.operation_dir/'creator-released.json')


def handoff_creator_to_verifier(root,args,nodes,checkpoint,disk_node,vm_node,disk,
                                creation,identity,helpers,public):
    verifier=None
    try:
        verifier,ready=acquire_verifier(args,nodes,checkpoint,disk_node,vm_node)
        if not ready:
            write_private(args.operation_dir/'worker-bindings.json',
              (json.dumps({'creator':disk,'verifier':verifier},indent=2,sort_keys=True)+'\n').encode())
            verifier_agent=host_agent(args.context,verifier['node'])
            controller(root,args,verifier,verifier_agent,True,'verifier-environment.json')
            require_same_worker(args.context,disk);require_same_worker(args.context,verifier)
            sparse_transfer(args.context,disk,verifier,target_container='disk',
                            target_directory='/work/disk-output',target_name='disk.raw',
                            operation=args.acceptance_id,identity=identity)
            verifier_identity=raw_identity(args.context,verifier,operation=args.acceptance_id)
            if verifier_identity!=identity:
                raise ValueError('independent verifier received different raw media')
            copied=transfer_receipt(creation,identity,remote_file_identity(args.context,verifier))
            stream_tree(args.context,verifier['namespace'],verifier['pod'],'disk','/work/verifier',helpers)
            stream_tree(args.context,verifier['namespace'],verifier['pod'],'disk','/work/public-trust',public)
            stream_file(args.context,verifier['namespace'],verifier['pod'],'disk',
                        '/work/disk-output/creation.json',creation)
            stream_file(args.context,verifier['namespace'],verifier['pod'],'disk',
                        '/work/disk-output/copy.json',
                        (json.dumps(copied,sort_keys=True)+'\n').encode())
            prepare_verifier_devices(args.context,verifier)
            write_verifier_handoff(args,checkpoint,verifier,'ready')
        release_creator_after_handoff(args,checkpoint,disk,verifier)
        return verifier
    except BaseException as error:
        try: pause_creator(args,checkpoint,type(error).__name__)
        except BaseException as pause_error:
            error.add_note('pause receipt refresh failed: '+str(pause_error))
        try: handoff=load_verifier_handoff(args,checkpoint,required=False)
        except BaseException as receipt_error:
            error.add_note('verifier handoff receipt read failed: '+str(receipt_error))
            handoff=None
        if verifier is not None and (handoff is None or handoff['state']=='started'):
            try:
                if optional_same_worker(args.context,verifier) is not None:
                    delete_worker(args.context,verifier)
                (args.operation_dir/'verifier-handoff.json').unlink(missing_ok=True)
            except BaseException as cleanup_error:
                error.add_note('verifier cleanup deferred to resume: '+str(cleanup_error))
        raise


def establish_creator_checkpoint(args,disk,disk_node,creation,identity,expires_at):
    try:
        checkpoint=checkpoint_creator(args,disk,disk_node,creation,identity,expires_at)
        pause_creator(args,checkpoint,'creator-checkpoint-ready')
        return checkpoint
    except BaseException as error:
        try: delete_worker(args.context,disk)
        except BaseException as cleanup_error:
            error.add_note('creator cleanup after checkpoint failure failed: '+str(cleanup_error))
        raise


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--context',required=True);p.add_argument('--operation-dir',type=Path,required=True)
    p.add_argument('--disk-args',type=Path);p.add_argument('--public-bundle',type=Path)
    p.add_argument('--installer-bootc-helper',type=Path)
    p.add_argument('--host-pins',type=Path);p.add_argument('--helper-revision')
    p.add_argument('--acceptance-id');p.add_argument('--disk-node');p.add_argument('--verifier-node');p.add_argument('--vm-node')
    mode=p.add_mutually_exclusive_group()
    mode.add_argument('--resume',action='store_true')
    mode.add_argument('--cleanup-paused',action='store_true')
    auth=p.add_mutually_exclusive_group()
    auth.add_argument('--registry-auth',type=Path);auth.add_argument('--registry-secret',default=None)
    p.add_argument('--registry-secret-namespace',default='kata-system')
    args=p.parse_args()
    if args.cleanup_paused:
        cleanup_paused_creator(args.context,args.operation_dir)
        return
    required=('disk_args','public_bundle','installer_bootc_helper','host_pins',
              'helper_revision','acceptance_id')
    if any(getattr(args,name) is None for name in required):
        p.error('creation and resume require all pinned operation inputs')
    if args.registry_auth is None and args.registry_secret is None:
        args.registry_secret='forgejo-pull-secret'
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{2,24}',args.acceptance_id):
        p.error('acceptance ID must be a short DNS-safe fresh identifier')
    if args.resume:
        if not args.operation_dir.is_dir():
            p.error('resume operation directory does not exist')
    else:
        args.operation_dir.mkdir(mode=0o700,parents=True,exist_ok=False)
    remove_legacy_registry_auth(args.operation_dir)
    timer=LaneTimer(args.operation_dir/'timings.json')
    values=json.loads(args.disk_args.read_text());validate_disk_args(values)
    root=Path(__file__).resolve().parents[2]; helpers=git_archive(root,args.helper_revision)
    public=public_archive(args.public_bundle)
    installer_bootc = installer_bootc_archive(
        args.installer_bootc_helper, values['installer_bootc_sha256'])
    pending_creator=False
    with timer.stage('creator_admission'):
        nodes=api_json(args.context,'get','nodes','-l','kubernetes.io/arch=arm64','--request-timeout=20s')
        if args.resume and (args.operation_dir/'creator-checkpoint.json').exists():
            checkpoint,creation,identity,disk,disk_node,vm_node=resume_creator(args,nodes)
        elif args.resume and not (args.operation_dir/'worker-bindings.json').is_file() \
                and (args.operation_dir/'creator-create-intent.json').is_file():
            disk,disk_node,vm_node,expires_at=resume_creator_create_intent(args,nodes)
            pending_creator=True
        elif args.resume:
            disk,disk_node,vm_node,expires_at=resume_pending_creator(args,nodes)
            pending_creator=True
        else:
            disk_node,vm_node=select_nodes(nodes,args.disk_node,args.vm_node)
            expires_at=int(time.time())+PAUSE_TTL_SECONDS
            creator_manifest=disk_manifest('tilo-disk-'+args.acceptance_id,disk_node,
              operation=args.acceptance_id,expires_at=expires_at)
            persist_creator_create_intent(args.operation_dir,args.acceptance_id,creator_manifest)
            disk=create_or_adopt_worker(args.context,creator_manifest)
            write_private(args.operation_dir/'worker-bindings.json',
                          (json.dumps({'creator':disk},indent=2,sort_keys=True)+'\n').encode())
    if args.resume and not pending_creator:
        timer.skip('input_transfer');timer.skip('create_install');timer.skip('freeze_identity')
    else:
        auth_bytes=None
        try:
            if pending_creator:
                auth_bytes=registry_auth(args)
                status=creator_input_status(args.context,disk,args.acceptance_id,args.operation_dir)
                if status is None:
                    with timer.stage('input_transfer'):
                        stream_creator_inputs(args.context,disk['namespace'],disk['pod'],'disk',
                                              helpers,public,installer_bootc)
                elif status['state']=='adoptable':
                    # The detached controller validates the complete job binding
                    # before polling it.  Preserve every retained input and output
                    # byte until that exact adoption has completed.
                    timer.skip('input_transfer')
                else:
                    if status['state'] in ('retryable-failed','retryable-cleanup'):
                        reset_failed_creator_job(args.context,disk,args.acceptance_id,status)
                        status=dict(status,state='ready')
                    components=creator_input_components(helpers,public,installer_bootc)
                    changed=(status['state'] != 'ready' or
                             any(status['receipt'][name] != hashlib.sha256(content).hexdigest()
                                 for name,(_,content) in components.items()))
                    if not changed:
                        timer.skip('input_transfer')
                    else:
                        with timer.stage('input_transfer'):
                            stream_creator_input_delta(args.context,disk['namespace'],disk['pod'],
                                                      'disk',helpers,public,installer_bootc,status)
            else:
                with timer.stage('input_transfer'):
                    auth_bytes=registry_auth(args)
                    stream_creator_inputs(args.context,disk['namespace'],disk['pod'],'disk',
                                          helpers,public,installer_bootc)
            with timer.stage('create_install'):
                agent=host_agent(args.context,disk['node'])
                create_on_worker(root,args,disk,agent,registry_auth_bytes=auth_bytes,
                                 retry_failed=pending_creator)
                creation=(args.operation_dir/'creation.json').read_bytes()
            with timer.stage('freeze_identity'):
                freeze_raw(args.context,disk)
                identity=raw_identity(args.context,disk,operation=args.acceptance_id)
                write_private(args.operation_dir/'raw-identity.json',
                              (json.dumps(identity,indent=2,sort_keys=True)+'\n').encode())
        except ControllerTransportError:
            write_private(args.operation_dir/'creator-recovery.json',
              (json.dumps({'schema':1,'purpose':'tilo-supervised-creator-recovery',
                           'acceptance_id':args.acceptance_id,'creator':disk,
                           'state':'transport-interrupted'},indent=2,sort_keys=True)+'\n').encode())
            raise
        except CreatorControllerFailure:
            write_private(args.operation_dir/'creator-recovery.json',
              (json.dumps({'schema':1,'purpose':'tilo-supervised-creator-recovery',
                           'acceptance_id':args.acceptance_id,'creator':disk,
                           'state':'controller-failure'},indent=2,sort_keys=True)+'\n').encode())
            # The staged inputs and exact worker are the retry checkpoint.  A
            # generic controller failure has no trusted remote failure receipt,
            # so deleting the worker would turn a cheap resume into a rebuild.
            raise
        except CreatorInputReconciliationError:
            write_private(args.operation_dir/'creator-recovery.json',
              (json.dumps({'schema':1,'purpose':'tilo-supervised-creator-recovery',
                           'acceptance_id':args.acceptance_id,'creator':disk,
                           'state':'input-reconciliation-refused'},indent=2,sort_keys=True)+'\n').encode())
            raise
        except CreatorJobFailed:
            write_private(args.operation_dir/'creator-recovery.json',
              (json.dumps({'schema':1,'purpose':'tilo-supervised-creator-recovery',
                           'acceptance_id':args.acceptance_id,'creator':disk,
                           'state':'job-failed'},indent=2,sort_keys=True)+'\n').encode())
            raise
        except BaseException:
            delete_worker(args.context,disk)
            raise
        finally:
            auth_bytes=None
        checkpoint=establish_creator_checkpoint(
          args,disk,disk_node,creation,identity,expires_at)
    resumed_handoff=None
    vm_manifest_value=vm_manifest('tilo-vm-'+args.acceptance_id,vm_node)
    if args.resume and not (args.operation_dir/'vm-handoff.json').is_file() \
            and (args.operation_dir/'vm-create-intent.json').is_file():
        vm_manifest_value=load_vm_create_intent(args.operation_dir,args.acceptance_id)['manifest']
        vm=adopt_worker(args.context,vm_manifest_value)
        if vm is None:
            raise ValueError('deterministic VM create intent has no live Pod to adopt')
    if args.resume and (args.operation_dir/'vm-handoff.json').is_file():
        resumed_handoff=load_vm_handoff(args.operation_dir,args.acceptance_id,
                                        expected_identity=identity)
        vm=resumed_handoff['vm']; adopt_vm_handoff(args.context,args.operation_dir,
                                                   args.acceptance_id,identity)
        bindings_path=args.operation_dir/'worker-bindings.json'
        bindings=json.loads(bindings_path.read_text()) if bindings_path.is_file() else None
        if resumed_handoff['state']=='vm-created' and (
                bindings is None or 'vm' not in bindings):
            bindings=repair_vm_handoff_bindings(
                args.operation_dir,resumed_handoff,disk,
                resumed_handoff.get('verifier'),vm)
        if bindings is None:
            raise ValueError('VM handoff lacks worker bindings')
        if bindings.get('vm')!=vm:
            raise ValueError('VM handoff differs from worker bindings')
        verifier=resumed_handoff.get('verifier',bindings.get('verifier'))
        if verifier is None:
            raise ValueError('VM handoff lacks verifier continuation binding')
        finalization_path=args.operation_dir/'finalized-verification.json'
        finalization_verification_sha256=hashlib.sha256(finalization_path.read_bytes()).hexdigest()
        timer.skip('verifier_provision_transfer');timer.skip('verification')
    else:
        # Only now provision the verifier. It has a distinct new Kata sandbox and
        # never coexisted with the privileged image execution.
        with timer.stage('verifier_provision_transfer'):
            verifier=handoff_creator_to_verifier(root,args,nodes,checkpoint,disk_node,vm_node,
              disk,creation,identity,helpers,public)
        # One authenticated exec installs its cleanup trap before reading the
        # credential stream, then removes the file on success, error or disconnect.
        with timer.stage('verification'):
            auth_bytes=registry_auth(args)
            try:
                verification=verify_disk(args.context,verifier,values,auth_bytes,args.acceptance_id)
            finally:
                del auth_bytes
            parsed=json.loads(verification)
            if (parsed.get('purpose') != 'tilo-installed-boot-and-root-verification'
                    or not parsed.get('checks') or not all(parsed['checks'].values())):
                raise ValueError('independent installed-disk verification did not pass')
            write_private(args.operation_dir/'installed-verification.json',(json.dumps(parsed,indent=2,sort_keys=True)+'\n').encode())
            finalized=verify_finalized_source(args.context,verifier,values,json.loads(creation),args.acceptance_id)
            finalized_bytes=(json.dumps(finalized,indent=2,sort_keys=True)+'\n').encode()
            write_private(args.operation_dir/'finalized-verification.json',finalized_bytes)
            finalization_verification_sha256=hashlib.sha256(finalized_bytes).hexdigest()
    with timer.stage('vm_transfer'):
        if resumed_handoff is None:
            persist_vm_create_intent(args.operation_dir,args.acceptance_id,vm_manifest_value)
            vm=create_or_adopt_worker(args.context,vm_manifest_value)
            write_private(args.operation_dir/'worker-bindings.json',
                          (json.dumps({'creator':disk,'verifier':verifier,'vm':vm},indent=2,sort_keys=True)+'\n').encode())
            persist_vm_handoff(args.operation_dir,args.acceptance_id,'vm-created',vm,identity,verifier)
            stream_tree(args.context,vm['namespace'],vm['pod'],'qemu','/work/source',helpers)
            preflight_vm(args.context,vm)
        elif resumed_handoff['state']=='vm-created':
            stream_tree(args.context,vm['namespace'],vm['pod'],'qemu','/work/source',helpers)
            preflight_vm(args.context,vm)
        if resumed_handoff is None or resumed_handoff['state']=='vm-created':
            require_same_worker(args.context,verifier);require_same_worker(args.context,vm)
            if remote_media_present(args.context,vm):
                # A controller can die after the copy commits but before its
                # receipt.  Rehash the exact destination and adopt it; never
                # start a second writer over an existing target.
                detached_media_identity(args.context,vm,identity,args.acceptance_id,args.operation_dir)
            else:
                sparse_transfer(args.context,verifier,vm,target_container='qemu',
                                target_directory='/work/verified',target_name='installer.raw',
                                operation=args.acceptance_id,identity=identity)
                detached_media_identity(args.context,vm,identity,args.acceptance_id,args.operation_dir)
            persist_vm_handoff(args.operation_dir,args.acceptance_id,'media-ready',vm,identity)
            resumed_handoff=load_vm_handoff(args.operation_dir,args.acceptance_id,
                                             expected_identity=identity)
        else:
            current_worker=cycle_worker_identity(args.context,vm)
            load_media_ready_receipt(args.operation_dir,args.acceptance_id,identity,current_worker)
            detached_media_identity(args.context,vm,identity,args.acceptance_id,args.operation_dir)
        if resumed_handoff['state'] in {'media-ready','verifier-release-intent'}:
            persist_vm_handoff(args.operation_dir,args.acceptance_id,'verifier-release-intent',vm,identity,verifier)
            release_exact_worker(args.context,verifier)
            persist_vm_handoff(args.operation_dir,args.acceptance_id,'verifier-released',vm,identity,verifier)
    with timer.stage('conversion_cycle'):
        require_same_worker(args.context,vm)
        cycle=convert_and_cycle(args.context,vm,identity,args.acceptance_id,args.operation_dir)
    with timer.stage('final_identity'):
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
    timer.finish()
    print(json.dumps(result,sort_keys=True))


if __name__=='__main__':
    try: main()
    except (ValueError,OSError,KeyError,TypeError,subprocess.TimeoutExpired) as error:
        raise SystemExit('supervised install lane: '+str(error))
