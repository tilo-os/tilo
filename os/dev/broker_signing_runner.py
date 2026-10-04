"""Existing authenticated EKS broker route for the signing Transport runner API.

Only fixed transport/staging commands are admitted. Tokens stay in transient
stdin, never argv, files or diagnostics. Public payloads use bounded file/stream
I/O rather than JSON/base64 buffering. No new credentials or service is created.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import tempfile
import threading
import time
import signing_spool as spool

SPEC = importlib.util.spec_from_file_location('broker_transport', Path(__file__).with_name('transfer-signing-spool.py'))
transport = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(transport)
CONTEXT = 'broker-authenticated-api'
MAX_STREAM = spool.MAX_INPUT + 65536
INSPECT = '{"Id":{{json .Id}},"Image":{{json .Image}},"State":{{json .State}},"RestartCount":{{json .RestartCount}}}'
TARGETS = {
    'on-prem': {'mode': 'in-cluster', 'kubeconfig': None, 'region': None, 'cluster': None},
    'aws-eu-central-1': {'mode': 'eks', 'kubeconfig': '/etc/sandbox-broker/clusters/eks.yaml',
                         'region': 'eu-central-1', 'cluster': 'podesta-agent-burst'},
    'aws-us-west-2': {'mode': 'eks', 'kubeconfig': '/etc/sandbox-broker/clusters/eks-us-west-2.yaml',
                      'region': 'us-west-2', 'cluster': 'podesta-burst-usw2'},
}

# Watchdog runs in the pinned worker even if the API connection disappears.
REMOTE_WRAPPER = r'''
import json,os,select,signal,subprocess,sys,tempfile,time
remaining=int(sys.argv[1]);deadline=time.monotonic()+float(sys.argv[2])
command=json.loads(sys.argv[3]);child=None
with tempfile.TemporaryFile() as payload:
 try:
  while remaining:
   budget=deadline-time.monotonic()
   if budget<=0 or not select.select([sys.stdin.fileno()],[],[],budget)[0]:
    raise TimeoutError('remote input deadline')
   data=os.read(sys.stdin.fileno(),min(remaining,65536))
   if not data: raise ValueError('short input')
   payload.write(data);remaining-=len(data)
  payload.seek(0)
  budget=deadline-time.monotonic()
  if budget<=0: raise TimeoutError('remote operation deadline')
  child=subprocess.Popen(command,stdin=payload,stderr=subprocess.DEVNULL,start_new_session=True)
  result=child.wait(timeout=budget)
 finally:
  # The leader may already have exited while descendants remain.
  if child is not None:
   try: os.killpg(child.pid,signal.SIGKILL)
   except ProcessLookupError: pass
   child.wait()
sys.exit(result)
'''

# Runs in the existing broker. No command is derived from remote output.
BRIDGE = r'''
import json,os,sys,time
from kubernetes import client,config
from kubernetes.client.exceptions import ApiException
from kubernetes.stream import stream
line=sys.stdin.buffer.readline(262145)
if len(line)>262144 or not line.endswith(b'\n'): raise ValueError('header bound')
p=json.loads(line)
deadline=time.monotonic()+p['timeout']
profiles=json.loads(os.environ['BROKER_CLUSTER_TARGETS'])
if (not isinstance(profiles,list) or not all(isinstance(x,dict) and isinstance(x.get('name'),str) for x in profiles)
    or len({x['name'] for x in profiles})!=len(profiles)): raise ValueError('invalid broker target inventory')
enabled={x['name']:x for x in profiles if x.get('ci_enabled') is True}
if not enabled or not set(enabled) <= set(TARGETS): raise ValueError('unknown enabled CI target')
selected=p.get('target')
if selected not in enabled or selected not in TARGETS or set(p['auth']) != {selected}:
 raise ValueError('pinned broker target differs')
candidates={selected:enabled[selected]}
matches=[]
for name in sorted(candidates):
 target=TARGETS[name];profile=candidates[name];cfg=client.Configuration()
 expected=target['kubeconfig']
 if profile.get('kubeconfig') != expected: raise ValueError('broker target path differs')
 if target['mode']=='in-cluster':
  if p['auth'][name] is not None: raise ValueError('in-cluster authorization differs')
  config.load_incluster_config(client_configuration=cfg)
 else:
  token=p['auth'][name]
  if not isinstance(token,str) or not token.startswith('k8s-aws-v1.') or len(token)>32768:
   raise ValueError('EKS authorization differs')
  config.load_kube_config(expected,client_configuration=cfg)
  cfg.refresh_api_key_hook=None;cfg.api_key['authorization']='Bearer '+token
 a=client.ApiClient(cfg);v=client.CoreV1Api(a)
 try: pod=v.read_namespaced_pod(p['pod'],p['namespace'],_request_timeout=min(15,p['timeout']))
 except ApiException as error:
  if error.status==404: continue
  raise
 labels=pod.metadata.labels or {};containers={x.name for x in pod.spec.containers}
 if (pod.metadata.uid==p['uid'] and pod.status.phase=='Running'
     and labels.get('app.kubernetes.io/managed-by')=='sandbox-broker'
     and labels.get('podesta.ai/workload')=='forgejo-ci'
     and labels.get('podesta.ai/cluster')==name and {'runner','dind'} <= containers):
  matches.append((name,a,v,pod))
if len(matches)!=1: raise ValueError('pinned pod must match exactly one enabled CI target')
target_name,a,v,pod=matches[0]
if p['operation']=='get':
 data=json.dumps(a.sanitize_for_serialization(pod)).encode()
 if len(data)>p['limit']: raise ValueError('output bound')
 sys.stdout.buffer.write(data)
else:
 command=p['command']
 # Read-only Docker inspect needs no Python in DinD. All worker endpoint
 # operations, including zero-input publication, receive their own watchdog.
 prefix=[]
 if command[:3]==['docker','exec','-i']:
  prefix,command=command[:4],command[4:]
 if command[:1]==['/usr/bin/python3']:
  command=prefix+['/usr/bin/python3','-c',REMOTE_WRAPPER,str(p['input_bytes']),
                  str(max(0.01,deadline-time.monotonic())),json.dumps(command)]
 else:
  command=prefix+command
 w=stream(v.connect_get_namespaced_pod_exec,p['pod'],p['namespace'],container=p['container'],
          command=command,stderr=True,stdin=bool(p['input_bytes']),stdout=True,tty=False,
          _preload_content=False,binary=True,_request_timeout=min(15,p['timeout']))
 count=errors=0;remaining=p['input_bytes'];status=b''
 try:
  while w.is_open():
   if time.monotonic()>=deadline: raise ValueError('exec deadline')
   if remaining:
    data=sys.stdin.buffer.read(min(65536,remaining))
    if not data: raise ValueError('short input')
    w.write_stdin(data);remaining-=len(data)
   w.update(timeout=0 if remaining else min(1,max(0.01,deadline-time.monotonic())))
   while w.peek_stdout():
    data=w.read_stdout();count+=len(data)
    if count>p['limit']: raise ValueError('output bound')
    sys.stdout.buffer.write(data);sys.stdout.buffer.flush()
   while w.peek_stderr():
    errors+=len(w.read_stderr())
    if errors>65536: raise ValueError('error output bound')
   if w.peek_channel(3): status+=w.read_channel(3)
   if len(status)>65536: raise ValueError('status bound')
  if remaining or not status or json.loads(status).get('status')!='Success':
   raise ValueError('remote operation failed')
 finally: w.close()
'''
BRIDGE = "REMOTE_WRAPPER = " + repr(REMOTE_WRAPPER) + "\nTARGETS = " + repr(TARGETS) + "\n" + BRIDGE


class BrokerRunner:
    def __init__(self, binding, *, local_user=None, kube_context=None, process=transport.bounded_process):
        self.binding = transport.validate_binding(binding)
        self.target = self.binding.get('cluster')
        if self.target not in TARGETS:
            raise ValueError('invalid pinned broker target')
        # The operator account and its kubeconfig context are per-machine;
        # they come from the caller or the environment, never from this file.
        local_user = local_user or os.environ.get('TILO_BROKER_OPERATOR_USER', '')
        if not re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', local_user):
            raise ValueError('set TILO_BROKER_OPERATOR_USER to the existing operator user')
        kube_context = kube_context or os.environ.get('TILO_BROKER_KUBE_CONTEXT', '')
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9@._:/-]{0,252}', kube_context):
            raise ValueError('set TILO_BROKER_KUBE_CONTEXT to the operator kubeconfig context')
        self.user = local_user
        self.kube_context = kube_context
        self.process = process

    def operation(self, argv):
        b = self.binding
        prefix = ['kubectl', '--context=' + CONTEXT, '--namespace=' + b['namespace']]
        if argv == prefix + ['get', 'pod', b['pod'], '-o', 'json', '--request-timeout=15s']:
            return 'get', None
        head = prefix + ['exec', '-i', b['pod'], '-c', b['container'], '--']
        if argv[:len(head)] != head:
            raise ValueError('unexpected authenticated transport command')
        command = argv[len(head):]
        if 'docker' in b:
            if command == ['docker', 'inspect', '--type=container', '--format', INSPECT, b['docker']['id']]:
                return 'exec', command
            nested = ['docker', 'exec', '-i', b['docker']['id']]
            if command[:len(nested)] != nested:
                raise ValueError('nested worker selection differs')
            tail = command[len(nested):]
            if tail in ([ '/usr/bin/python3', '/work/helpers/os/dev/stage-finalization-worker.py', op]
                        for op in ('receive', 'release')):
                return 'exec', command
        else:
            tail = command
        if tail[:2] != ['/usr/bin/python3', b['endpoint']] or len(tail) < 3:
            raise ValueError('unexpected remote endpoint')
        op, rest = tail[2], tail[3:]
        if op == 'list' and not rest:
            return 'exec', command
        if op not in ('request', 'input', 'put-output', 'publish-response') or len(rest) < 2:
            raise ValueError('unexpected signing endpoint operation')
        if rest[0] != '--request-id' or not re.fullmatch(r'[0-9a-f]{32}', rest[1]):
            raise ValueError('invalid fixed request selector')
        if op in ('request', 'input') and len(rest) == 2:
            return 'exec', command
        if op in ('put-output', 'publish-response') and len(rest) == 4 and rest[2] == '--response':
            if len(rest[3]) > spool.MAX_JSON or not isinstance(json.loads(rest[3], object_pairs_hook=spool.unique), dict):
                raise ValueError('invalid bounded response metadata')
            return 'exec', command
        raise ValueError('unexpected endpoint arguments')

    def __call__(self, argv, sink, limit, *, source=None, timeout=120):
        operation, command = self.operation(argv)
        if type(limit) is not int or not 0 < limit <= MAX_STREAM or not 0 < timeout <= 1800:
            raise ValueError('invalid broker operation bounds')
        deadline = time.monotonic() + timeout
        # Seekable public input can feed the broker directly once its remaining
        # size is admitted. Only non-seekable input needs an anonymous staging
        # file. The token header always stays in the pipe.
        with contextlib.ExitStack() as resources:
            count = 0
            public = io.BytesIO()
            public_start = 0
            if source is not None and getattr(source, 'seekable', lambda: False)():
                start = source.tell()
                end = source.seek(0, os.SEEK_END)
                source.seek(start)
                count = end - start
                if not 0 <= count <= MAX_STREAM:
                    raise ValueError('broker input exceeds admission')
                public = source
                public_start = start
            elif source is not None:
                public = resources.enter_context(tempfile.TemporaryFile())
                while True:
                    data = source.read(min(65536, MAX_STREAM + 1 - count))
                    if not data: break
                    count += len(data)
                    if count > MAX_STREAM or time.monotonic() >= deadline:
                        raise ValueError('broker input exceeds admission')
                    public.write(data)
            targets = {self.target: TARGETS[self.target]}
            auth = {}
            for name, target in targets.items():
                if target['mode'] == 'in-cluster':
                    auth[name] = None
                    continue
                token_output = io.BytesIO()
                self.process(['runuser', '-u', self.user, '--', 'aws', 'eks', 'get-token', '--profile',
                    'tilo-eks-operator', '--region', target['region'], '--cluster-name', target['cluster']],
                    token_output, 65536, timeout=min(30, max(0.01, deadline-time.monotonic())))
                token = json.loads(token_output.getvalue())['status']['token']
                if not isinstance(token, str) or not token.startswith('k8s-aws-v1.') or len(token) > 32768:
                    raise ValueError('existing operator token unavailable')
                auth[name] = token
            remaining = deadline-time.monotonic()
            if remaining <= 0: raise ValueError('broker operation deadline')
            header = {'auth': auth, 'target': self.target, 'operation': operation, 'command': command,
                'pod': self.binding['pod'], 'namespace': self.binding['namespace'],
                'uid': self.binding['pod_uid'], 'container': self.binding['container'],
                'input_bytes': count, 'limit': limit, 'timeout': remaining}
            header_raw = spool.canonical(header) + b'\n'
            if len(header_raw) > 262144:
                raise ValueError('broker header exceeds admission')
            public.seek(public_start)
            reader, writer = os.pipe()
            failures = []
            def feed():
                try:
                    with os.fdopen(writer, 'wb') as stream:
                        stream.write(header_raw)
                        remaining = count
                        while remaining:
                            chunk = public.read(min(65536, remaining))
                            if not chunk:
                                raise ValueError('broker input shortened during transfer')
                            stream.write(chunk)
                            remaining -= len(chunk)
                        if public.read(1):
                            raise ValueError('broker input grew during transfer')
                except (OSError, ValueError):
                    failures.append(True)
            thread = threading.Thread(target=feed, daemon=True)
            thread.start()
            try:
                with os.fdopen(reader, 'rb') as payload:
                    result = self.process(['runuser', '-u', self.user, '--', 'kubectl', '--context=' + self.kube_context, '-n', 'sandbox-broker',
                        'exec', '-i', 'deployment/sandbox-broker', '--', 'python', '-c', BRIDGE],
                        sink, limit, source=payload, timeout=max(0.01, deadline-time.monotonic()))
                thread.join(timeout=1)
                if thread.is_alive() or failures:
                    raise ValueError('broker input stream failed')
                return result
            finally:
                # Closing the read end releases a blocked feeder after command
                # failure; bounded_process also kills its owned process group.
                thread.join(timeout=1)
