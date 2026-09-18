#!/usr/bin/env python3
"""Launch one disposable native-overlay finalizer; never compile or create disks."""
import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import selectors
import subprocess
import sys
import tarfile
import tempfile
import time

TOOLS = 'code.podesta.ai/qos/tilo-development-finalization-tools-aarch64@sha256:b8f09490cb08b93ca137a9aca5a7c87b97a218ae33453f6b24b315fb0469903f'
CONFIG = 'sha256:8deaeea8c5fd08f67856c6a82efa87ad5c580e829c46fd8b15d58900c6d5e47a'
SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
HEX = re.compile(r'[0-9a-f]{64}')
DIRECTORY_MODES = {'/var/lib': 0o755, '/var/lib/containers': 0o755,
                   '/work': 0o755, '/evidence': 0o700}
# Public receipts only. Never copy /work, /var/lib, logs, registry auth or spool.
EVIDENCE = ('01-rebind.json', '02-loader.json', '03-provisional.json',
            '04-composefs.json', '05-reseal.json', '06-prediction.json',
            '07-verified.json', '08-published.json', 'result.json', 'status.json', 'signing-pending.json')


def receipt_from_tar(raw):
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        entries = archive.getmembers()
        if len(entries) != 1 or not entries[0].isfile() or not 0 < entries[0].size <= 1024 * 1024:
            raise ValueError('one bounded regular public receipt required')
        return json.loads(archive.extractfile(entries[0]).read(), object_pairs_hook=unique)


def bounded_receipt(container, filename):
    """Bound tar transport before parsing, including a stalled/oversized copy."""
    process = subprocess.Popen(['docker', 'cp', container + ':/evidence/' + filename, '-'],
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    raw = bytearray()
    try:
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + 15
            while True:
                if time.monotonic() >= deadline:
                    raise TimeoutError('receipt export timed out')
                if not ready.select(min(1, deadline - time.monotonic())):
                    continue
                block = os.read(process.stdout.fileno(), 65536)
                if not block:
                    break
                raw.extend(block)
                if len(raw) > 1024 * 1024 + 65536:
                    raise ValueError('public receipt transport exceeds limit')
        if process.wait(timeout=5):
            raise ValueError('public receipt unavailable')
        return receipt_from_tar(raw)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        process.stdout.close()


def cleanup_owned(container, volumes):
    commands = []
    if container and HEX.fullmatch(container):
        commands.append(['docker', 'rm', '--force', container])
    commands.extend(['docker', 'volume', 'rm', volume] for volume in volumes)
    failures = []
    for command in commands:
        try:
            if subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30).returncode:
                failures.append(command[-1])
        except (OSError, subprocess.SubprocessError):
            failures.append(command[-1])
    return failures


def require_published_exports(exports, request):
    """Green requires coherent public publication evidence, not only exit zero."""
    result = exports.get('result.json')
    published = exports.get('08-published.json')
    if not isinstance(result, dict) or not isinstance(published, dict):
        raise ValueError('required publication receipts were not exported')
    canonical = json.dumps(request, sort_keys=True, separators=(',', ':')).encode()
    binding = hashlib.sha256(canonical).hexdigest()
    for receipt in (result, published):
        if receipt.get('schema') != 1 or receipt.get('request_sha256') != binding:
            raise ValueError('publication receipt belongs to another request')
    digest = published.get('manifest_digest', '')
    config = published.get('config_digest', '')
    if (not re.fullmatch(r'sha256:[0-9a-f]{64}', digest)
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', config)
            or published.get('stage') != 'published'
            or published.get('published_reference') != request['publish_repository'] + '@' + digest
            or published.get('image_id') != config
            or published.get('target_update_ref') != request['new_target']):
        raise ValueError('publication receipt lacks matching immutable identities')
    for key in ('image_id', 'published_reference', 'manifest_digest', 'config_digest'):
        if result.get(key) != published.get(key):
            raise ValueError('final result differs from published image')
    if result.get('publication_required') is not False:
        raise ValueError('final result still requires publication')


def run(*args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, **kwargs).stdout.decode().strip()


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate request field')
        result[key] = value
    return result


def request_bytes(raw, digest, helpers):
    data = raw.encode()
    if not 0 < len(data) <= 65536 or not HEX.fullmatch(digest):
        raise ValueError('bounded request and SHA256 required')
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError('request hash mismatch')
    value = json.loads(data, object_pairs_hook=unique)
    if not isinstance(value, dict):
        raise ValueError('request object required')
    # Validate the actual driver's complete contract before pulling tools or
    # allocating a worker. The narrower launcher pins below are additional.
    dev = Path(__file__).resolve().parent
    if str(dev) not in sys.path:
        sys.path.insert(0, str(dev))
    spec = importlib.util.spec_from_file_location('worker_finalizer_contract', dev / 'finalize-development-rootfs.py')
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    driver.validate_request(value)
    if not re.fullmatch(r'[0-9a-f]{40}', helpers):
        raise ValueError('exact helper commit required')
    required = {'source_revision': SOURCE, 'helpers_revision': helpers,
                'source_dir': '/work/source', 'architecture': 'aarch64',
                'receipt': '/work/inputs/retained-rootfs.json',
                'public_bundle': '/work/inputs/original-public',
                'new_public_bundle': '/work/inputs/new-public',
                'signer': '/var/lib/tilo-signing-bridge/remote-development-signer.py'}
    if any(value.get(k) != v for k, v in required.items()):
        raise ValueError('request does not match fixed worker contract')
    return data


def create_argv(name):
    argv = ['docker', 'create', '--name', name, '--privileged',
            '--memory=9g', '--memory-swap=9g', '--cpus=2',
            '--label=tilo.operation=image-finalization',
            '--env=REGISTRY_AUTH_FILE=/work/registry-auth.json']
    for suffix, path in [('work', '/work'), ('storage', '/var/lib/containers'),
                         ('scratch', '/var/lib'), ('evidence', '/evidence')]:
        argv += ['--mount', f'type=volume,src={name}-{suffix},dst={path}']
    return argv + ['--entrypoint=/usr/bin/python3', TOOLS,
                   '/work/controller/os/dev/launch-finalization-worker.py', '--inside']


def inside():
    # Ordinary guest image operations only. The separately supervised disk
    # controller must never treat this block-backed CI guest as a disk worker.
    for path, mode in DIRECTORY_MODES.items():
        p = Path(path)
        if p.is_symlink() or not p.is_dir() or p.stat().st_uid != 0:
            raise ValueError('trusted writable worker directories required')
        p.chmod(mode)
    info = json.loads(run('podman', '--remote=false', 'info', '--format=json'))
    if info['store']['graphDriverName'] != 'overlay':
        raise ValueError('native overlay required')
    deadline = time.monotonic() + 300
    gate = Path('/work/operator-ready')
    while not gate.exists():
        if time.monotonic() >= deadline:
            raise TimeoutError('operator preparation timed out')
        time.sleep(2)
    if gate.is_symlink() or not gate.is_file() or gate.stat().st_uid != 0:
        raise ValueError('invalid operator gate')
    expected = hashlib.sha256(Path('/work/finalization-request.json').read_bytes()).hexdigest()
    if gate.read_text().strip() != expected:
        raise ValueError('operator gate request mismatch')
    result = subprocess.run(['/usr/bin/python3',
        '/work/controller/os/dev/finalize-development-rootfs.py',
        '--helpers', '/work/helpers', '--request', '/work/finalization-request.json',
        '--evidence', '/evidence'])
    snapshot_spool()
    raise SystemExit(result.returncode)


def snapshot_spool():
    # Only validated pending metadata; never input/output signature payloads.
    import sys
    sys.path.insert(0, '/work/helpers/os/dev')
    import signing_spool as spool
    config = spool.document(spool.signer.trusted(Path('/var/lib/tilo-signing-bridge/bridge.json')))
    pending = []
    for entry in spool.private_directory(config['spool']).iterdir():
        if len(pending) >= 64:
            raise ValueError('too many pending signing requests')
        if not re.fullmatch(r'[0-9a-f]{32}', entry.name):
            raise ValueError('unexpected signing entry')
        if (entry / 'request.json').exists() and not (entry / 'consumed.json').exists():
            pending.append(spool.request_metadata(spool.document(entry / 'request.json')))
    Path('/evidence/signing-pending.json').write_text(json.dumps({'requests': pending}) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inside', action='store_true')
    parser.add_argument('--snapshot-spool', action='store_true')
    parser.add_argument('--controller', type=Path)
    parser.add_argument('--source', type=Path)
    parser.add_argument('--helpers', type=Path)
    parser.add_argument('--evidence', type=Path)
    args = parser.parse_args()
    if args.snapshot_spool:
        snapshot_spool()
        return
    if args.inside:
        inside()
        return
    if not all((args.controller, args.source, args.helpers, args.evidence)):
        parser.error('controller, source, helpers and evidence are required')
    def interrupted(_number, _frame):
        raise RuntimeError('interrupted; resolve pending signing before retry')
    signal.signal(signal.SIGTERM, interrupted)
    helpers = os.environ['HELPERS_REVISION']
    data = request_bytes(os.environ['FINALIZATION_REQUEST'], os.environ['FINALIZATION_REQUEST_SHA256'], helpers)
    for path, revision in ((args.source, SOURCE), (args.helpers, helpers)):
        if run('git', '-C', str(path), 'rev-parse', 'HEAD') != revision:
            raise ValueError('checkout revision mismatch')
        if run('git', '-C', str(path), 'status', '--porcelain', '--untracked-files=no'):
            raise ValueError('modified pinned checkout')
    run_id, attempt = os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT']
    if not run_id.isdecimal() or not attempt.isdecimal():
        raise ValueError('numeric CI identity required')
    name = f'tilo-finalization-{run_id}-{attempt}'
    args.evidence.mkdir(parents=True, exist_ok=True)
    run('docker', 'pull', TOOLS, timeout=600)
    image = json.loads(run('docker', 'image', 'inspect', TOOLS))[0]
    if image['Id'] != CONFIG or (image['Os'], image['Architecture']) != ('linux', 'arm64'):
        raise ValueError('tools identity mismatch')
    volumes = []
    container = None
    exports = {}
    try:
        # Refuse collisions: cleanup only resources this invocation created.
        for suffix in ('work', 'storage', 'scratch', 'evidence'):
            volume = name + '-' + suffix
            exists = subprocess.run(['docker', 'volume', 'inspect', volume], capture_output=True)
            if exists.returncode == 0:
                raise ValueError('existing worker volume; operator recovery required')
            run('docker', 'volume', 'create', volume)
            volumes.append(volume)
        container = run(*create_argv(name))
        if not HEX.fullmatch(container):
            raise ValueError('invalid Docker container identity')
        with tempfile.TemporaryDirectory(prefix='tilo-finalization-public-') as stage:
            stage = Path(stage)
            (stage / 'finalization-request.json').write_bytes(data)
            for label, tree in (('controller', args.controller), ('source', args.source), ('helpers', args.helpers)):
                archive = stage / (label + '.tar')
                run('git', '-C', str(tree), 'archive', '--format=tar', '--prefix=' + label + '/',
                    '--output=' + str(archive), 'HEAD', *(['os'] if label != 'source' else []))
                # docker cp extracts tar at destination; no .git or credentials.
                with archive.open('rb') as stream:
                    run('docker', 'cp', '-', container + ':/work', stdin=stream)
                archive.unlink()
            for label, revision in (('source', SOURCE), ('helpers', helpers)):
                marker = stage / ('.finalization-' + label + '.json')
                marker.write_text(json.dumps({'revision': revision}) + '\n')
                run('docker', 'cp', str(marker), container + ':/work/' + label + '/' + marker.name)
            run('docker', 'cp', str(stage / 'finalization-request.json'), container + ':/work/finalization-request.json')
            auth_path = Path(os.environ.get('DOCKER_CONFIG', str(Path.home() / '.docker'))) / 'config.json'
            credentials = json.loads(auth_path.read_text())['auths']['code.podesta.ai']
            auth = stage / 'registry-auth.json'
            auth.write_text(json.dumps({'auths': {'code.podesta.ai': credentials}}))
            auth.chmod(0o600)
            run('docker', 'cp', str(auth), container + ':/work/registry-auth.json')
        run('docker', 'start', container)
        state = json.loads(run('docker', 'inspect', container))[0]
        if state['Image'] != CONFIG or not state['State']['Running']:
            raise ValueError('worker did not start with pinned tools')
        identity = {'schema': 1, 'docker_container_id': container, 'docker_image_id': state['Image'],
                    'docker_started_at': state['State']['StartedAt'], 'docker_running': state['State']['Running'],
                    'tools_ref': TOOLS, 'source_revision': SOURCE, 'helpers_revision': helpers,
                    'request_sha256': hashlib.sha256(data).hexdigest(),
                    'controller_revision': run('git', '-C', str(args.controller), 'rev-parse', 'HEAD')}
        (args.evidence / 'worker-identity.json').write_text(json.dumps(identity, indent=2) + '\n')
        print(json.dumps(identity), flush=True)
        result = run('docker', 'wait', container, timeout=9000)
        if result != '0':
            raise RuntimeError('finalization failed; inspect public receipts, do not blindly retry')
    finally:
        try:
            if container and HEX.fullmatch(container):
                try:
                    subprocess.run(['docker', 'exec', container, '/usr/bin/python3',
                                    '/work/controller/os/dev/launch-finalization-worker.py', '--snapshot-spool'],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
                except subprocess.TimeoutExpired:
                    pass
                for file in EVIDENCE:
                    try:
                        value = bounded_receipt(container, file)
                        (args.evidence / file).write_text(json.dumps(value) + '\n')
                        exports[file] = value
                    except (OSError, ValueError, TimeoutError, tarfile.TarError, subprocess.SubprocessError):
                        pass
        finally:
            failures = cleanup_owned(container, volumes)
            (args.evidence / 'cleanup.json').write_text(json.dumps({'unremoved_owned_resources': failures}) + '\n')
    # Reached only after a successful worker exit, and after unconditional cleanup.
    # Missing/bad exports must never turn a lost result into a green CI job.
    require_published_exports(exports, json.loads(data, object_pairs_hook=unique))
    if failures:
        raise RuntimeError('owned worker cleanup incomplete')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError, KeyError):
        raise SystemExit('finalization worker rejected or failed; inspect public evidence and resolve pending signing before retry')
