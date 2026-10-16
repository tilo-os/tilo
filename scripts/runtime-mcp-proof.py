#!/usr/bin/env python3
"""Real native three-app proof. All workflow mutations use external MCP stdio."""
import argparse
import contextlib
import os
import shutil
import tempfile
import hashlib
import json
import pathlib
import queue
import subprocess
import threading
import time
import traceback


class ProofError(RuntimeError):
    pass


class StaleDiscovery(ProofError):
    pass


class PublicationPending(ProofError):
    pass


def square_region(width, height):
    if width <= 0 or height <= 0:
        raise ValueError('positive dimensions required')
    side = min(width, height)
    return ','.join(str(v) if v not in (0, 1) else str(int(v))
                    for v in ((width-side)/(2*width), (height-side)/(2*height), side/width, side/height))


def decode_reply(reply, request_id):
    if reply.get('jsonrpc') != '2.0' or reply.get('id') != request_id:
        raise ProofError(f'Unexpected protocol reply: {reply}')
    if 'error' in reply:
        raise ProofError(f'JSON-RPC error: {reply["error"]}')
    return reply['result']


@contextlib.contextmanager
def short_profile():
    # Darwin's Unix-domain socket addresses are bounded independently of the
    # artifact directory. Never put the live sockets under a long output path.
    with tempfile.TemporaryDirectory(prefix='tilo-mcp-', dir='/tmp') as directory:
        yield pathlib.Path(directory)


def browse_rows(tool, peer_id):
    pending = ['']
    seen = set()
    revision = ''
    pages = 0
    while pending:
        parent = pending.pop(0)
        cursor = []
        while True:
            pages += 1
            if pages > 128:
                raise ProofError('Directory traversal exceeded 128-page proof budget')
            page = tool('tilo_query', dict(peer_id=peer_id, parent_resource=parent,
                                        revision=revision, cursor=cursor))
            if page.get('stale_cursor'):
                raise StaleDiscovery('Directory revision changed during read-only discovery')
            revision = page['revision']
            for row in page['rows']:
                yield revision, row
                if row.get('has_children') and row['resource'] not in seen:
                    seen.add(row['resource'])
                    pending.append(row['resource'])
            if page['exhausted']:
                break
            next_cursor = page.get('next_cursor')
            if not next_cursor or next_cursor == cursor:
                raise ProofError('Directory pagination did not advance')
            cursor = next_cursor


def resolve_action(tool, peer_id, action, timeout=10, interval=.1):
    deadline = time.monotonic() + timeout
    while True:
        try:
            for revision, row in browse_rows(tool, peer_id):
                if row.get('action') == action and row.get('descriptor'):
                    target = dict(peer_id=peer_id, revision=revision, resource=row['resource'], action=action)
                    return target, tool('tilo_resolve', target)['row']['descriptor']
            raise PublicationPending(f'Action absent: {action}')
        except (StaleDiscovery, PublicationPending):
            # Only query/resolve have happened here. Never catch an uncertain
            # invocation or resubmit it in this read-only discovery loop.
            if time.monotonic() >= deadline:
                raise
            time.sleep(interval)


def single_argument(descriptor, value):
    params = descriptor['params']
    if len(params) != 1:
        raise ProofError(f'Expected one declared parameter: {params}')
    return {params[0]['name']: value}


def observe_terminal(tool, result, timeout=30, interval=.1):
    invocation_id = result['invocation_id']
    deadline = time.monotonic() + timeout
    while result['update']['state'] not in ('succeeded', 'failed', 'cancelled'):
        if time.monotonic() >= deadline:
            raise ProofError(f'Observation timeout; do not replay invocation: {result}')
        time.sleep(interval)
        result = tool('tilo_status', {'invocation_id': invocation_id})
    return result


def invoke(tool, target, arguments, timeout=30, interval=.1):
    result = observe_terminal(tool, tool('tilo_invoke', dict(target, arguments=arguments)), timeout, interval)
    if result['update']['state'] != 'succeeded':
        raise ProofError(f'Invocation terminal failure: {result}')
    return result['update']['output']


def expect_denial(tool, result, timeout=30, interval=.1):
    if result.get('isError'):
        return result
    terminal = observe_terminal(tool, result['structuredContent'], timeout, interval)
    if terminal['update']['state'] != 'failed':
        raise ProofError(f'Expected invocation failure, received {terminal}')
    return terminal


class Client:
    def __init__(self, command, root):
        self.transcript = (root / 'transcript.jsonl').open('w')
        self.stderr = (root / 'runtime.stderr.log').open('w')
        env = {key: value for key, value in os.environ.items() if not key.startswith(('TILO_', 'QUICK_'))}
        self.process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, text=True, bufsize=1)
        self.replies = queue.Queue()
        self.next_id = 0
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.process.stdout:
            self.replies.put(line)
        self.replies.put(None)

    def request(self, method, params):
        self.next_id += 1
        request = dict(jsonrpc='2.0', id=self.next_id, method=method, params=params)
        self.transcript.write(json.dumps({'direction': 'request', 'message': request}) + '\n')
        self.transcript.flush()
        self.process.stdin.write(json.dumps(request) + '\n')
        self.process.stdin.flush()
        try:
            line = self.replies.get(timeout=15)
        except queue.Empty as error:
            raise ProofError('MCP response timeout; no mutation replay') from error
        if line is None:
            raise ProofError(f'MCP EOF, host status {self.process.poll()}')
        reply = json.loads(line)
        self.transcript.write(json.dumps({'direction': 'response', 'message': reply}) + '\n')
        self.transcript.flush()
        return decode_reply(reply, self.next_id)

    def tool(self, name, arguments):
        result = self.request('tools/call', dict(name=name, arguments=arguments))
        if result.get('isError'):
            if any(item.get('text') == 'StaleRevision' for item in result.get('content', [])):
                raise StaleDiscovery('StaleRevision')
            raise ProofError(json.dumps(result))
        return result['structuredContent']

    def close(self):
        self.process.stdin.close()
        try:
            code = self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            code = self.process.wait(timeout=15)
        self.reader.join(timeout=2)
        self.process.stdout.close()
        self.stderr.close()
        self.transcript.close()
        return code


def inspect(binary, *args):
    result = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True)
    if result.returncode:
        raise ProofError(result.stderr.strip())
    return json.loads(result.stdout)


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def process_snapshot():
    lines = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,stat=,command='], text=True).splitlines()
    return [{'pid': int(p[0]), 'ppid': int(p[1]), 'state': p[2], 'command': p[3]} for line in lines
            if len(p := line.strip().split(None, 3)) == 4]


def child_processes(pid):
    return [process for process in process_snapshot() if process['ppid'] == pid]


def processes_with_pids(pids):
    # Check identities retained while the host was alive, even if a survivor
    # has been reparented after shutdown. Never terminate a process found here.
    return [process for process in process_snapshot() if process['pid'] in pids]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--bin-dir', type=pathlib.Path, default=pathlib.Path('target/debug'))
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    try:
        with short_profile() as profile:
            run_proof(args, root, profile)
    except Exception:
        (root / 'failure.txt').write_text(traceback.format_exc())
        raise


def run_proof(args, root, profile):
    binaries = {name: (args.bin_dir / name).resolve() for name in ('tilo-runtime-mcp', 'photos', 'word-processor', 'calculator')}
    originals = dict(binaries, inspector=(args.bin_dir / 'examples/runtime-mcp-inspect').resolve())
    (root / 'binaries').mkdir()
    for name, source_binary in originals.items():
        shutil.copy2(source_binary, root / 'binaries' / name)
    binaries = {name: root / 'binaries' / name for name in binaries}
    helper = root / 'binaries/inspector'
    client = None
    owned_pids = set()
    try:
        metadata = {'binaries': {name: {'path': str(path), 'built_path': str(originals[name]), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()} for name, path in dict(binaries, inspector=helper).items()}, 'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(), 'platform': subprocess.check_output(['uname', '-a'], text=True).strip()}
        provenance = args.bin_dir / 'provenance.json'
        if provenance.exists():
            metadata['build_provenance'] = json.loads(provenance.read_text())
        metadata['runtime_profile'] = str(profile)
        save(root / 'metadata.json', metadata)
        source = root / 'source.png'
        fixture = inspect(helper, 'fixture', source)
        manifest = {'apps': [
            {'id': 'dev.tilo.photos', 'executable': str(binaries['photos']), 'args': [str(source)]},
            {'id': 'dev.tilo.word-processor', 'executable': str(helper), 'args': ['observe', str(root)]},
            {'id': 'dev.tilo.word-processor', 'executable': str(helper), 'args': ['wait-ready-exec', str(root), str(binaries['word-processor'])]},
            {'id': 'dev.tilo.calculator', 'executable': str(binaries['calculator']), 'args': []},
        ]}
        save(root / 'apps.json', manifest)
        client = Client([str(binaries['tilo-runtime-mcp']), '--profile', str(profile), '--apps', str(root / 'apps.json')], root)
        owned_pids.add(client.process.pid)
        print(f'host_pid={client.process.pid} evidence={root}', flush=True)
        # The proof reads full rows (descriptors and contracts).
        client.request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {'experimental': {'so.tilo.mcp': {'detail': 'full'}}}, 'clientInfo': {'name': 'native-proof', 'version': '1'}})
        client.request('tools/list', {})
        (root / 'profile').mkdir()
        shutil.copy2(profile / 'workspace.json', root / 'profile/workspace.json')
        deadline = time.monotonic() + 60
        while True:
            apps = client.tool('tilo_list_apps', {})['apps']
            by_id = {app['app_id']: app for app in apps}
            if all(app['id'] in by_id for app in manifest['apps']):
                break
            if time.monotonic() > deadline:
                raise ProofError(f'Apps failed to publish: {apps}')
            time.sleep(.2)
        save(root / 'live-apps.json', apps)
        processes = child_processes(client.process.pid)
        owned_pids.update(process['pid'] for process in processes)
        if len(processes) != 4 or any('Z' in p['state'] for p in processes):
            raise ProofError(f'Expected three native apps and one live observer: {processes}')
        for name in ('photos', 'word-processor', 'calculator'):
            if not any(p['command'].startswith(str(binaries[name])) for p in processes):
                raise ProofError(f'Real {name} executable is not live: {processes}')
        save(root / 'live-processes.json', processes)
        photo = by_id['dev.tilo.photos']['peer_id']
        writer = by_id['dev.tilo.word-processor']
        calc = by_id['dev.tilo.calculator']['peer_id']
        target, descriptor = resolve_action(client.tool, photo, 'crop')
        denied = client.request('tools/call', {'name': 'tilo_invoke', 'arguments': dict(target, arguments=single_argument(descriptor, 123))})
        denied_terminal = expect_denial(client.tool, denied)
        denial_text = json.dumps(denied_terminal).lower()
        if 'argument' not in denial_text and 'region' not in denial_text:
            raise ProofError(f'Failure did not identify invalid typed arguments: {denied_terminal}')
        stale = client.request('tools/call', {'name': 'tilo_resolve', 'arguments': dict(target, peer_id=18446744073709551615)})
        if not stale.get('isError'):
            raise ProofError('Unavailable instance was accepted')
        stale_revision = client.request('tools/call', {'name': 'tilo_invoke', 'arguments': dict(target, revision='definitely-not-the-discovered-revision', arguments=single_argument(descriptor, square_region(fixture['width'], fixture['height'])))})
        if not stale_revision.get('isError'):
            raise ProofError(f'Stale revision was admitted: {stale_revision}')
        save(root / 'denials.json', {'invalid_typed_argument': {'initial': denied, 'terminal': denied_terminal}, 'unavailable_instance': stale, 'stale_revision': stale_revision})
        target, descriptor = resolve_action(client.tool, photo, 'crop')
        invoke(client.tool, target, single_argument(descriptor, square_region(fixture['width'], fixture['height'])))
        target, descriptor = resolve_action(client.tool, photo, 'export-edited-content')
        exported = invoke(client.tool, target, {})
        save(root / 'export-output.json', exported)
        resources = exported.get('resources', [])
        if len(resources) != 1:
            raise ProofError(f'Expected a single actual content resource: {exported}')
        image = inspect(helper, 'image', profile, source, resources[0], root / 'result.png')
        if image['hash'] == fixture['hash']:
            raise ProofError('Export reused original bytes')
        target, descriptor = resolve_action(client.tool, writer['peer_id'], 'place-image')
        invoke(client.tool, target, single_argument(descriptor, image['hash']))
        target, descriptor = resolve_action(client.tool, calc, 'add-function')
        formula = 'sin(x) + 0.314159'
        invoke(client.tool, target, single_argument(descriptor, formula))
        deadline = time.monotonic() + 10
        while True:
            try:
                page = {'rows': [row for _, row in browse_rows(client.tool, calc)]}
                if any(row['resource_kind'] == 'calculator.graph' and formula in row['resource_label'] for row in page['rows']):
                    break
                raise PublicationPending(f'Fresh graph discovery missing expected formula: {page}')
            except (StaleDiscovery, PublicationPending):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.1)
        save(root / 'calculator-fresh-query.json', page)
        deadline = time.monotonic() + 30
        while True:
            try:
                document = inspect(helper, 'document', root, image['hash'], root / 'received.automerge', root / 'document.json')
                break
            except ProofError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(.2)
        final_processes = child_processes(client.process.pid)
        if {p['pid'] for p in final_processes} != {p['pid'] for p in processes} or any('Z' in p['state'] for p in final_processes):
            raise ProofError('Native child liveness changed during proof')
        proof = {'passed': True, 'source': fixture, 'image': image, 'document': document, 'calculator_formula': formula, 'processes': final_processes, 'dispatch': 'external MCP stdio tools/call only'}
    finally:
        if client:
            owned_pids.update(process['pid'] for process in child_processes(client.process.pid))
            exit_code = client.close()
            save(root / 'shutdown.json', {'exit_code': exit_code})
            remaining = processes_with_pids(owned_pids)
            save(root / 'cleanup.json', {'owned_pids': sorted(owned_pids), 'remaining_processes': remaining})
        if (profile / 'logs').exists():
            shutil.copytree(profile / 'logs', root / 'app-logs')
        if (profile / 'workspace.json').exists():
            (root / 'profile').mkdir(exist_ok=True)
            shutil.copy2(profile / 'workspace.json', root / 'profile/workspace.json')
        final_hashes = {name: hashlib.sha256(path.read_bytes()).hexdigest()
                        for name, path in dict(binaries, inspector=helper).items()}
        save(root / 'binary-hashes-after.json', final_hashes)
    if exit_code != 0 or remaining:
        raise ProofError(f'Host/child cleanup failed: exit={exit_code}, remaining={remaining}')
    if any(metadata['binaries'][name]['sha256'] != value for name, value in final_hashes.items()):
        raise ProofError('A retained executable changed during the native proof')
    save(root / 'proof.json', proof)
    print(json.dumps({'passed': True, 'output': str(root), 'edited_hash': image['hash']}), flush=True)


if __name__ == '__main__':
    main()
