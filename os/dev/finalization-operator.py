#!/usr/bin/env python3
"""Public finalization operations. No private-key access or inferred authorization."""
import argparse
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import time
import signing_spool as spool
import broker_signing_runner as broker


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


stage = module('operator_stage', 'stage-finalization-worker.py')
loader = module('operator_loader', 'verify-development-loader.py')
HEX = re.compile(r'[0-9a-f]{64}')
NONCE = re.compile(r'[0-9a-f]{32}')
EFI_PURPOSES = ('loader', 'live-uki', 'target-uki')
CONFIG_FIELDS = {'schema', 'request_sha256', 'helper_revision', 'archive_sha256',
                 'tools_image_id', 'hardware_profile', 'initial_free_bytes',
                 'forgejo_host', 'repository', 'dispatch_ref', 'files', 'operator_sha256'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path, limit=spool.MAX_JSON):
    return spool.snapshot(spool.signer.trusted(Path(path)), limit)


def document(path):
    return json.loads(read(path), object_pairs_hook=spool.unique)


def execute(argv, data=None, timeout=90):
    result = subprocess.run(argv, input=data, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=timeout)
    require(result.returncode == 0 and len(result.stdout) <= 2**20,
            'operator command failed; state preserved and diagnostics withheld')
    return result.stdout


def validate_config(c):
    require(set(c) == CONFIG_FIELDS and type(c['schema']) is int and c['schema'] == 1, 'operation schema differs')
    for field in ('request_sha256', 'archive_sha256', 'operator_sha256'):
        require(isinstance(c[field], str) and HEX.fullmatch(c[field]), 'invalid digest')
    require(re.fullmatch(r'[0-9a-f]{40}', c['helper_revision']), 'invalid helper revision')
    require(re.fullmatch(r'sha256:[0-9a-f]{64}', c['tools_image_id']), 'invalid tools identity')
    require(re.fullmatch(r'[a-z0-9][a-z0-9_.-]{0,127}', c['hardware_profile']), 'invalid hardware profile')
    require(re.fullmatch(r'[a-z0-9][a-z0-9.-]*', c['forgejo_host']), 'invalid Forgejo host')
    require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', c['repository']), 'invalid repository')
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_/-]*', c['dispatch_ref']), 'invalid dispatch ref')
    require(type(c['initial_free_bytes']) is int and c['initial_free_bytes'] > 0, 'invalid capacity')
    required = {'request.json', 'retained-rootfs.json'} | {
        folder + '/' + name for folder in ('original-public', 'new-public') for name in stage.PUBLIC}
    require(isinstance(c['files'], dict) and required <= c['files'].keys(), 'incomplete operation')
    for name, digest in c['files'].items():
        p = Path(name)
        require(not p.is_absolute() and '..' not in p.parts and '\\' not in name and
                p.as_posix() == name,
                'invalid snapshot path')
        require(name in required or name.startswith('helpers/os/dev/'), 'unexpected snapshot path')
        require(isinstance(digest, str) and HEX.fullmatch(digest), 'invalid snapshot digest')
    require(c['files']['request.json'] == c['request_sha256'], 'request pin differs')
    require(all('helpers/os/dev/' + name in c['files'] for name in stage.CODE), 'missing bridge helper')
    return c


def load(operation, compatible_operation_sha256=None, compatible_operator_sha256=None):
    operation = spool.private_directory(operation)
    config_raw = read(operation / 'operation.json')
    c = validate_config(json.loads(config_raw, object_pairs_hook=spool.unique))
    current_operator_sha256 = operator_identity()
    if c['operator_sha256'] != current_operator_sha256:
        require(isinstance(compatible_operation_sha256, str) and
                HEX.fullmatch(compatible_operation_sha256) is not None and
                spool.digest(config_raw) == compatible_operation_sha256 and
                isinstance(compatible_operator_sha256, str) and
                HEX.fullmatch(compatible_operator_sha256) is not None and
                compatible_operator_sha256 == current_operator_sha256,
                'operator implementation changed; use original checkout or authorize exact operation and replacement operator')
    for name, digest in c['files'].items():
        require(spool.digest(read(operation / name, 2**21)) == digest, 'operation snapshot changed')
    q = stage.driver.validate_request(document(operation / 'request.json'))
    require(q['helpers_revision'] == c['helper_revision'], 'helper request differs')
    require(c['initial_free_bytes'] >= q['minimum_free_bytes'], 'capacity below continuing floor')
    return c, q


def operator_identity():
    # Pin the public local implementation as well as the independently pinned worker helpers.
    # This does not require them to be the same revision or copy an old worker's code locally.
    files = {p.name: spool.digest(p.read_bytes()) for p in Path(__file__).parent.glob('*.py')}
    return spool.digest(spool.canonical(files))


def retained_hardware_profile(raw):
    """Return the canonical profile authenticated by the pinned retained receipt."""
    receipt = json.loads(raw, object_pairs_hook=spool.unique)
    require(isinstance(receipt, dict) and receipt.get('schema') == 1 and
            receipt.get('purpose') == 'tilo-retained-development-raw-rootfs' and
            'hardware_profile' in receipt, 'retained receipt identity differs')
    profile = receipt['hardware_profile']
    require(profile is None or isinstance(profile, str), 'invalid retained hardware profile')
    profile = 'generic' if profile is None else profile
    require(re.fullmatch(r'[a-z0-9][a-z0-9_.-]{0,127}', profile),
            'invalid retained hardware profile')
    return profile


def initialize(args):
    """Snapshot public inputs once. Existing/partial operations are never overwritten."""
    request_raw = read(args.request)
    q = stage.driver.validate_request(json.loads(request_raw, object_pairs_hook=spool.unique))
    archive_raw = read(args.helper_archive, 8 * 2**20)
    require(spool.digest(archive_raw) == args.archive_sha256, 'archive hash differs')
    files = {'request.json': request_raw, 'retained-rootfs.json': read(args.receipt, 65536)}
    require(spool.digest(files['retained-rootfs.json']) == q['receipt_sha256'], 'receipt pin differs')
    retained_profile = retained_hardware_profile(files['retained-rootfs.json'])
    require(args.hardware_profile == retained_profile,
            'operation hardware profile differs from retained rootfs')
    for folder, source in (('original-public', args.original_public), ('new-public', args.new_public)):
        for name in stage.PUBLIC:
            files[folder + '/' + name] = read(source / name, 65536)
    with tarfile.open(fileobj=io.BytesIO(archive_raw)) as archive:
        require(archive.pax_headers.get('comment') == q['helpers_revision'], 'archive revision differs')
        for member in archive.getmembers():
            if not member.name.startswith('os/dev/') or member.isdir():
                continue
            require(member.isfile() and 0 < member.size <= 2**21, 'invalid helper archive entry')
            name = 'helpers/' + member.name
            require(name not in files, 'duplicate helper entry')
            files[name] = archive.extractfile(member).read()
    c = validate_config(dict(schema=1, request_sha256=spool.digest(request_raw),
        helper_revision=q['helpers_revision'], archive_sha256=args.archive_sha256,
        operator_sha256=operator_identity(),
        tools_image_id=args.tools_image_id, hardware_profile=retained_profile,
        initial_free_bytes=args.initial_free_bytes, forgejo_host=args.forgejo_host,
        repository=args.repository, dispatch_ref=args.dispatch_ref,
        files={name: spool.digest(data) for name, data in files.items()}))
    require(c['initial_free_bytes'] >= q['minimum_free_bytes'], 'capacity below continuing floor')
    spool.signer.trusted(args.operation.parent, directory=True)
    args.operation.mkdir(mode=0o700)
    for name, data in files.items():
        path = args.operation / name
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        spool.write_new(path, data)
    # Last publication marks a complete operation. Incomplete directories remain reserved.
    spool.write_new(args.operation / 'operation.json', spool.canonical(c))
    return {'operation': str(args.operation), 'config_sha256': spool.digest(spool.canonical(c)),
            'hardware_profile': c['hardware_profile'], 'signing_authorized': False}


def api(config, endpoint):
    executable = shutil.which('fgj') or shutil.which('fgj.exe')
    require(executable is not None, 'authenticated fgj CLI must be on PATH')
    raw = execute([executable, '--hostname', config['forgejo_host'], 'api',
                   'repos/' + config['repository'] + '/actions/' + endpoint])
    return json.loads(raw, object_pairs_hook=spool.unique)


def admit_job(c, qraw, run, jobs, attempt):
    """Authenticate current event bytes; a branch name or aggregate status is insufficient."""
    require(run['workflow_id'] == 'development-finalize.yml' and
            run['trigger_event'] == 'workflow_dispatch' and
            run['commit_sha'] == c['helper_revision'], 'unexpected finalization run')
    event = json.loads(run['event_payload'], object_pairs_hook=spool.unique)
    require(event['ref'] == 'refs/heads/' + c['dispatch_ref'], 'dispatch ref differs')
    inputs = event['inputs']
    base = {'request': qraw.decode(), 'request_sha256': c['request_sha256'],
            'helpers_revision': c['helper_revision']}
    restore_fields = ('restore_run_id', 'restore_run_attempt', 'restore_receipt_sha256')
    required_fields = set(base) | {'restore_stage'}
    allowed_fields = required_fields | set(restore_fields)
    require(isinstance(inputs, dict) and required_fields <= set(inputs) <= allowed_fields
            and all(inputs.get(key) == value for key, value in base.items()), 'dispatch inputs differ')
    # Forgejo omits optional workflow-dispatch inputs whose submitted value is empty.
    # Normalize only those known fields; required inputs and unexpected keys remain authenticated.
    normalized = {**inputs, **{key: inputs.get(key, '') for key in restore_fields}}
    restore = tuple(normalized[key] for key in restore_fields)
    require(all(isinstance(value, str) for value in (*restore, normalized['restore_stage'])),
            'dispatch restore inputs differ')
    if any(restore):
        require(all(restore) and re.fullmatch(r'[1-9][0-9]*', restore[0]) is not None
                and re.fullmatch(r'[1-9][0-9]*', restore[1]) is not None
                and HEX.fullmatch(restore[2]) is not None
                and normalized['restore_stage'] in ('2', '5'), 'dispatch restore inputs differ')
    else:
        require(restore == ('', '', '') and normalized['restore_stage'] == '5',
                'fresh dispatch restore defaults differ')
    selected = [j for j in jobs if j['run_id'] == run['id'] and j['attempt'] == attempt and j['name'] == 'finalize']
    require(len(selected) == 1, 'ambiguous run attempt')
    return selected[0]


def actual_job(operation, c, run_id, attempt):
    run = api(c, f'runs/{run_id}')
    require(run['id'] == run_id, 'run ID differs')
    jobs = api(c, f'runs/{run_id}/jobs')
    return run, admit_job(c, read(operation / 'request.json'), run, jobs, attempt)


def run_path(operation, run_id, attempt):
    require(type(run_id) is int and run_id > 0 and type(attempt) is int and attempt > 0, 'actual run/attempt required')
    return operation / f'run-{run_id}-attempt-{attempt}'


def client_for(directory):
    spool.private_directory(directory)
    binding = document(directory / 'binding.json')
    context = document(directory / 'signing-context.json')
    stage.bound_context(context, binding)
    return broker.transport.Transport(broker.CONTEXT, binding, runner=broker.BrokerRunner(binding))


def admit_bound(operation, c, run_id, attempt, *, require_running=True):
    """A complete local binding and the current actual task are both required."""
    _, job = actual_job(operation, c, run_id, attempt)
    directory = run_path(operation, run_id, attempt)
    saved = document(directory / 'job.json')  # Published last by bind.
    require(all(saved[k] == job[k] for k in ('id', 'run_id', 'attempt', 'name', 'task_id')),
            'bound job identity changed')
    require(not require_running or job['status'] == 'running', 'bound job is not running')
    context = document(directory / 'signing-context.json')
    require(context['job_id'] == f"{c['repository']}/{run_id}/{attempt}/{job['task_id']}",
            'bound context job differs')
    stage.bound_context(context, document(directory / 'binding.json'))
    return directory


def packet(operation, context, c):
    names = ['retained-rootfs.json'] + [folder + '/' + name
        for folder in ('original-public', 'new-public') for name in stage.PUBLIC]
    return dict(schema=1, request_sha256=c['request_sha256'], context=context,
                files={name: base64.b64encode(read(operation / name, 65536)).decode() for name in names})


OBSERVE = r'''
import hashlib,json,os,sys
from kubernetes import client,config
from kubernetes.stream import stream
p=json.load(sys.stdin)
if set(p['auth']) != set(TARGETS):raise ValueError('broker target authorization differs')
profiles=json.loads(os.environ['BROKER_CLUSTER_TARGETS'])
if (not isinstance(profiles,list) or not all(isinstance(x,dict) and isinstance(x.get('name'),str) for x in profiles)
    or len({x['name'] for x in profiles})!=len(profiles)):raise ValueError('invalid broker target inventory')
enabled={x['name']:x for x in profiles if x.get('ci_enabled') is True}
if not enabled or not set(enabled) <= set(TARGETS):raise ValueError('unknown enabled CI target')
namespace='agent-sandboxes'
matches=[]
for target_name in sorted(enabled):
 target=TARGETS[target_name];profile=enabled[target_name];cfg=client.Configuration()
 expected=target['kubeconfig']
 if profile.get('kubeconfig')!=expected:raise ValueError('broker target path differs')
 if target['mode']=='in-cluster':
  if p['auth'][target_name] is not None:raise ValueError('in-cluster authorization differs')
  config.load_incluster_config(client_configuration=cfg)
 else:
  token=p['auth'][target_name]
  if not isinstance(token,str) or not token.startswith('k8s-aws-v1.') or len(token)>32768:
   raise ValueError('EKS authorization differs')
  config.load_kube_config(expected,client_configuration=cfg)
  cfg.refresh_api_key_hook=None;cfg.api_key['authorization']='Bearer '+token
 a=client.ApiClient(cfg);v=client.CoreV1Api(a)
 for pod in v.list_namespaced_pod(namespace,_request_timeout=15).items:
  labels=pod.metadata.labels or {};containers={c.name for c in pod.spec.containers}
  if (pod.status.phase!='Running' or labels.get('app.kubernetes.io/managed-by')!='sandbox-broker'
      or labels.get('sandbox.example/workload')!='forgejo-ci'
      or labels.get('sandbox.example/cluster')!=target_name or not {'runner','dind'}<=containers):continue
  if p.get('pod') and pod.metadata.name!=p['pod']:continue
  log=v.read_namespaced_pod_log(pod.metadata.name,namespace,container='runner',tail_lines=500,_request_timeout=10)
  if ('task '+str(p['task'])+' repo is '+p['repository']) in log:matches.append((target_name,a,v,pod))
if len(matches)!=1:raise ValueError('expected one runner matching the actual task')
target_name,a,v,pod=matches[0];name=pod.metadata.name;uid=pod.metadata.uid
def execute(command):
 w=stream(v.connect_get_namespaced_pod_exec,name,namespace,container='dind',command=command,
          stderr=True,stdin=False,stdout=True,tty=False,_preload_content=False)
 try:
  w.run_forever(timeout=20);out=w.read_stdout();status=json.loads(w.read_channel(3))
  if status['status']!='Success' or len(out)>262144:raise ValueError('bounded worker observation failed')
  return json.loads(out)
 finally:w.close()
nested=execute(['docker','inspect','--type=container','--format',p['inspect'],p['inner_name']])
if nested['Image']!=p['tools'] or not nested['State']['Running']:raise ValueError('tools worker differs')
inner=nested['Id']
observation=execute(['docker','exec',inner,'python3','-c',p['inside'],json.dumps(p['helper_files'])])
fresh=v.read_namespaced_pod(name,namespace,_request_timeout=15)
spec=a.sanitize_for_serialization(pod.spec)
freshspec=a.sanitize_for_serialization(fresh.spec)
state=next(x for x in a.sanitize_for_serialization(pod.status)['containerStatuses'] if x['name']=='dind')
freshstate=next(x for x in a.sanitize_for_serialization(fresh.status)['containerStatuses'] if x['name']=='dind')
if fresh.metadata.uid!=uid or freshspec!=spec or any(state[k]!=freshstate[k] for k in ('containerID','imageID','restartCount')):
 raise ValueError('pod identity changed during observation')
container=next(x for x in spec['containers'] if x['name']=='dind')
again=execute(['docker','inspect','--type=container','--format',p['inspect'],inner])
if again!=nested:raise ValueError('inner identity changed during observation')
print(json.dumps({'cluster':target_name,'pod':name,'pod_uid':uid,'spec_sha256':hashlib.sha256(json.dumps(spec,sort_keys=True,separators=(',',':')).encode()).hexdigest(),
 'image':container['image'],'container_id':state['containerID'],'image_id':state['imageID'],'restart_count':state['restartCount'],
 'docker':nested,'worker':observation}))
'''
OBSERVE = "TARGETS = " + repr(broker.TARGETS) + "\n" + OBSERVE

INSIDE = r'''
import hashlib,json,os,pathlib,subprocess,sys
def public(path):
 p=pathlib.Path(path);s=p.lstat()
 if p.is_symlink() or not p.is_file() or s.st_size>65536 or s.st_uid!=0:raise ValueError('invalid worker public file')
 return p.read_bytes()
directories={}
for name in ['/','/var','/var/lib','/var/lib/containers','/work']:
 p=pathlib.Path(name);s=p.lstat()
 if p.is_symlink() or not p.is_dir() or s.st_uid!=0 or s.st_mode&0o022:raise ValueError('worker directory is untrusted')
 f=os.statvfs(name)
 directories[name]={'mode':s.st_mode&0o777,'device':s.st_dev,'free_bytes':f.f_bavail*f.f_frsize}
info=json.loads(subprocess.check_output(['podman','--remote=false','info','--format=json'],timeout=15))
if info['store']['graphDriverName']!='overlay':raise ValueError('native overlay required')
if pathlib.Path('/work/operator-ready').exists() or pathlib.Path('/work/.operator-staging').exists():raise ValueError('worker is already staged')
for name,digest in json.loads(sys.argv[1]).items():
 p=pathlib.Path('/work')/name
 for parent in (p,*p.parents):
  s=parent.lstat()
  if parent.is_symlink() or s.st_uid!=0 or s.st_mode&0o022:raise ValueError('helper path is untrusted')
 if hashlib.sha256(p.read_bytes()).hexdigest()!=digest:raise ValueError('helper snapshot differs')
print(json.dumps({'request_sha256':hashlib.sha256(public('/work/finalization-request.json')).hexdigest(),
 'helpers':json.loads(public('/work/helpers/.finalization-helpers.json')),
 'source':json.loads(public('/work/source/.finalization-source.json')),
 'directories':directories,'graph_driver':info['store']['graphDriverName']}))
'''


def observe(c, job, run_id, attempt):
    # Existing broker credentials stay in memory/stdin; never exported in receipts.
    auth = {}
    for name, target in broker.TARGETS.items():
        if target['mode'] == 'in-cluster':
            auth[name] = None
            continue
        token = json.loads(execute(['runuser', '-u', 'xemmerling', '--', 'aws', 'eks', 'get-token',
            '--profile', 'tilo-eks-operator', '--region', target['region'],
            '--cluster-name', target['cluster']], timeout=30))['status']['token']
        require(isinstance(token, str) and token.startswith('k8s-aws-v1.') and len(token) <= 32768,
                'existing operator token unavailable')
        auth[name] = token
    payload = dict(auth=auth, task=job['task_id'], repository=c['repository'],
        inner_name=f'tilo-finalization-{run_id}-{attempt}', tools=c['tools_image_id'],
        inspect=broker.INSPECT, inside=INSIDE,
        helper_files={name: digest for name, digest in c['files'].items() if name.startswith('helpers/')})
    return json.loads(execute(['runuser', '-u', 'xemmerling', '--', 'kubectl',
        '--context=admin@tupo-production', '-n', 'sandbox-broker', 'exec', '-i',
        'deployment/sandbox-broker', '--', 'python', '-c', OBSERVE],
        spool.canonical(payload)))


def bind(operation, c, q, run_id, attempt):
    destination = run_path(operation, run_id, attempt)
    require(not destination.exists(), 'run already reserved; inspect existing state, never overwrite')
    run, job = actual_job(operation, c, run_id, attempt)
    require(job['status'] == 'running' and job['task_id'] > 0, 'job not running')
    observed = observe(c, job, run_id, attempt)
    worker, nested = observed['worker'], observed['docker']
    require(worker['request_sha256'] == c['request_sha256'] and
            worker['helpers'] == {'revision': c['helper_revision']} and
            worker['source'] == {'revision': q['source_revision']}, 'worker inputs differ')
    for directory in ('/work', '/var/lib', '/var/lib/containers'):
        require(worker['directories'][directory]['mode'] == 0o755 and
                worker['directories'][directory]['free_bytes'] >= c['initial_free_bytes'],
                'worker capacity or mode differs')
    context = dict(schema=1, job_id=f"{c['repository']}/{run_id}/{attempt}/{job['task_id']}",
        pod_uid=observed['pod_uid'], container_id=nested['Id'], image_id=c['tools_image_id'],
        revision=q['source_revision'], architecture=q['architecture'],
        manifest_sha256=q['new_manifest_sha256'], target=q['new_target'])
    binding = dict(schema=1, cluster=observed['cluster'], namespace='agent-sandboxes',
        pod=observed['pod'], container='dind',
        pod_uid=observed['pod_uid'], container_id=observed['container_id'], image=observed['image'],
        image_id=observed['image_id'], restart_count=observed['restart_count'],
        pod_spec_sha256=observed['spec_sha256'], context_sha256=spool.context(context),
        endpoint='/var/lib/tilo-signing-bridge/signing-spool-endpoint.py',
        docker=dict(id=nested['Id'], image_id=nested['Image'],
                    started_at=nested['State']['StartedAt'], restart_count=nested['RestartCount']))
    stage.bound_context(context, binding)
    stage.validate(packet(operation, context, c), read(operation / 'request.json'))
    # Recheck the job after observing the worker, before reserving any local state.
    _, fresh = actual_job(operation, c, run_id, attempt)
    require(fresh == job, 'job changed during binding')
    destination.mkdir(mode=0o700)
    for name in ('received', 'responses', 'checkpoints'):
        (destination / name).mkdir(mode=0o700)
    for name, value in (('binding.json', binding), ('signing-context.json', context),
                        ('observation.json', observed), ('job.json', job)):
        spool.write_new(destination / name, spool.canonical(value))
    return {'run_id': run_id, 'attempt': attempt, 'run_number': run['index_in_repo'],
            'task_id': job['task_id'], 'directory': str(destination), 'bound': True}


def stage_run(operation, c, directory):
    require(not (directory / 'stage-started.json').exists(), 'staging reserved; no automatic retry')
    client = client_for(directory)
    context = document(directory / 'signing-context.json')
    spool.write_new(directory / 'stage-started.json', spool.canonical({'request_sha256': c['request_sha256']}))
    result = stage.stage(client, packet(operation, context, c), read(operation / 'request.json'))
    spool.write_new(directory / 'staging-receipt.json', spool.canonical(result))
    return result


def status(operation, c, run_id, attempt):
    run, job = actual_job(operation, c, run_id, attempt)
    directory = run_path(operation, run_id, attempt)
    result = {'run_id': run_id, 'attempt': attempt, 'run_status': run['status'],
              'job_status': job['status'], 'task_id': job['task_id'],
              'bound': directory.exists(), 'requests': []}
    if directory.exists():
        require(document(directory / 'job.json')['task_id'] == job['task_id'], 'bound job task changed')
        result['staged'] = (directory / 'staging-receipt.json').exists()
        if result['staged'] and job['status'] == 'running':
            client = client_for(directory)
            try:
                raw_pending = client.transfer('list')
            except Exception:
                # A terminal runner can disappear between the job observation and
                # spool transfer. Re-read once and accept only the same terminal job.
                fresh_run, fresh_job = actual_job(operation, c, run_id, attempt)
                require(all(fresh_job[key] == job[key]
                            for key in ('id', 'run_id', 'attempt', 'name', 'task_id')),
                        'bound job identity changed during terminal recheck')
                if fresh_job['status'] in ('success', 'failure', 'cancelled', 'skipped'):
                    result.update(run_status=fresh_run['status'], job_status=fresh_job['status'], requests=[])
                    return result
                raise
            pending = json.loads(raw_pending, object_pairs_hook=spool.unique)
            require(set(pending) == {'requests'} and isinstance(pending['requests'], list) and
                    len(pending['requests']) <= 64 and all(isinstance(n, str) and NONCE.fullmatch(n)
                    for n in pending['requests']), 'invalid pending request list')
            result['requests'] = pending['requests']
    return result


def wait_status(operation, c, run_id, attempt, until, timeout, interval, ignore=()):
    require(1 <= timeout <= 3600 and 1 <= interval <= 60, 'invalid observation bounds')
    deadline = time.monotonic() + timeout
    while True:
        result = status(operation, c, run_id, attempt)
        result['requests'] = [n for n in result['requests'] if n not in ignore]
        if (until == 'running' and result['job_status'] == 'running') or (
                until == 'request' and result['requests']) or result['job_status'] in ('success', 'failure', 'cancelled', 'skipped'):
            return result
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('observation expired; same run remains selected, no restart authorized')
        time.sleep(min(interval, remaining))


def reuse_efi(purpose, old_context, new_context, old_request, request, old_response,
              original, incoming, signed, expected_input_sha256):
    """Pure admission for an exact checkpoint-selected EFI response."""
    require(purpose in EFI_PURPOSES, 'unsupported EFI reuse purpose')
    old_request, request = spool.request_metadata(old_request), spool.request_metadata(request)
    require(old_request['context_sha256'] == spool.context(old_context) and
            request['context_sha256'] == spool.context(new_context), 'request context differs')
    immutable = ('schema', 'revision', 'architecture', 'manifest_sha256', 'target', 'image_id')
    require(all(old_context[k] == new_context[k] for k in immutable), 'retained immutable context differs')
    require(HEX.fullmatch(expected_input_sha256) is not None, 'checkpoint EFI digest required')
    require(original == incoming and spool.digest(original) == expected_input_sha256,
            'checkpoint-selected EFI bytes differ')
    for metadata in (old_request, request):
        require(metadata['operation'] == 'sign-efi' and metadata['input_sha256'] == expected_input_sha256
                and metadata['input_bytes'] == len(original), 'request is not selected EFI input')
    fields = {'schema', 'request_id', 'context_sha256', 'operation', 'input_sha256', 'output_sha256', 'output_bytes'}
    require(set(old_response) == fields and all(old_response[k] == old_request[k]
            for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')),
            'retained response context differs')
    require(type(old_response['output_bytes']) is int and old_response['output_bytes'] == len(signed)
            and old_response['output_sha256'] == spool.digest(signed), 'retained output bytes differ')
    return dict(old_response, request_id=request['request_id'], context_sha256=request['context_sha256'])


def reuse_loader(old_context, new_context, old_request, request, old_response,
                 original, incoming, signed, expected_input_sha256):
    """Compatibility wrapper for pure loader admission."""
    return reuse_efi('loader', old_context, new_context, old_request, request, old_response,
                     original, incoming, signed, expected_input_sha256)


def checkpoint_input(directory, purpose, context, request, incoming):
    """Revalidate a materialized checkpoint against its exact fetched request."""
    checkpoint = spool.private_directory(directory / 'checkpoints' / purpose)
    receipt = document(checkpoint / 'receipt.json')
    materialization = document(checkpoint / 'materialization.json')
    selected = read(checkpoint / 'input.efi', spool.MAX_INPUT)
    context_sha256 = spool.context(context)
    expected_materialization = {
        'schema': 1, 'purpose': purpose, 'request_id': request['request_id'],
        'context_sha256': context_sha256, 'input_sha256': request['input_sha256'],
        'input_bytes': request['input_bytes'], 'signing_authorized': False,
    }
    require(receipt.get('purpose') == purpose and
            receipt.get('transport_context_sha256') == context_sha256 and
            receipt.get('signing_authorized') is False and
            receipt.get('private_keys_used') is False and
            receipt.get('input_sha256') == request['input_sha256'] and
            receipt.get('input_bytes') == request['input_bytes'],
            'authenticated checkpoint receipt differs')
    require(materialization == expected_materialization and selected == incoming and
            len(selected) == request['input_bytes'] and
            spool.digest(selected) == request['input_sha256'],
            'checkpoint materialization differs from fetched input')
    return request['input_sha256']


def reuse_response(operation, directory, nonce, old_directory, old_nonce, purpose):
    for value in (nonce, old_nonce):
        require(NONCE.fullmatch(value), 'invalid request ID')
    received, old_received = directory / 'received' / nonce, old_directory / 'received' / old_nonce
    old_response_dir = old_directory / 'responses' / old_nonce
    new_context, old_context = document(directory / 'signing-context.json'), document(old_directory / 'signing-context.json')
    incoming = read(received / 'input', spool.MAX_INPUT)
    original = read(old_received / 'input', spool.MAX_INPUT)
    signed = read(old_response_dir / 'output', spool.MAX_INPUT + 65536)
    old_request = spool.request_metadata(document(old_received / 'request.json'))
    request = spool.request_metadata(document(received / 'request.json'))
    old_checkpoint_sha = checkpoint_input(old_directory, purpose, old_context, old_request, original)
    checkpoint_sha = checkpoint_input(directory, purpose, new_context, request, incoming)
    require(old_checkpoint_sha == checkpoint_sha, 'checkpoint input identity differs across runs')
    response = reuse_efi(purpose, old_context, new_context, old_request, request,
        document(old_response_dir / 'response.json'), original, incoming, signed, checkpoint_sha)
    require(response['request_id'] == nonce and
            document(old_received / 'request.json')['request_id'] == old_nonce, 'request directory differs')
    q = document(operation / 'request.json')
    require(all(new_context[k] == q[f] for k, f in (
        ('revision', 'source_revision'), ('architecture', 'architecture'),
        ('manifest_sha256', 'new_manifest_sha256'), ('target', 'new_target'))), 'operation context differs')
    stage.bound_context(new_context, document(directory / 'binding.json'))
    trust = stage.trust.validate(operation / 'new-public', q['new_manifest_sha256'])
    # Existing native crypto verifies certificate, signature and preserved PE envelope.
    verified = loader.verify(received / 'input', old_response_dir / 'output', operation / 'new-public/efi-signing.crt',
                             trust['identities']['efi-signing.crt'], new_context['architecture'],
                             spool.MAX_INPUT)
    require(verified['original_sha256'] == spool.digest(incoming) and
            verified['signed_sha256'] == spool.digest(signed) and
            verified['certificate_sha256'] == trust['identities']['efi-signing.crt'] and
            verified['architecture'] == new_context['architecture'], 'EFI response changed during verification')
    destination = directory / 'responses' / nonce
    destination.mkdir(mode=0o700)
    spool.write_new(destination / 'output', signed)
    spool.write_new(destination / 'response.json', spool.canonical(response))
    return {'request_id': nonce, 'purpose': purpose, 'signature_reused': True, 'private_key_used': False,
            'output_sha256': response['output_sha256'], 'directory': str(destination)}


def service_reused_efi(operation, c, directory, run_id, attempt, old_directory,
                       old_nonce, purpose, timeout, interval, ignore=()):
    """Wait for, fetch, verify, reuse, and deliver one exact EFI request."""
    observed = wait_status(
        operation, c, run_id, attempt, 'request', timeout, interval, ignore)
    require(observed['job_status'] == 'running' and len(observed['requests']) == 1,
            'expected exactly one pending EFI request from a running job')
    nonce = observed['requests'][0]
    received = directory / 'received' / nonce
    response = directory / 'responses' / nonce
    require(not received.exists() and not response.exists(),
            'EFI service state already exists; inspect it before continuing')
    client = client_for(directory)
    fetched = client.fetch(nonce, received)
    import checkpoint_export
    materialized = checkpoint_export.materialize(
        operation, directory, purpose, received, spool)
    reused = reuse_response(operation, directory, nonce, old_directory, old_nonce, purpose)
    delivered = client.deliver(response)
    require(fetched.get('request_id') == nonce and materialized.get('request_id') == nonce
            and materialized.get('input_sha256') == fetched.get('input_sha256')
            and reused.get('request_id') == nonce
            and delivered.get('request_id') == nonce
            and reused.get('output_sha256') == delivered.get('output_sha256'),
            'EFI service result identity differs')
    return {'request_id': nonce, 'input_sha256': fetched['input_sha256'],
            'output_sha256': delivered['output_sha256'], 'purpose': purpose, 'signature_reused': True,
            'private_key_used': False, 'delivered': True}


def handoff(operation, directory, c):
    context = document(directory / 'signing-context.json')
    stage.bound_context(context, document(directory / 'binding.json'))
    received = spool.private_directory(directory / 'received')
    responses = spool.private_directory(directory / 'responses')
    checkpoints = spool.private_directory(directory / 'checkpoints')
    return dict(operation=str(operation), run_directory=str(directory),
        request=str(operation / 'request.json'), binding=str(directory / 'binding.json'),
        signing_context=str(directory / 'signing-context.json'),
        context_sha256=spool.context(context), hardware_profile=c['hardware_profile'],
        architecture=context['architecture'], received=str(received),
        responses=str(responses), checkpoints=str(checkpoints),
        next_step='Authenticate checkpoint input before authorize-signing-input.py; then service-local-signing-request.py and deliver.',
        signing_authorized=False)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--operation', type=Path, required=True)
    ap.add_argument('--compatible-operation-sha256',
                    help='authorize the exact immutable operation config with reviewed newer operator code')
    ap.add_argument('--compatible-operator-sha256',
                    help='pin the exact reviewed replacement operator implementation')
    sub = ap.add_subparsers(dest='command', required=True)
    for name, help_text, purposes in (
            ('diagnose-checkpoint', 'read-only checkpoint diagnostics', ('loader', 'live-uki', 'target-uki')),
            ('export-checkpoint', 'publish authenticated checkpoint metadata without large EFI transport', ('loader', 'live-uki', 'target-uki')),
            ('materialize-checkpoint', 'match a fetched signing input to checkpoint metadata', EFI_PURPOSES)):
        p = sub.add_parser(name, help=help_text)
        p.add_argument('--purpose', choices=purposes, required=True)
        p.add_argument('--run-id', type=int)
        p.add_argument('--attempt', type=int)
        p.add_argument('--legacy-layout', action='store_true',
                       help='use the explicitly selected pre-operator operation layout')
        p.add_argument('--run-directory', type=Path,
                       help='legacy compatibility only; generic paths derive from run ID and attempt')
        if name == 'materialize-checkpoint':
            p.add_argument('--request-directory', type=Path, required=True)
    init = sub.add_parser('init')
    for name in ('request', 'helper-archive', 'receipt', 'original-public', 'new-public'):
        init.add_argument('--' + name, type=Path, required=True)
    for name in ('archive-sha256', 'tools-image-id', 'hardware-profile', 'forgejo-host', 'repository', 'dispatch-ref'):
        init.add_argument('--' + name, required=True)
    init.add_argument('--initial-free-bytes', type=int, required=True)
    for command in ('bind', 'stage', 'status', 'wait', 'fetch', 'reuse-efi',
                    'service-reused-efi', 'deliver', 'handoff'):
        p = sub.add_parser(command)
        p.add_argument('--run-id', type=int, required=True)
        p.add_argument('--attempt', type=int, required=True)
        if command in ('fetch', 'reuse-efi', 'deliver'):
            p.add_argument('--request-id', required=True)
        if command in ('wait', 'service-reused-efi'):
            if command == 'wait':
                p.add_argument('--until', choices=('running', 'request'), required=True)
            p.add_argument('--timeout', type=int, default=300)
            p.add_argument('--interval', type=int, default=30)
            p.add_argument('--ignore-request', action='append', default=[])
        if command in ('reuse-efi', 'service-reused-efi'):
            p.add_argument('--old-run-directory', type=Path, required=True)
            p.add_argument('--old-request-id', required=True)
            p.add_argument('--purpose', choices=EFI_PURPOSES, required=True)
    a = ap.parse_args()
    require(os.geteuid() == 0, 'trusted local root operation required')
    if a.command in ('diagnose-checkpoint', 'export-checkpoint', 'materialize-checkpoint'):
        import checkpoint_export
        if a.legacy_layout:
            require(a.run_directory is not None and a.run_id is None and a.attempt is None,
                    'legacy checkpoint layout requires only --run-directory')
            directory = a.run_directory
        else:
            require(a.run_directory is None and a.run_id is not None and a.attempt is not None,
                    'generic checkpoint layout requires only --run-id and --attempt')
            c, _ = load(a.operation, a.compatible_operation_sha256,
                        a.compatible_operator_sha256)
            directory = admit_bound(a.operation, c, a.run_id, a.attempt)
        if a.command == 'diagnose-checkpoint':
            result = checkpoint_export.diagnose(
                a.operation, directory, a.purpose, spool, legacy=a.legacy_layout)
        elif a.command == 'export-checkpoint':
            result = checkpoint_export.export_metadata(
                a.operation, directory, a.purpose, spool, legacy=a.legacy_layout)
        else:
            result = checkpoint_export.materialize(
                a.operation, directory, a.purpose, a.request_directory, spool,
                legacy=a.legacy_layout)
    elif a.command == 'init':
        require(a.compatible_operation_sha256 is None and
                a.compatible_operator_sha256 is None,
                'operator compatibility applies only to an existing operation')
        result = initialize(a)
    else:
        c, q = load(a.operation, a.compatible_operation_sha256,
                    a.compatible_operator_sha256)
        directory = run_path(a.operation, a.run_id, a.attempt)
        if a.command not in ('bind', 'status', 'wait'):
            directory = admit_bound(a.operation, c, a.run_id, a.attempt,
                                    require_running=a.command != 'handoff')
        if a.command == 'bind': result = bind(a.operation, c, q, a.run_id, a.attempt)
        elif a.command == 'stage': result = stage_run(a.operation, c, directory)
        elif a.command == 'status': result = status(a.operation, c, a.run_id, a.attempt)
        elif a.command == 'wait': result = wait_status(a.operation, c, a.run_id, a.attempt, a.until, a.timeout, a.interval, a.ignore_request)
        elif a.command == 'service-reused-efi':
            require(NONCE.fullmatch(a.old_request_id), 'invalid old request ID')
            result = service_reused_efi(a.operation, c, directory, a.run_id, a.attempt,
                a.old_run_directory, a.old_request_id, a.purpose,
                a.timeout, a.interval, a.ignore_request)
        elif a.command == 'handoff': result = handoff(a.operation, directory, c)
        else:
            require(NONCE.fullmatch(a.request_id), 'invalid request ID')
            if a.command == 'fetch': result = client_for(directory).fetch(a.request_id, directory / 'received' / a.request_id)
            elif a.command == 'deliver': result = client_for(directory).deliver(directory / 'responses' / a.request_id)
            else: result = reuse_response(a.operation, directory, a.request_id, a.old_run_directory,
                                         a.old_request_id, a.purpose)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    main()
