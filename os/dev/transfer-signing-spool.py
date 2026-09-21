#!/usr/bin/python3
"""Transport public signing files through an existing authenticated kubectl context.

Does not retrieve/copy credentials, authorize inputs, invoke signing keys, or
install anything. Pod/container identity is checked around every transfer.
"""
import argparse
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import signal
import subprocess
import time
import signing_spool as spool

SPEC = importlib.util.spec_from_file_location('spool_endpoint', Path(__file__).with_name('signing-spool-endpoint.py'))
endpoint = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(endpoint)

PAYLOAD_SETUP_SECONDS = 120
PAYLOAD_MIN_BYTES_PER_SECOND = 1024 * 1024


def payload_timeout(byte_count):
    """Keep the fixed setup budget and admit one second per MiB streamed."""
    if type(byte_count) is not int or not 0 < byte_count <= spool.MAX_INPUT + 65536:
        raise ValueError('invalid signing payload size')
    stream_seconds = (byte_count + PAYLOAD_MIN_BYTES_PER_SECOND - 1) // PAYLOAD_MIN_BYTES_PER_SECOND
    return PAYLOAD_SETUP_SECONDS + stream_seconds


def bounded_process(argv, sink, limit, *, source=None, timeout=120):
    process = subprocess.Popen(argv, stdin=source if source is not None else subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
    deadline, count = time.monotonic() + timeout, 0
    completed = False
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while True:
                if time.monotonic() >= deadline:
                    raise ValueError('authenticated transfer deadline exceeded')
                if not selector.select(min(1, deadline - time.monotonic())):
                    continue
                data = process.stdout.read1(1024 * 1024)
                if not data:
                    break
                count += len(data)
                if count > limit:
                    raise ValueError('authenticated transfer exceeds byte admission')
                sink.write(data)
        if process.wait(timeout=max(0.1, deadline - time.monotonic())):
            raise ValueError('authenticated transfer command failed')
        completed = True
        return count
    finally:
        # The leader may already have exited while descendants hold stdout.
        # A failed operation must still terminate the owned process group.
        if not completed:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.wait()
        process.stdout.close()


def validate_binding(binding):
    expected = {'schema', 'namespace', 'pod', 'container', 'pod_uid', 'container_id', 'image',
                'image_id', 'restart_count', 'pod_spec_sha256', 'context_sha256', 'endpoint'}
    variants = tuple(expected | extra for extra in (
        set(), {'docker'}, {'cluster'}, {'docker', 'cluster'}))
    if (not isinstance(binding, dict) or set(binding) not in variants
            or binding['schema'] != 1):
        raise ValueError('invalid pinned transport binding')
    for name in ('namespace', 'pod', 'container'):
        if not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,252}', binding[name]):
            raise ValueError('invalid Kubernetes resource name')
    if ('cluster' in binding and
            not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,127}', binding['cluster'])):
        raise ValueError('invalid pinned cluster name')
    for name in ('pod_spec_sha256', 'context_sha256'):
        if not spool.HEX.fullmatch(binding[name]):
            raise ValueError('immutable transport identity missing')
    if type(binding['restart_count']) is not int or binding['restart_count'] < 0:
        raise ValueError('invalid pinned restart count')
    for name in ('pod_uid', 'container_id', 'image', 'image_id'):
        if not isinstance(binding[name], str) or not binding[name] or len(binding[name]) > 1024:
            raise ValueError('invalid pinned container identity')
    path = PurePosixPath(binding['endpoint'])
    if not path.is_absolute() or '..' in path.parts or path.name != 'signing-spool-endpoint.py':
        raise ValueError('endpoint must be the installed fixed signing helper')
    if 'docker' in binding:
        nested = binding['docker']
        if (not isinstance(nested, dict) or set(nested) != {'id', 'image_id', 'started_at', 'restart_count'}
                or not isinstance(nested['id'], str) or not re.fullmatch(r'[0-9a-f]{64}', nested['id'])
                or not isinstance(nested['image_id'], str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', nested['image_id'])
                or not isinstance(nested['started_at'], str) or not 1 <= len(nested['started_at']) <= 64
                or type(nested['restart_count']) is not int or nested['restart_count'] < 0):
            raise ValueError('invalid pinned nested Docker identity')
    return binding


def check_pod(pod, binding):
    metadata, spec, status = pod.get('metadata', {}), pod.get('spec', {}), pod.get('status', {})
    if (metadata.get('uid') != binding['pod_uid'] or metadata.get('name') != binding['pod']
            or metadata.get('namespace') != binding['namespace'] or metadata.get('deletionTimestamp')
            or status.get('phase') != 'Running' or spool.digest(spool.canonical(spec)) != binding['pod_spec_sha256']):
        raise ValueError('pod identity or specification changed')
    entries = [item for item in spec.get('containers', []) if item.get('name') == binding['container']]
    states = [item for item in status.get('containerStatuses', []) if item.get('name') == binding['container']]
    if len(entries) != 1 or len(states) != 1:
        raise ValueError('named container identity missing')
    item = states[0]
    if (entries[0].get('image') != binding['image'] or item.get('imageID') != binding['image_id']
            or item.get('containerID') != binding['container_id'] or item.get('restartCount') != binding['restart_count']
            or not item.get('state', {}).get('running')):
        raise ValueError('container identity or restart state changed')


class Transport:
    def __init__(self, context, binding, runner=bounded_process):
        self.binding = validate_binding(binding)
        self.runner = runner
        self.prefix = ['kubectl', '--context=' + context, '--namespace=' + binding['namespace']]

    def exec_prefix(self):
        return self.prefix + ['exec', '-i', self.binding['pod'], '-c', self.binding['container'], '--']

    def check(self):
        output = io.BytesIO()
        self.runner(self.prefix + ['get', 'pod', self.binding['pod'], '-o', 'json', '--request-timeout=15s'], output, 1024 * 1024, timeout=30)
        check_pod(json.loads(output.getvalue(), object_pairs_hook=spool.unique), self.binding)
        if 'docker' in self.binding:
            nested = self.binding['docker']
            output = io.BytesIO()
            # Inspect only public identity/state fields, never Config.Env or
            # registry authentication. A full ID cannot select a replacement name.
            template = '{"Id":{{json .Id}},"Image":{{json .Image}},"State":{{json .State}},"RestartCount":{{json .RestartCount}}}'
            self.runner(self.exec_prefix() + ['docker', 'inspect', '--type=container', '--format', template, nested['id']],
                        output, spool.MAX_JSON, timeout=30)
            info = json.loads(output.getvalue(), object_pairs_hook=spool.unique)
            state = info.get('State', {})
            if (info.get('Id') != nested['id'] or info.get('Image') != nested['image_id']
                    or info.get('RestartCount') != nested['restart_count']
                    or state.get('StartedAt') != nested['started_at'] or state.get('Status') != 'running'
                    or state.get('Running') is not True or any(state.get(k) is not False for k in ('Paused', 'Restarting', 'Dead'))):
                raise ValueError('nested Docker container identity or state changed')

    def transfer(self, operation, nonce=None, response=None, source=None, sink=None,
                 limit=spool.MAX_JSON, timeout=120):
        self.check()
        command = self.exec_prefix()
        if 'docker' in self.binding:
            command += ['docker', 'exec', '-i', self.binding['docker']['id']]
        command += ['/usr/bin/python3', self.binding['endpoint'], operation]
        if nonce is not None:
            command += ['--request-id', nonce]
        if response is not None:
            command += ['--response', spool.canonical(response).decode()]
        target = sink if sink is not None else io.BytesIO()
        self.runner(command, target, limit, source=source, timeout=timeout)
        self.check()
        return target.getvalue() if sink is None else None

    def fetch(self, nonce, destination):
        if not re.fullmatch(r'[0-9a-f]{32}', nonce):
            raise ValueError('invalid request identifier')
        destination = Path(destination)
        spool.private_directory(destination.parent)
        destination.mkdir(mode=0o700)
        raw = self.transfer('request', nonce)
        request = spool.request_metadata(json.loads(raw, object_pairs_hook=spool.unique))
        if request['request_id'] != nonce or request['context_sha256'] != self.binding['context_sha256']:
            raise ValueError('fetched request differs from pinned job context')
        # Keep incomplete input quarantined: local signer requires request.json,
        # which appears only after all bytes and post-transfer identity pass.
        with spool.signer.exclusive_output(destination / 'input') as output:
            self.transfer('input', nonce, sink=output, limit=request['input_bytes'],
                          timeout=payload_timeout(request['input_bytes']))
            output.flush()
            os.fsync(output.fileno())
        data = spool.snapshot(destination / 'input', spool.MAX_INPUT)
        if len(data) != request['input_bytes'] or spool.digest(data) != request['input_sha256']:
            raise ValueError('fetched signing input differs')
        spool.publish(destination / 'request.json', spool.canonical(request))
        return {'request_id': nonce, 'input_sha256': request['input_sha256'], 'input_bytes': len(data), 'received': True}

    def deliver(self, response_directory):
        directory = spool.private_directory(response_directory)
        response = spool.document(directory / 'response.json')
        nonce = response.get('request_id')
        if not isinstance(nonce, str) or not re.fullmatch(r'[0-9a-f]{32}', nonce):
            raise ValueError('invalid response identifier')
        remote = spool.request_metadata(json.loads(self.transfer('request', nonce), object_pairs_hook=spool.unique))
        if remote['context_sha256'] != self.binding['context_sha256']:
            raise ValueError('remote signing context changed')
        endpoint.response_for(remote, spool.canonical(response))
        data = spool.snapshot(directory / 'output', spool.MAX_INPUT + 65536)
        if len(data) != response['output_bytes'] or spool.digest(data) != response['output_sha256']:
            raise ValueError('local signing response changed')
        # File-backed stdin reaches EOF through kubectl without an unbounded
        # interactive session; only public signed bytes cross the connection.
        import tempfile
        with tempfile.TemporaryFile() as stream:
            stream.write(data)
            stream.seek(0)
            self.transfer('put-output', nonce, response, source=stream,
                          timeout=payload_timeout(response['output_bytes']))
        self.transfer('publish-response', nonce, response)
        return {'request_id': nonce, 'output_sha256': response['output_sha256'], 'delivered': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('list', 'fetch', 'deliver'))
    parser.add_argument('--context', required=True)
    parser.add_argument('--broker', action='store_true', help='use existing authenticated broker API route')
    parser.add_argument('--binding', required=True, type=Path)
    parser.add_argument('--request-id')
    parser.add_argument('--directory', type=Path)
    args = parser.parse_args()
    try:
        binding = spool.document(spool.signer.trusted(args.binding))
        if args.broker:
            from broker_signing_runner import BrokerRunner, CONTEXT
            if args.context != CONTEXT:
                raise ValueError('broker route requires its exact named context')
            client = Transport(args.context, binding, runner=BrokerRunner(binding))
        else:
            client = Transport(args.context, binding)
        if args.operation == 'list':
            result = json.loads(client.transfer('list'), object_pairs_hook=spool.unique)
        elif args.operation == 'fetch':
            result = client.fetch(args.request_id, args.directory)
        else:
            result = client.deliver(args.directory)
        print(json.dumps(result, sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError, subprocess.TimeoutExpired):
        parser.exit(1, 'authenticated signing-spool transport rejected; no credential diagnostics emitted\n')
