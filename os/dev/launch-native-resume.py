#!/usr/bin/env python3
"""Manual CI controller for exact-source checkpoint reconstruction, not a retry."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import shutil
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
BUILDER = 'code.sandbox.example/qos/tilo-builder@sha256:568fca1f8b4a4b1700c871cca033d2d291a5eb02c4b3be9681410f456b5a7bc9'
RPMS = 'code.sandbox.example/qos/tilo-development-rpms-aarch64@sha256:912255637bab849ee5c3f62a963481f0b994576ef08cec8ff4fe4bc4cfb9e02f'
CHECKPOINT_DIGESTS = {'CHECKPOINT_INITIAL': '0d474c3568ac76c984c85472e924d47007f99cd6aecbf5a0a5656b2f1f4bb750',
                      'CHECKPOINT_DELTA': '59342924ce24301542b6312c82a1aa2c8d5d5430f7b050ce4827c45ddd7153d4'}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def run(*args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, **kwargs).stdout.decode().strip()


def stage_tools_context(source, build, cosign):
    """Keep executable metadata explicit across the isolated COPY context."""
    shutil.copyfile(source / 'os/Containerfile.development-compile', build / 'Containerfile')
    staged = build / 'cosign'
    shutil.copyfile(cosign, staged)
    staged.chmod(0o755)
    # Run in the actual image during its existing build, before allocating a
    # worker or pulling application inputs. The frozen source remains untouched.
    # The provisional signer requires non-writable ancestors. Provision that
    # property in our disposable tools image instead of relying on RPM modes.
    with (build / 'Containerfile').open('a') as recipe:
        recipe.write('\nRUN chmod 0755 /var/lib && /usr/local/bin/cosign version\n')


EXTRA_PARTIALS = {'CHECKPOINT_RELEASE': ('checkpoint-release.json', '279f3b208500a67d6bceb38ead1fa33a5916417517a6ca6a74710d80588f88fd'),
                  'CHECKPOINT_WINDOWS': ('checkpoint-windows.json', '63673aa2e9d4764698bc8235e7dec24cd4ec9f7a9e77b8b926594ee52adc3403')}
ADMISSION = 'RECONSTRUCTED_PARTIAL_ADMISSION'


def prepare_extra_partials(environment, unique):
    labels = (*EXTRA_PARTIALS, ADMISSION)
    fields = [label + suffix for label in labels for suffix in ('', '_SHA256')]
    if not any(environment.get(key) for key in fields):
        return {}
    require(all(environment.get(key) for key in fields), 'all reconstructed partials and admission hashes required')
    receipts, documents = {}, {}
    for label in labels:
        raw = environment[label].encode()
        require(0 < len(raw) <= 65536 and re.fullmatch('[0-9a-f]{64}', environment[label+'_SHA256'])
                and hashlib.sha256(raw).hexdigest() == environment[label+'_SHA256'], 'reconstructed public input hash differs')
        value = json.loads(raw, object_pairs_hook=unique)
        require(isinstance(value, dict), 'reconstructed public input object required')
        receipts[label], documents[label] = raw, value
        if label in EXTRA_PARTIALS:
            require(value.get('purpose') == 'tilo-development-reconstructed-partial-native-checkpoint'
                and value.get('source_revision') == SOURCE and value.get('helper_revision') == '9d7a6b259c9b75b6c5031472bfed05b6461c1893'
                and value.get('run_id') == 21554 and value.get('run_number') == 8166
                and all(value.get(k) is False for k in ('installable','complete_native_builder','build_success','resume_admitted')),
                'reviewed incomplete reconstructed producer required')
            require(value.get('reference') == 'code.sandbox.example/qos/tilo-development-partial-native-aarch64@sha256:'+EXTRA_PARTIALS[label][1],
                    'reviewed reconstructed reference required')
    admission = documents[ADMISSION]
    require(admission.get('purpose') == 'tilo-reconstructed-partial-admission'
        and set(admission.get('receipts', {})) == {environment[k+'_SHA256'] for k in EXTRA_PARTIALS}, 'admission receipt set differs')
    for label in EXTRA_PARTIALS:
        require(admission['receipts'][environment[label+'_SHA256']]['inventory'] == documents[label]['inventory'], 'admitted inventory differs')
    return receipts


def prepare_receipts(environment):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate public receipt field')
            result[key] = value
        return result
    receipts = {}
    for label in ('CHECKPOINT_INITIAL', 'CHECKPOINT_DELTA', 'WPE_ADOPTION'):
        raw = environment[label].encode()
        require(0 < len(raw) <= 65536, 'bounded public receipt required')
        require(re.fullmatch('[0-9a-f]{64}', environment[label + '_SHA256']) and
                hashlib.sha256(raw).hexdigest() == environment[label + '_SHA256'], 'public receipt hash mismatch')
        value = json.loads(raw, object_pairs_hook=unique)
        require(isinstance(value, dict), 'receipt object required')
        if label.startswith('CHECKPOINT_'):
            require(value.get('purpose') == 'tilo-development-partial-native-checkpoint'
                    and value.get('source_revision') == SOURCE and value.get('complete_native_builder') is False
                    and value.get('build_success') is False and value.get('installable') is False,
                    'source4583 incomplete checkpoint required')
            require(value.get('reference') == 'code.sandbox.example/qos/tilo-development-partial-native-aarch64@sha256:' + CHECKPOINT_DIGESTS[label],
                    'reviewed checkpoint reference required')
        receipts[label] = raw
    require(environment['WPE_ADOPTION_SHA256'] == 'df41194aae2531d2cd9cd815cbce8009dd9dc254551b64154137228578ef32e2',
            'original WPE adoption bytes required')
    require(receipts['CHECKPOINT_INITIAL'] != receipts['CHECKPOINT_DELTA'], 'two distinct checkpoints required')
    if environment.get('THIN_CACHE_APPROVAL') or environment.get('THIN_CACHE_APPROVAL_SHA256'):
        raw = environment.get('THIN_CACHE_APPROVAL', '').encode()
        require(0 < len(raw) <= 65536 and re.fullmatch('[0-9a-f]{64}', environment.get('THIN_CACHE_APPROVAL_SHA256', ''))
                and hashlib.sha256(raw).hexdigest() == environment['THIN_CACHE_APPROVAL_SHA256'], 'thin cache approval hash differs')
        value = json.loads(raw, object_pairs_hook=unique)
        require(value.get('purpose') == 'tilo-incomplete-thin-cache-admission' and value.get('source_revision') == SOURCE,
                'thin cache approval identity differs')
        receipts['THIN_CACHE_APPROVAL'] = raw
    receipts.update(prepare_extra_partials(environment, unique))
    return receipts


def driver_argv(helper_revision, hashes):
    result = ['/usr/bin/python3', '/work/controller/os/dev/compile-development-os.py',
            '--source-dir', '/work/repo', '--controller-revision', helper_revision,
            '--mode', 'native-payload', '--architecture', 'aarch64',
            '--revision', SOURCE, '--branch', 'refs/heads/codex/arm64-tilo-integration',
            '--generation', '1', '--builder-candidate', BUILDER, '--rpms-candidate', RPMS,
            '--wpe-adoption-receipt', '/work/wpe-adoption.json',
            '--wpe-adoption-sha256', hashes['WPE_ADOPTION_SHA256'],
            '--resume-checkpoint', '/work/checkpoint-initial.json', hashes['CHECKPOINT_INITIAL_SHA256'],
            '--resume-checkpoint', '/work/checkpoint-delta.json', hashes['CHECKPOINT_DELTA_SHA256']]
    if hashes.get('THIN_CACHE_APPROVAL_SHA256'):
        result += ['--resume-thin-cache-approval', '/work/thin-cache-approval.json', hashes['THIN_CACHE_APPROVAL_SHA256']]
    if hashes.get(ADMISSION+'_SHA256'):
        for label, (filename, _) in EXTRA_PARTIALS.items():
            result += ['--resume-checkpoint', '/work/'+filename, hashes[label+'_SHA256']]
        result += ['--resume-reconstructed-partial-admission', '/work/reconstructed-partial-admission.json', hashes[ADMISSION+'_SHA256']]
    return result


def resource_snapshot(container):
    code = """import json,pathlib,time
root=pathlib.Path('/sys/fs/cgroup')
names=('cpu.max','cpu.stat','memory.max','memory.current','memory.events','memory.stat','cpu.pressure','memory.pressure','io.pressure')
print(json.dumps({'observed_unix':time.time(),'cgroup':{n:(root/n).read_text() if (root/n).is_file() else None for n in names}}))
"""
    try:
        return json.loads(run('docker', 'exec', container, '/usr/bin/python3', '-c', code, timeout=15))
    except (OSError, ValueError, subprocess.SubprocessError):
        return {'observation_unavailable': True, 'observed_unix': time.time()}


def attach_and_observe(container, evidence):
    # Existing compile workflow logs are already public-safe. Stream them live,
    # rather than retaining an unbounded capture buffer or hiding compiler errors.
    process = subprocess.Popen(['docker', 'start', '--attach', container])
    started = time.monotonic()
    samples = []
    try:
        for offset in (2, 60, 300):
            delay = max(0.1, started + offset - time.monotonic())
            try:
                process.wait(timeout=delay)
                break
            except subprocess.TimeoutExpired:
                samples.append(resource_snapshot(container))
                (evidence / 'early-resources.json').write_text(json.dumps(samples) + '\n')
        process.wait(timeout=max(1, started + 9300 - time.monotonic()))
        state = json.loads(run('docker', 'inspect', container))[0]['State']
        require(process.returncode == 0 and state['ExitCode'] == 0 and not state['OOMKilled'],
                'reconstruction failed; inspect streamed logs and public evidence')
    finally:
        # Disconnecting this client does not stop the container; outer cleanup
        # always removes the exact owned container, including on cancellation.
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--helpers', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    def interrupted(_signal, _frame):
        raise RuntimeError('reconstruction interrupted; inspect public checkpoint evidence')
    signal.signal(signal.SIGTERM, interrupted)
    helper_revision = os.environ['HELPERS_REVISION']
    require(re.fullmatch('[0-9a-f]{40}', helper_revision), 'exact reviewed helper commit required')
    receipts = prepare_receipts(os.environ)
    for path, expected in ((args.source, SOURCE), (args.helpers, helper_revision)):
        require(run('git', '-C', str(path), 'rev-parse', 'HEAD') == expected, 'checkout identity differs')
        require(not run('git', '-C', str(path), 'status', '--porcelain', '--untracked-files=all'), 'clean checkout required')
    # Shared bounded public-TAR export and always-attempt cleanup implementation.
    # This controller's reviewed helper commit must include the finalization launcher.
    transport = module(args.helpers / 'os/dev/launch-finalization-worker.py', 'resume_public_export')
    compile_driver = module(args.helpers / 'os/dev/compile-development-os.py', 'resume_compile_setup')
    run_id, attempt = os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT']
    require(run_id.isdecimal() and attempt.isdecimal(), 'numeric CI identity required')
    name = 'tilo-native-resume-' + run_id + '-' + attempt
    args.evidence.mkdir(parents=True, exist_ok=False)
    volumes, container, exports = [], None, {}
    tools_created = False
    with tempfile.TemporaryDirectory(prefix='tilo-resume-tools-') as temporary:
        temporary = Path(temporary)
        build = temporary / 'build'
        build.mkdir()
        secrets = temporary / 'cache-secrets'
        secrets.mkdir(mode=0o700)
        compile_driver.stage_cache_credentials(secrets)
        # Existing tools convention: no source or cache credentials in context.
        stage_tools_context(args.source, build, run('which', 'cosign'))
        try:
            require(subprocess.run(['docker', 'image', 'inspect', name + ':tools'], capture_output=True).returncode != 0,
                    'existing tools tag requires operator recovery')
            run('docker', 'build', '--file', str(build / 'Containerfile'), '--tag', name + ':tools',
                '--build-arg', 'BUILDER_REF=' + BUILDER, str(build), timeout=600)
            tools_created = True
            image = json.loads(run('docker', 'image', 'inspect', name + ':tools'))[0]
            require((image['Os'], image['Architecture']) == ('linux', 'arm64'), 'native ARM tools required')
            for suffix in ('work', 'storage', 'cache-secrets'):
                volume = name + '-' + suffix
                require(subprocess.run(['docker', 'volume', 'inspect', volume], capture_output=True).returncode != 0,
                        'existing worker resources require operator recovery')
                run('docker', 'volume', 'create', volume)
                volumes.append(volume)
            command = ['docker', 'create', '--name', name, '--privileged', '--cpus=4', '--memory=9g', '--memory-swap=9g',
                       '--entrypoint=/usr/bin/python3', '--env=REGISTRY_AUTH_FILE=/work/registry-auth.json',
                       '--env=PYTHONDONTWRITEBYTECODE=1']
            for key in ('SCCACHE_ENDPOINT', 'SCCACHE_BUCKET', 'SCCACHE_REGION', 'SCCACHE_S3_USE_SSL',
                        'SCCACHE_S3_KEY_PREFIX', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT'):
                command += ['--env', key]
            for suffix, destination in (('work', '/work'), ('storage', '/var/lib/containers'), ('cache-secrets', '/run/tilo-cache-secrets')):
                command += ['--mount', f'type=volume,src={name}-{suffix},dst={destination}']
            command += [name + ':tools', *driver_argv(helper_revision, os.environ)[1:]]
            container = run(*command)
            require(re.fullmatch('[0-9a-f]{64}', container), 'invalid Docker identity')
            # Persist-credentials:false checkouts retain Git identity for strict
            # source/controller checks. They contain no registry/cache credentials.
            run('docker', 'cp', str(args.source) + '/.', container + ':/work/repo')
            run('docker', 'cp', str(args.helpers), container + ':/work/controller')
            public_files = [('CHECKPOINT_INITIAL', 'checkpoint-initial.json'), ('CHECKPOINT_DELTA', 'checkpoint-delta.json'), ('WPE_ADOPTION', 'wpe-adoption.json')]
            if ADMISSION in receipts:
                public_files += [(key, value[0]) for key, value in EXTRA_PARTIALS.items()]
                public_files += [(ADMISSION, 'reconstructed-partial-admission.json')]
            for key, filename in public_files:
                path = temporary / filename
                path.write_bytes(receipts[key])
                run('docker', 'cp', str(path), container + ':/work/' + filename)
            if 'THIN_CACHE_APPROVAL' in receipts:
                path = temporary / 'thin-cache-approval.json'
                path.write_bytes(receipts['THIN_CACHE_APPROVAL'])
                run('docker', 'cp', str(path), container + ':/work/thin-cache-approval.json')
            run('docker', 'cp', str(secrets) + '/.', container + ':/run/tilo-cache-secrets')
            config = Path(os.environ.get('DOCKER_CONFIG', str(Path.home() / '.docker'))) / 'config.json'
            auth = temporary / 'registry-auth.json'
            auth.write_text(json.dumps({'auths': {'code.sandbox.example': json.loads(config.read_text())['auths']['code.sandbox.example']}}))
            auth.chmod(0o600)
            run('docker', 'cp', str(auth), container + ':/work/registry-auth.json')
            identity = {'schema': 1, 'source_revision': SOURCE, 'helpers_revision': helper_revision,
                        'tools_config_digest': image['Id'], 'docker_container_id': container,
                        'cpu_limit': 4, 'memory_limit_bytes': 9 * 1024**3,
                        'checkpoint_receipt_sha256': [os.environ['CHECKPOINT_INITIAL_SHA256'], os.environ['CHECKPOINT_DELTA_SHA256']]}
            if ADMISSION in receipts:
                identity['checkpoint_receipt_sha256'] += [os.environ[key+'_SHA256'] for key in EXTRA_PARTIALS]
                identity['reconstructed_partial_admission_sha256'] = os.environ[ADMISSION+'_SHA256']
            (args.evidence / 'worker.json').write_text(json.dumps(identity) + '\n')
            attach_and_observe(container, args.evidence)
        finally:
            try:
                if container and re.fullmatch('[0-9a-f]{64}', container):
                    for filename in ('result.json', 'inputs.json', 'controller-source.json', 'admission-capacity.json',
                                     'cache-configuration.json', 'reconstruction/reconstruction.json',
                                     'reconstruction/reconstructed-native.json'):
                        try:
                            value = transport.bounded_receipt(container, filename)
                            destination = args.evidence / filename.replace('/', '-')
                            destination.write_text(json.dumps(value) + '\n')
                            exports[filename] = value
                        except (OSError, ValueError, TimeoutError, subprocess.SubprocessError):
                            pass
            finally:
                failures = transport.cleanup_owned(container, volumes)
                (args.evidence / 'cleanup.json').write_text(json.dumps({'unremoved_owned_resources': failures}) + '\n')
                if tools_created:
                    subprocess.run(['docker', 'image', 'rm', name + ':tools'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    receipt = exports.get('reconstruction/reconstructed-native.json', {})
    require(receipt.get('purpose') == 'tilo-development-reconstructed-native-builder'
            and receipt.get('complete_native_builder') is True and receipt.get('consumer_adoption_required') is True
            and re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-reconstructed-native-aarch64@sha256:[0-9a-f]{64}', receipt.get('reference', '')),
            'successful reconstruction publication receipt was not exported')
    require(not failures, 'worker cleanup incomplete')


if __name__ == '__main__':
    main()
