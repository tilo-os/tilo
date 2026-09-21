import io
import json
import os
from pathlib import Path
import subprocess
import sys
import types
import tempfile
import time
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import broker_signing_runner as b


def binding():
    return {'schema': 1, 'cluster': 'aws-us-west-2', 'namespace': 'test',
        'pod': 'runner', 'container': 'dind', 'pod_uid': 'uid',
        'container_id': 'containerd://abc', 'image': 'image:fixture', 'image_id': 'sha256:abc',
        'restart_count': 0, 'pod_spec_sha256': 'a'*64, 'context_sha256': 'b'*64,
        'endpoint': '/var/lib/tilo-signing-bridge/signing-spool-endpoint.py',
        'docker': {'id': 'c'*64, 'image_id': 'sha256:'+'d'*64, 'started_at': 'fixture', 'restart_count': 0}}


class BrokerTests(unittest.TestCase):
    def test_strict_identity_command_allowlist(self):
        unpinned = binding()
        unpinned.pop('cluster')
        with self.assertRaisesRegex(ValueError, 'pinned broker target'):
            b.BrokerRunner(unpinned)
        runner = b.BrokerRunner(binding())
        client = b.transport.Transport(b.CONTEXT, binding(), runner)
        valid = client.exec_prefix() + ['docker', 'exec', '-i', 'c'*64, '/usr/bin/python3',
            '/work/helpers/os/dev/stage-finalization-worker.py', 'receive']
        self.assertEqual(runner.operation(valid)[0], 'exec')
        for bad in (valid[:-1]+['unknown'], ['sh','-c','true'],
                    [x.replace('c'*64, 'e'*64) for x in valid],
                    [x.replace('broker-authenticated-api','other') for x in valid]):
            with self.assertRaises(ValueError): runner.operation(bad)

    def test_public_input_and_transient_token_stream_with_bound_failure(self):
        events = []
        token_commands = []
        def process(argv, sink, limit, source=None, timeout=120):
            self.assertGreater(timeout, 0)
            self.assertLessEqual(timeout, 30)
            self.assertNotIn('k8s-aws-v1.fixture', str(argv))
            if 'aws' in argv:
                token_commands.append(argv)
                sink.write(json.dumps({'status': {'token': 'k8s-aws-v1.' + argv[argv.index('--region') + 1]}}).encode())
                return 42
            header = json.loads(source.readline())
            self.assertIn('--context=admin@tupo-production', argv)
            data = source.read()
            events.append((header,data))
            self.assertEqual(set(header['auth']), {'aws-us-west-2'})
            self.assertEqual(header['target'], 'aws-us-west-2')
            self.assertEqual(header['container'], 'dind')
            self.assertEqual(header['input_bytes'], len(data))
            sink.write(b'public reply')
            return 12
        runner = b.BrokerRunner(binding(), process=process)
        client = b.transport.Transport(b.CONTEXT, binding(), runner)
        argv = client.exec_prefix()+['docker','exec','-i','c'*64,'/usr/bin/python3',
            '/work/helpers/os/dev/stage-finalization-worker.py','receive']
        output = io.BytesIO()
        data = bytes(range(256))*1024
        class SeekableSource(io.BytesIO):
            def read(self, *args):
                self.assert_token()
                return super().read(*args)
            def assert_token(self):
                if len(token_commands) != 1:
                    raise AssertionError('seekable input was staged before token acquisition')
        runner(argv, output, 65536, source=SeekableSource(data), timeout=30)
        self.assertEqual({(x[x.index('--region') + 1], x[x.index('--cluster-name') + 1]) for x in token_commands},
                         {('us-west-2', 'burst-us')})
        self.assertEqual(events[0][1], data)
        self.assertEqual(output.getvalue(), b'public reply')
        with patch.object(b, 'MAX_STREAM', 10), self.assertRaises(ValueError):
            runner(argv, io.BytesIO(), 10, source=io.BytesIO(b'12345678901'))
        def failed(*args, **kwargs): raise ValueError('bounded process failure')
        runner.process = failed
        with self.assertRaises(ValueError): runner(argv, io.BytesIO(), 10)

    def bridge(self, data, *, fail=False, limit=4096, nested=False, profiles=None,
               target='aws-us-west-2'):
        """Exercise actual embedded EOF wrapper and stream drain protocol."""
        header = {'auth':{'aws-us-west-2':'k8s-aws-v1.west'}, 'target':target,
            'pod':'runner','namespace':'test','uid':'uid','container':'dind',
            'operation':'exec','command':['/usr/bin/python3','-c','import sys;sys.stdout.buffer.write(sys.stdin.buffer.read())'],
            'input_bytes':len(data),'limit':limit,'timeout':5}
        if nested:
            header['command'] = ['docker','exec','-i','c'*64] + header['command']
        captured = {}
        class WS:
            opened=True
            payload=bytearray()
            out=b''
            status=b''
            def is_open(self): return self.opened
            def write_stdin(self, chunk): self.payload.extend(chunk)
            def update(self, timeout):
                if len(self.payload)==len(data):
                    command = captured['command']
                    if nested:
                        if command[:4] != ['docker','exec','-i','c'*64]:
                            raise AssertionError('EOF wrapper escaped nested worker')
                        command = command[4:]
                    result=subprocess.run(command,input=bytes(self.payload),capture_output=True,timeout=3)
                    if result.returncode: raise AssertionError(result.stderr.decode())
                    self.out=result.stdout
                    self.status=json.dumps({'status':'Failure' if fail else 'Success'}).encode()
                    self.opened=False
            def peek_stdout(self): return bool(self.out)
            def read_stdout(self): result,self.out=self.out,b'';return result
            def peek_stderr(self): return False
            def peek_channel(self,n): return bool(self.status)
            def read_channel(self,n): result,self.status=self.status,b'';return result
            def close(self): self.opened=False
        def stream(*args,**kwargs): captured.update(kwargs);return WS()
        def api(target):
            pod=types.SimpleNamespace(metadata=types.SimpleNamespace(uid='uid', labels={
                    'app.kubernetes.io/managed-by':'sandbox-broker', 'sandbox.example/workload':'forgejo-ci',
                    'sandbox.example/cluster':target}), status=types.SimpleNamespace(phase='Running'),
                    spec=types.SimpleNamespace(containers=[types.SimpleNamespace(name='runner'),types.SimpleNamespace(name='dind')]))
            return types.SimpleNamespace(read_namespaced_pod=lambda *args,**kw:pod,
                                         connect_get_namespaced_pod_exec=None)
        class Configuration:
            def __init__(self): self.api_key={};self.target=None
        def load(path, *, client_configuration):
            client_configuration.target=next(name for name,target in b.TARGETS.items() if target['kubeconfig']==path)
        def incluster(*, client_configuration): client_configuration.target='on-prem'
        kube=types.ModuleType('kubernetes')
        kube.client=types.SimpleNamespace(Configuration=Configuration,ApiClient=lambda cfg:cfg,CoreV1Api=lambda cfg:api(cfg.target))
        kube.config=types.SimpleNamespace(load_kube_config=load, load_incluster_config=incluster)
        exceptions=types.ModuleType('kubernetes.client.exceptions')
        exceptions.ApiException=type('ApiException',(Exception,),{})
        streaming=types.ModuleType('kubernetes.stream');streaming.stream=stream
        output=io.BytesIO()
        if profiles is None:
            profiles=[{'name':'aws-us-west-2','ci_enabled':True,
                       'kubeconfig':'/etc/sandbox-broker/clusters/eks-us-west-2.yaml'}]
        profiles=json.dumps(profiles)
        with patch.dict(sys.modules,{'kubernetes':kube,'kubernetes.client.exceptions':exceptions,
                                     'kubernetes.stream':streaming}), \
             patch.dict(os.environ, {'BROKER_CLUSTER_TARGETS':profiles}), \
             patch.object(sys,'stdin',types.SimpleNamespace(buffer=io.BytesIO(json.dumps(header).encode()+b'\n'+data))), \
             patch.object(sys,'stdout',types.SimpleNamespace(buffer=output)):
            exec(b.BRIDGE,{})
        self.assertEqual(captured['stdin'], bool(data))
        return output.getvalue()

    def test_real_input_wrapper_eof_and_output(self):
        self.assertEqual(self.bridge(b'public\x00input'), b'public\x00input')

    def test_nested_eof_wrapper_runs_inside_exact_worker(self):
        self.assertEqual(self.bridge(b'nested public input', nested=True), b'nested public input')

    def test_empty_input_exec_uses_no_stdin_channel(self):
        # List/inspect commands need no stdin. The simulated API executes the
        # unwrapped command with an empty input pipe, just as stdin=False does.
        self.assertEqual(self.bridge(b''), b'')

    def test_remote_failure_and_output_bound_rejected(self):
        with self.assertRaises(ValueError): self.bridge(b'public',fail=True)
        with self.assertRaises(ValueError): self.bridge(b'public',limit=1)

    def test_bridge_rejects_unknown_disabled_and_mismatched_pinned_targets(self):
        with self.assertRaises(ValueError):
            self.bridge(b'', profiles=[{'name':'unknown','ci_enabled':True}])
        with self.assertRaises(ValueError):
            self.bridge(b'', profiles=[{'name':'aws-us-west-2','ci_enabled':False,
                                        'kubeconfig':b.TARGETS['aws-us-west-2']['kubeconfig']}])

        with self.assertRaises(ValueError):
            self.bridge(b'', target='aws-eu-central-1')



class RemoteDeadlineTests(unittest.TestCase):
    def command(self, payload_bytes, timeout, code):
        return [sys.executable, '-c', b.REMOTE_WRAPPER, str(payload_bytes), str(timeout),
                json.dumps([sys.executable, '-c', code])]

    def test_binary_input_and_real_eof(self):
        data=bytes(range(256))*4097
        result=subprocess.run(self.command(len(data), 3, 'import sys;sys.stdout.buffer.write(sys.stdin.buffer.read())'),
                              input=data, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, data)

    def test_stalled_input_has_worker_deadline(self):
        child=subprocess.Popen(self.command(5, .2, 'raise AssertionError("must not run")'),
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.assertNotEqual(child.wait(timeout=2), 0)
        finally:
            child.kill();child.communicate()

    def test_timeout_kills_child_and_descendant(self):
        with tempfile.TemporaryDirectory() as tmp:
            marker=Path(tmp)/'escaped'
            descendant='import time,pathlib;time.sleep(.7);pathlib.Path('+repr(str(marker))+').write_text("escaped")'
            code='import subprocess,sys,time;subprocess.Popen([sys.executable,"-c",'+repr(descendant)+']);time.sleep(30)'
            before=time.monotonic()
            result=subprocess.run(self.command(0,.2,code),input=b'',capture_output=True,timeout=2)
            self.assertNotEqual(result.returncode,0)
            self.assertLess(time.monotonic()-before,1)
            time.sleep(.8)
            self.assertFalse(marker.exists())

    def test_exited_leader_does_not_leave_descendant_pipe_open(self):
        code='import subprocess,sys;subprocess.Popen([sys.executable,"-c","import time;time.sleep(30)"])'
        result=subprocess.run(self.command(0,1,code),input=b'',capture_output=True,timeout=2)
        self.assertEqual(result.returncode,0)

if __name__=='__main__': unittest.main()
