#!/usr/bin/env python3
"""Prove the remote cache, or explicitly compile the full development OS.

The default cache-only mode creates no signing identity or OS image. Full-build
requires an explicit mode, immutable WPE input and development generation.

Full-build retains the unsealed raw rootfs in the development quarantine registry
after auditing its layers. It is not installable. Private identities belong to
this run; completing the image requires rebinding trust before signing and sealing.

The cache probe proves remote access from the tools container, not hits inside
every nested build. Actual build statistics remain authoritative. Pinned
sccache 0.17.0 filters SOURCE_DATE_EPOCH from rustc's environment; the probe
characterizes unchanged epoch-independent code and ordinary env! dependencies,
and does not certify epoch-sensitive rustc inputs.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import signal
import shutil
import subprocess
import stat
import sys
import tempfile
import time

sys.dont_write_bytecode = True

REPO = Path(__file__).resolve().parents[2]
CONTROLLER = REPO
RECONSTRUCTION_SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
PRIVATE = Path('/var/lib/tilo-provisional-compile')
EVIDENCE = Path('/evidence')
BUILDER = 'code.sandbox.example/qos/tilo-builder@sha256:568fca1f8b4a4b1700c871cca033d2d291a5eb02c4b3be9681410f456b5a7bc9'
PIN_FILES = {'BASE_REF': 'base-snapshot.env', 'RPMS_REF': 'rpms-snapshot.env',
             'KERNEL_RPMS_REF': 'kernel-snapshot.env'}
ARCHITECTURES = {'x86_64': 'amd64', 'aarch64': 'arm64'}
LOADER_NAMES = {'x86_64': 'systemd-bootx64.efi', 'aarch64': 'systemd-bootaa64.efi'}
GIB = 1024**3
INITIAL_FREE = 80 * GIB
ABORT_FREE = 8 * GIB
CACHE_SECRETS = Path('/run/tilo-cache-secrets')
CACHE_CREDENTIALS = {'AWS_ACCESS_KEY_ID': 'aws_access_key_id',
                     'AWS_SECRET_ACCESS_KEY': 'aws_secret_access_key'}


def validate_inputs(wpe, generation, revision, branch, mode='full-build',
                    architecture='x86_64', kernel_candidate='', wpe_stage_receipt=None,
                    wpe_adoption_receipt=None, wpe_adoption_sha256='',
                    fex_payload_candidate='', windows_guest_candidate='', hardware_profile='generic',
                    firmware_candidate='', firmware_manifest_sha256='', require_firmware_artifact=False,
                    native_builder_receipt=None, native_builder_sha256='',
                    reconstructed_native_receipt=None, reconstructed_native_sha256='',
                    approved_reconstruction_sha256='', resume_checkpoint=None,
                    native_source_compatibility=None, native_source_compatibility_sha256=''):
    validate_firmware_selection(mode, hardware_profile, firmware_candidate, firmware_manifest_sha256,
                                require_firmware_artifact)
    hardware_definition(hardware_profile, architecture)
    if architecture not in ARCHITECTURES:
        raise ValueError('unsupported native architecture')
    if mode == 'full-build' and architecture == 'aarch64' and not kernel_candidate:
        raise ValueError('ARM full-build requires an explicit immutable kernel candidate')
    if kernel_candidate:
        immutable_candidate(kernel_candidate)
    for name, reference in (('fex-payload', fex_payload_candidate), ('windows-guest', windows_guest_candidate)):
        if mode == 'full-build' and architecture == 'aarch64' and not reference:
            raise ValueError(f'ARM full-build requires an explicit immutable {name} candidate')
        if reference:
            immutable_candidate(reference)
    if mode not in ('cache-only', 'native-payload', 'full-build'):
        raise ValueError('mode must be cache-only, native-payload or full-build')
    if mode == 'native-payload' and hardware_profile != 'generic':
        raise ValueError('native payload compilation is independent of hardware profiles')
    if native_builder_receipt or native_builder_sha256:
        if (mode != 'full-build' or not native_builder_receipt
                or not re.fullmatch(r'[0-9a-f]{64}', native_builder_sha256)):
            raise ValueError('native builder reuse requires full-build and independently pinned receipt')
    reconstructed = bool(reconstructed_native_receipt or reconstructed_native_sha256 or approved_reconstruction_sha256)
    compatibility = bool(native_source_compatibility or native_source_compatibility_sha256)
    if compatibility and (not native_source_compatibility
            or not re.fullmatch(r'[0-9a-f]{64}', native_source_compatibility_sha256)
            or not reconstructed or revision == RECONSTRUCTION_SOURCE):
        raise ValueError('source compatibility requires corrected source and pinned reconstructed native admission')
    if reconstructed:
        if (mode != 'full-build' or architecture != 'aarch64' or (revision != RECONSTRUCTION_SOURCE and not compatibility)
                or native_builder_receipt or native_builder_sha256 or not reconstructed_native_receipt
                or not re.fullmatch(r'[0-9a-f]{64}', reconstructed_native_sha256)
                or not re.fullmatch(r'[0-9a-f]{64}', approved_reconstruction_sha256)):
            raise ValueError('reconstructed native adoption requires frozen ARM full-build and both independent approvals')
    if resume_checkpoint:
        if (mode != 'native-payload' or architecture != 'aarch64' or revision != RECONSTRUCTION_SOURCE
                or reconstructed or native_builder_receipt or native_builder_sha256 or not 1 <= len(resume_checkpoint) <= 4
                or any(len(pair) != 2 or not re.fullmatch(r'[0-9a-f]{64}', pair[1]) for pair in resume_checkpoint)):
            raise ValueError('partial resume requires frozen ARM native-payload and bounded pinned checkpoints')
    adopting = bool(wpe_adoption_receipt or wpe_adoption_sha256)
    if adopting and (mode not in ('full-build', 'native-payload') or not wpe_adoption_receipt
            or not re.fullmatch(r'[0-9a-f]{64}', wpe_adoption_sha256)
            or wpe or wpe_stage_receipt):
        raise ValueError('adoption requires its independent receipt SHA256 and excludes current WPE inputs')
    if mode in ('full-build', 'native-payload') and not adopting and not re.fullmatch(r'code\.podesta\.ai/qos/tilo-wpe-buildroot@sha256:[0-9a-f]{64}', wpe):
        raise ValueError('an immutable reviewed WPE candidate reference is required')
    if mode in ('full-build', 'native-payload') and not re.fullmatch(r'[1-9][0-9]{0,11}', generation):
        raise ValueError('generation must be an explicit positive decimal integer')
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('full committed source revision required')
    if branch != 'refs/heads/main' and not branch.startswith('refs/heads/codex/'):
        raise ValueError('development compilation requires main or a codex branch')


def validate_firmware_selection(mode, profile, reference, manifest_sha256, required=False):
    if required and mode == 'full-build' and profile != 'generic' and not reference:
        raise ValueError('board CI builds require an explicit immutable firmware candidate')
    if reference or manifest_sha256:
        if mode != 'full-build' or profile == 'generic':
            raise ValueError('firmware artifacts require an explicit board full-build')
        immutable_candidate(reference)
        if not re.fullmatch(r'[0-9a-f]{64}', manifest_sha256):
            raise ValueError('firmware artifact requires an independent manifest SHA256')


def import_hardware_inputs(reference, manifest_sha256, profile, architecture):
    """Import inert firmware data before starting any compiler or signer."""
    return json.loads(run('python3', 'os/dev/hardware-firmware-artifact.py', 'import',
        '--reference', reference, '--manifest-sha256', manifest_sha256,
        '--profile', profile, '--architecture', architecture, '--repo', str(REPO)))


def hardware_definition(profile, architecture):
    """Use the image builder's declarative profile contract, without mutation."""
    if profile == 'generic':
        return None
    spec = importlib.util.spec_from_file_location('compile_hardware', REPO / 'os/dev/hardware_profile.py')
    hardware = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(hardware)
    definition = hardware.load_definition(profile, REPO / 'os/hardware/profiles')
    if definition['architecture'] != architecture:
        raise ValueError(f'{profile} requires a native {definition["architecture"]} builder')
    return definition


def local_hardware_inputs(profile, architecture):
    """Fail before cache probes or compilation if locally staged firmware is invalid.

    Firmware stays in the existing ignored build-context directory. This does not
    publish it or arrange transport to a remote runner. The image builder repeats
    verification when it consumes the bytes.
    """
    definition = hardware_definition(profile, architecture)
    if definition is None or not definition['firmware']:
        return {}
    directory = REPO / 'os/hardware/firmware' / profile

    def regular(relative):
        path = REPO
        for component in relative.parts:
            path /= component
            if path.is_symlink():
                raise ValueError('local firmware path contains a symlink')
        if not path.is_file():
            raise ValueError(f'stage the {profile} firmware first: {relative.name}')
        return path

    manifest_path = regular((directory / 'manifest.json').relative_to(REPO))
    if manifest_path.stat().st_size > 16384:
        raise ValueError('firmware manifest is too large')
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    if (not isinstance(manifest, dict)
            or set(manifest) != {'schema_version', 'profile', 'files'}
            or type(manifest['schema_version']) is not int or manifest['schema_version'] != 1
            or manifest['profile'] != profile):
        raise ValueError('invalid firmware manifest for selected hardware profile')
    hashes = manifest['files']
    if not isinstance(hashes, dict) or set(hashes) != set(definition['firmware']):
        raise ValueError('firmware manifest must name exactly the hardware definition files')
    for name, digest in hashes.items():
        if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest):
            raise ValueError(f'invalid firmware SHA256 for {name}')
        path = regular((directory / name).relative_to(REPO))
        with path.open('rb') as stream:
            actual = hashlib.file_digest(stream, 'sha256').hexdigest()
        if path.stat().st_size == 0 or actual != digest:
            raise ValueError(f'firmware SHA256 mismatch: {name}')
    return hashes


def run(*command, env=None, capture=True, timeout=None, cwd=None):
    result = subprocess.run(command, cwd=REPO if cwd is None else cwd, env=env, check=True,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None, timeout=timeout)
    return result.stdout.decode().strip() if capture else ''


def build_host_crypto():
    """Host utility commands belong to the controller, not frozen OS source."""
    target = PRIVATE / 'host-tools-target'
    run('cargo', 'build', '--locked', '--release', '-p', 'tilo-crypto',
        '--target-dir', str(target), cwd=CONTROLLER)
    run('install', '-m', '0755', str(target / 'release/tilo-crypto'),
        '/usr/local/bin/tilo-crypto', cwd=CONTROLLER)


def write_receipt(name, value):
    (EVIDENCE / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def select_source(source_dir, revision, controller_revision):
    """Application source and controller are separate clean, committed inputs."""
    global REPO
    if source_dir is None:
        if controller_revision:
            raise ValueError('controller revision requires explicit source directory')
        return None
    if not re.fullmatch(r'[0-9a-f]{40}', controller_revision or ''):
        raise ValueError('separate controller requires immutable revision')
    source = Path(source_dir).resolve()
    if source == CONTROLLER:
        raise ValueError('explicit source must be separate from controller')
    for directory, expected in ((CONTROLLER, controller_revision), (source, revision)):
        actual = subprocess.check_output(['git', '-C', str(directory), 'rev-parse', 'HEAD'], text=True).strip()
        dirty = subprocess.check_output(['git', '-C', str(directory), 'status', '--porcelain', '--untracked-files=all'], text=True).strip()
        if actual != expected or dirty:
            raise ValueError('source/controller must be clean and match independent revisions')
    REPO = source
    return {'schema': 1, 'source_revision': revision, 'controller_revision': controller_revision,
            'source_recipe_sha256': hashlib.sha256((source / 'os/Containerfile').read_bytes()).hexdigest(),
            'controller_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}


def import_selected_native(native, args, inputs):
    if args.reconstructed_native_receipt:
        import native_resume
        return native_resume.import_reconstructed(native, args.reconstructed_native_receipt,
            args.reconstructed_native_sha256, inputs, args.approved_reconstruction_sha256)
    return native.import_artifact(args.native_builder_receipt, args.native_builder_sha256, inputs)


def resume_selected_native(args, inputs, environment):
    """Prepare private controller state, then enter the explicit producer once."""
    paths = []
    for name, value in (('resume-inputs.json', inputs), ('resume-environment.json', environment)):
        path = PRIVATE / name
        with path.open('x', encoding='utf-8') as stream:
            os.chmod(path, 0o600)
            json.dump(value, stream, sort_keys=True)
        paths.append(path)
    command = ['python3', str(CONTROLLER / 'os/dev/resume-native-payload.py'), '--source', str(REPO),
        '--inputs', str(paths[0]), '--environment', str(paths[1]), '--evidence', str(EVIDENCE / 'reconstruction'),
        '--run-id', os.environ['GITHUB_RUN_ID'], '--attempt', os.environ.get('GITHUB_RUN_ATTEMPT', '1')]
    for receipt, digest in args.resume_checkpoint:
        command += ['--checkpoint', str(receipt), digest]
    if args.resume_thin_cache_approval:
        command += ['--thin-cache-approval', *args.resume_thin_cache_approval]
    if args.resume_reconstructed_partial_admission:
        command += ['--reconstructed-partial-admission', *args.resume_reconstructed_partial_admission]
    run(*command, capture=False)


def completed_wpe_receipt(path, buildroot, architecture='x86_64'):
    """Preflight provenance; the build consumer also checks actual source assets."""
    if path is None:
        raise ValueError('full-build requires a completed WPE stage receipt; compile it once in the producer')
    spec = importlib.util.spec_from_file_location('compile_wpe_artifact', REPO / 'os/dev/wpe-stage-artifact.py')
    artifact = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(artifact)
    receipt = artifact.strict_json(artifact.read_bounded(path, 65536))
    inputs = receipt.get('inputs', {})
    artifact.validate_receipt(receipt, inputs)
    if (inputs.get('architecture') != architecture or inputs.get('epoch') != '0'
            or inputs.get('wpe_buildroot') != buildroot):
        raise ValueError('completed WPE receipt does not match this native build')
    return receipt


def adopted_wpe_receipt(path, digest, architecture):
    """Pin original receipt bytes; recipe/archive compatibility is checked by import."""
    spec = importlib.util.spec_from_file_location('compile_wpe_adoption', REPO / 'os/dev/adopt-development-wpe.py')
    adoption = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(adoption)
    receipt = adoption.pinned_json(path, digest)
    audit = receipt.get('audit', {})
    adoption.validate_receipt(receipt, audit, receipt.get('audit_sha256'))
    if (audit.get('architecture') != architecture
            or 'original_build_epoch' not in audit or audit['original_build_epoch'] is not None
            or 'original_source_revision' not in audit or audit['original_source_revision'] is not None):
        raise ValueError('adoption native architecture differs or historical identity was invented')
    # Preserve exact bytes: write_receipt() canonicalizes formatting and would
    # invalidate the independently supplied file digest used by the importer.
    raw = adoption.wpe.read_bounded(path, 65536)
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError('adoption receipt changed while snapshotting')
    return receipt, raw


def cache_configuration(environment):
    # These are the deployed broker's public S3 identities on both runner
    # placements. Do not send existing credentials to a redirected endpoint.
    expected = {
        'SCCACHE_ENDPOINT': 'https://s3.eu-central-1.amazonaws.com',
        'SCCACHE_BUCKET': 'podesta-agent-sccache-125650147312-eu-central-1',
        'SCCACHE_REGION': 'eu-central-1', 'SCCACHE_S3_USE_SSL': 'true',
    }
    if any(environment.get(name) != value for name, value in expected.items()):
        raise ValueError('remote compiler cache configuration differs from the reviewed runner S3 identity')
    prefix = environment.get('SCCACHE_S3_KEY_PREFIX', '')
    if not re.fullmatch(r'[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*', prefix) or len(prefix) > 128:
        raise ValueError('remote compiler cache requires a canonical runner key prefix')
    # Compiler, arguments and source content determine cache validity. Reuse
    # the existing trusted runner namespace rather than forcing a cold lane.
    return {**expected, 'SCCACHE_S3_KEY_PREFIX': prefix}


def stage_cache_credentials(directory):
    """Write only existing cache credentials outside the Docker build context."""
    cache_configuration(os.environ)
    if os.environ.get('AWS_SESSION_TOKEN'):
        raise ValueError('temporary AWS session credentials are not supported by this compile lane')
    directory = Path(directory)
    directory.chmod(0o700)
    for variable, filename in CACHE_CREDENTIALS.items():
        value = os.environ.get(variable, '')
        if not value or len(value) > 1024 or any(char.isspace() for char in value):
            raise ValueError('existing runner cache credentials are missing or malformed')
        descriptor = os.open(directory / filename, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, 'w') as stream:
            stream.write(value)


def cache_credentials():
    values = {}
    for variable, filename in CACHE_CREDENTIALS.items():
        descriptor = os.open(CACHE_SECRETS / filename, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077 or not 0 < info.st_size <= 1024:
                raise ValueError('cache credential must be a private root-owned bounded regular file')
            value = stream.read(1025)
        if len(value) > 1024 or not value or any(char.isspace() for char in value):
            raise ValueError('cache credential is malformed')
        values[variable] = value
    return values


def cache_counters(raw, prefix):
    payload = json.loads(raw)
    location = payload.get('cache_location', '')
    expected_location = f's3, name: podesta-agent-sccache-125650147312-eu-central-1, prefix: /{prefix}/'
    if location != expected_location:
        raise ValueError('compiler probe did not use the requested remote S3 cache')
    counters = payload.get('stats', {})
    def count(name):
        value = counters.get(name, 0)
        if isinstance(value, dict):
            value = sum(value.get('counts', {}).values())
        if type(value) is not int or value < 0:
            raise ValueError('invalid compiler cache counter')
        return value
    result = {name: count(name) for name in
              ('cache_hits', 'cache_misses', 'cache_writes', 'cache_errors', 'cache_read_errors', 'cache_write_errors')}
    if any(result[name] for name in ('cache_errors', 'cache_read_errors', 'cache_write_errors')):
        raise ValueError('remote compiler cache reported authentication or I/O errors')
    return result


def verify_remote_cache(configuration):
    """Exercise real S3 writes/reads through the pinned sccache, not just DNS."""
    receipt = {'configuration': configuration, 'status': 'probing', 'credentials_retained': False}
    write_receipt('cache-configuration.json', receipt)
    started = time.monotonic()
    environment = {name: os.environ[name] for name in ('PATH', 'HOME', 'RUSTUP_HOME', 'CARGO_HOME') if name in os.environ}
    environment.update(configuration)
    environment.update(SCCACHE_IGNORE_SERVER_IO_ERROR='0', SCCACHE_SERVER_PORT='4236',
                       AWS_EC2_METADATA_DISABLED='true')
    try:
        environment.update(cache_credentials())
        with tempfile.TemporaryDirectory(prefix='cache-probe-', dir=PRIVATE) as temporary:
            source, output = Path(temporary) / 'probe.rs', Path(temporary) / 'libtilo_cache_probe.rlib'
            source.write_text('pub fn tilo_cache_probe() -> &\'static str { env!("TILO_CACHE_PROBE_VALUE") }\n')
            first_hash = None
            for attempt in ('first', 'warm', 'changed_epoch', 'changed_dependency'):
                # New empty local cache plus a stopped server between attempts:
                # a warm hit must come from the configured remote S3 backend.
                cache_dir = Path(temporary) / attempt
                cache_dir.mkdir()
                environment['SCCACHE_DIR'] = str(cache_dir)
                environment['SOURCE_DATE_EPOCH'] = '2' if attempt == 'changed_epoch' else '1'
                environment['TILO_CACHE_PROBE_VALUE'] = '2' if attempt == 'changed_dependency' else '1'
                run('sccache', '--zero-stats', env=environment, timeout=45)
                run('sccache', 'rustc', '--crate-name', 'tilo_cache_probe', '--crate-type', 'rlib',
                    str(source), '--emit=link,dep-info', '--out-dir', temporary, env=environment, timeout=45)
                counters = cache_counters(run('sccache', '--show-stats', '--stats-format=json',
                                              env=environment, timeout=15), configuration['SCCACHE_S3_KEY_PREFIX'])
                receipt[attempt] = counters
                if not counters['cache_hits'] and not counters['cache_writes']:
                    raise ValueError('remote compiler probe neither hit nor populated the cache')
                if attempt == 'warm' and not counters['cache_hits']:
                    raise ValueError('remote compiler probe had no warm cache hit')
                output_hash = hashlib.sha256(output.read_bytes()).hexdigest()
                if first_hash is None:
                    first_hash = output_hash
                elif (output_hash == first_hash) != (attempt != 'changed_dependency'):
                    raise ValueError('compiler cache changed equivalent output or reused a stale env dependency')
                run('sccache', '--stop-server', env=environment, timeout=15)
                output.unlink()
        receipt['remote_hit_proof'] = 'S3 backend, restarted server, distinct empty local cache per attempt'
        receipt['tracked_env_dependency_output_changed'] = True
        receipt['epoch_behavior'] = {
            'source_uses_epoch': False, 'changed_epoch_output_identical': True,
            'limitation': 'Pinned sccache 0.17.0 filters SOURCE_DATE_EPOCH from rustc environment; '
                          'this does not prove correctness for epoch-sensitive compiler inputs',
        }
        receipt['status'] = 'verified-remote-compile-and-hit'
    except (OSError, ValueError, subprocess.SubprocessError):
        receipt['status'] = 'failed-before-full-compilation'
        raise ValueError('remote compiler cache probe failed; inspect public cache configuration and runner connectivity') from None
    finally:
        receipt['elapsed_seconds'] = time.monotonic() - started
        write_receipt('cache-configuration.json', receipt)
        try:
            run('sccache', '--stop-server', env=environment, timeout=15)
        except (OSError, subprocess.SubprocessError):
            pass


def capacity_decision(free, *, starting):
    threshold = INITIAL_FREE if starting else ABORT_FREE
    return free >= threshold


def memory_budget():
    result = {'scope': 'container-visible cgroup and guest meminfo'}
    for name in ('memory.max', 'memory.peak', 'memory.events', 'cpu.max', 'cpuset.cpus.effective',
                 'cpu.stat', 'cpu.pressure', 'memory.pressure', 'io.pressure'):
        path = Path('/sys/fs/cgroup') / name
        if path.is_file():
            with path.open() as stream:
                result[name] = stream.read(1024).strip()
        else:
            result[name] = 'unavailable'
    path = Path('/proc/meminfo')
    result['guest_memory'] = [line for line in path.read_text().splitlines()
                              if line.startswith(('MemTotal:', 'MemAvailable:', 'SwapTotal:'))]
    return result


def compile_with_capacity_watchdog(env):
    """Measure the capacity experiment and stop our build before guest ENOSPC."""
    initial = shutil.disk_usage('/var/lib/containers').free
    started = time.monotonic()
    receipt = {'capacity_experiment': True, 'sufficiency_previously_proven': False,
               'initial_free_bytes': initial, 'minimum_free_bytes': initial,
               'abort_free_bytes': ABORT_FREE, 'samples': 0, 'status': 'running'}
    child = subprocess.Popen(['bash', 'os/dev/build-image.sh'], cwd=REPO, env=env,
                             start_new_session=True)
    try:
        while True:
            free = min(shutil.disk_usage(path).free for path in ('/work', '/var/lib/containers'))
            receipt.update(last_sample_unix=time.time(), elapsed_seconds=time.monotonic() - started,
                           last_free_bytes=free,
                           minimum_free_bytes=min(receipt['minimum_free_bytes'], free),
                           samples=receipt['samples'] + 1)
            write_receipt('capacity.json', receipt)
            if not capacity_decision(free, starting=False):
                receipt['status'] = 'aborted-before-guest-storage-exhaustion'
                raise ValueError('capacity experiment reached the 8 GiB free-space reserve')
            try:
                code = child.wait(timeout=5)
                receipt['status'] = 'build-succeeded' if code == 0 else 'build-failed'
                if code:
                    raise subprocess.CalledProcessError(code, child.args)
                return
            except subprocess.TimeoutExpired:
                continue
    finally:
        if child.poll() is None:
            # This is only the process group launched by this driver. The outer
            # workflow subsequently removes its own disposable container/volumes.
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    # Exiting this container's PID 1 lets the outer workflow
                    # remove any remaining build processes with the container.
                    receipt['termination_timeout'] = True
        write_receipt('capacity.json', receipt)
        write_receipt('memory-final.json', memory_budget())


def inspect_compilation(image_name, revision, generation, target, manifest_digest, certificate_digest, architecture='x86_64'):
    spec = importlib.util.spec_from_file_location('compile_rootfs', REPO / 'os/dev/sign-development-rootfs.py')
    signing = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signing)
    raw = run('podman', '--remote=false', 'image', 'inspect', image_name)
    image = json.loads(raw)[0]
    signing.identity(raw, image['Id'], architecture, revision)
    with signing.mounted(image['Id']) as root:
        profile = signing.source.mounted_identity(root, target, manifest_digest, architecture)
        embedded_generation = signing.hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
        if embedded_generation.read_text().strip() != generation:
            raise ValueError('compiled image generation differs from explicit input')
        original = signing.hardware.trusted_path(root,
            Path('usr/lib/systemd/boot/efi') / LOADER_NAMES[architecture], regular=True)
        signed = signing.hardware.trusted_path(root,
            Path('usr/lib/systemd/boot/efi') / (LOADER_NAMES[architecture] + '.signed'), regular=True)
        loader = signing.loader.verify(original, signed, PRIVATE / 'public/efi-signing.crt',
                                        certificate_digest, architecture)
        # Query the mounted RPM database using the trusted tools image. Do not
        # run the compiled OS entrypoint or any image-supplied inspection code.
        packages = run('rpm', '--root', str(root), '-qa', '--qf',
                       '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n')
        if not packages:
            raise ValueError('compiled OS RPM inventory is empty')
        (EVIDENCE / 'installed-rpms.tsv').write_text('\n'.join(sorted(packages.splitlines())) + '\n')
    return {'image_id': image['Id'], 'hardware_profile': profile, 'loader': loader}


def pinned_inputs(architecture='x86_64', kernel_candidate='', *, include_kernel=True):
    result = {}
    for variable, filename in PIN_FILES.items():
        if variable == 'KERNEL_RPMS_REF' and not include_kernel:
            continue
        if variable == 'KERNEL_RPMS_REF' and kernel_candidate:
            result[variable] = immutable_candidate(kernel_candidate)
            continue
        lines = (REPO / 'os' / filename).read_text().splitlines()
        selected = variable
        if variable == 'KERNEL_RPMS_REF':
            selected = f'KERNEL_RPMS_{architecture.upper()}_REF'
        values = [line.split('=', 1)[1] for line in lines if line.startswith(selected + '=')]
        if len(values) != 1 or not re.fullmatch(r'code\.podesta\.ai/qos/[a-z0-9-]+@sha256:[0-9a-f]{64}', values[0]):
            raise ValueError(f'{filename} must contain one immutable {selected}')
        result[variable] = values[0]
    return result


def immutable_candidate(reference):
    if not re.fullmatch(r'code\.podesta\.ai/qos/[a-z0-9-]+@sha256:[0-9a-f]{64}', reference):
        raise ValueError('candidate must be an immutable tilo registry digest')
    return reference


def selected_pins(architecture='x86_64', kernel_candidate='', rpms_candidate='', require_kernel=True):
    if architecture not in ARCHITECTURES:
        raise ValueError('unsupported native architecture')
    if require_kernel and architecture == 'aarch64' and not kernel_candidate:
        raise ValueError('ARM full-build requires an explicit immutable kernel candidate')
    pins = pinned_inputs(architecture, kernel_candidate, include_kernel=require_kernel)
    for name, candidate in (('KERNEL_RPMS_REF', kernel_candidate), ('RPMS_REF', rpms_candidate)):
        if candidate and (name != 'KERNEL_RPMS_REF' or require_kernel):
            pins[name] = immutable_candidate(candidate)
    return pins


def verify_native_image(reference, architecture):
    immutable_candidate(reference)
    if architecture not in ARCHITECTURES:
        raise ValueError('unsupported native architecture')
    # Only manifest/config metadata is fetched. Multiarch indexes must resolve
    # to an actual native config; a digest alone does not establish architecture.
    raw = run('skopeo', 'inspect', '--config', '--tls-verify=true',
              '--override-os', 'linux', '--override-arch', ARCHITECTURES[architecture],
              'docker://' + reference)
    config = json.loads(raw)
    if config.get('architecture') != ARCHITECTURES[architecture] or config.get('os') != 'linux':
        raise ValueError('candidate image config does not match native Linux architecture')
    return {'reference': reference, 'architecture': config['architecture'], 'os': config['os'],
            'scope': 'native manifest/config selection; no layer or kernel payload execution'}


def prepare_base_installation_tooling(mode, image):
    # The native builder consumes builder/RPM/WPE layers, never the installed
    # base rootfs. Its registry identity is still verified by the caller.
    if mode == 'native-payload':
        return
    run('podman', '--remote=false', 'pull', image)
    write_receipt('base-installation-tooling.json', installation_tooling(image))


def installation_tooling(image):
    """Read CLI evidence without exposing devices, credentials or host mounts."""
    common = ('podman', '--remote=false', 'run', '--rm', '--pull=never',
              '--network=none', '--read-only', '--cap-drop=ALL',
              '--security-opt=no-new-privileges')
    def probe(executable, *arguments):
        try:
            return {'exit_code': 0, 'stdout': run(*common, '--entrypoint', executable,
                                                 image, *arguments)}
        except subprocess.CalledProcessError as error:
            # A minimal base may omit bootctl. Record absence/failure rather
            # than misreporting the full source compilation as unsuccessful.
            return {'exit_code': error.returncode, 'stdout': None}

    help_result = probe('/usr/bin/bootc', 'install', 'to-disk', '--help')
    help_text = help_result['stdout']
    flags = ('--via-loopback', '--generic-image', '--composefs-backend',
             '--bootloader', '--source-imgref', '--target-imgref',
             '--enforce-container-sigpolicy', '--run-fetch-check')
    return {'image': image, 'disk_created': False,
            'bootc_version': probe('/usr/bin/bootc', '--version'),
            'bootctl_version': probe('/usr/bin/bootctl', '--version'),
            'to_disk_help': help_result,
            # Presence in help is discovery evidence, not proof that installation
            # enforces a policy or that undocumented options are unsupported.
            'flags_advertised': {flag: None if help_text is None else bool(re.search(re.escape(flag) + r'(?=[\s=,\]]|$)', help_text))
                                 for flag in flags}}


def build_environment(wpe, generation, revision, manifest, signer, cache,
                      architecture='x86_64', pins=None, adoption_sha256='',
                      fex_payload_candidate='', windows_guest_candidate='', hardware_profile='generic',
                      verified_native_reuse=False):
    hardware_definition(hardware_profile, architecture)
    # No inherited TILO_BUILD_ARGS, prebuilt payload, skip-build or remote engine
    # settings can change this from a complete, local, source build.
    allowed = ('PATH', 'HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'REGISTRY_AUTH_FILE')
    env = {name: os.environ[name] for name in allowed if name in os.environ}
    env.update(LANG='C.UTF-8', PYTHONDONTWRITEBYTECODE='1', TILO_DISTROBOX='',
               TILO_IMAGE='localhost/tilo-development:compile-' + revision[:12],
               TILO_OUTPUT='/work/output', TMPDIR='/work/tmp',
               TILO_INSTALLER_SCRATCH=str(PRIVATE),
               TILO_PUBLIC_TRUST_MODE='development',
               TILO_PUBLIC_TRUST_BUNDLE=str(PRIVATE / 'public'),
               TILO_PUBLIC_TRUST_MANIFEST_SHA256=manifest,
               TILO_PCR_PREDICTION_SIGNER=signer,
               TILO_TARGET_IMGREF='code.sandbox.example/qos/tilo-development:provisional-' + revision[:12] + '-' + architecture,
               TILO_GENERATION=generation, TILO_SEAL_GENERATION='1',
               TILO_WITH_BROWSER='1', TILO_WPE_BOOTSTRAP='0',
               TILO_WPE_BUILD_JOBS='2', WPE_BUILDROOT_REF=wpe,
               TILO_WPE_STAGE_RECEIPT=str(EVIDENCE / 'wpe-stage.json'),
               TILO_WPE_SOURCE_DATE_EPOCH='0',
               TILO_RAW_ROOTFS_RECEIPT=str(EVIDENCE / 'raw-rootfs.json'),
               TILO_RAW_ROOTFS_ONLY='1',
               TILO_SKIP_BUILD='0', TILO_HARDWARE_PROFILE=hardware_profile,
               TILO_BUILD_ARGS='--build-arg TILO_PREBUILT_APPS=0 --build-arg TILO_PREBUILT_DESKTOP=0 --build-arg TILO_CARGO_BUILD_JOBS=2 --build-arg TILO_REQUIRE_COMPILER_CACHE=1')
    if adoption_sha256:
        if wpe or not re.fullmatch(r'[0-9a-f]{64}', adoption_sha256):
            raise ValueError('adoption excludes current WPE buildroot and requires receipt pin')
        for name in ('WPE_BUILDROOT_REF', 'TILO_WPE_STAGE_RECEIPT', 'TILO_WPE_SOURCE_DATE_EPOCH', 'TILO_WPE_BUILD_JOBS'):
            env.pop(name, None)
        env['TILO_WPE_ADOPTION_RECEIPT'] = str(EVIDENCE / 'wpe-adoption.json')
        env['TILO_WPE_ADOPTION_SHA256'] = adoption_sha256
    for name, reference in (('FEX_PAYLOAD_REF', fex_payload_candidate),
                            ('WINDOWS_GUEST_REF', windows_guest_candidate)):
        if reference:
            env[name] = immutable_candidate(reference)
    if pins is not None:
        env.update({name: pins[name] for name in ('RPMS_REF', 'KERNEL_RPMS_REF') if name in pins})
    for name, value in cache.items():
        env['TILO_BUILD_ARGS'] += f' --build-arg {name}={value}'
    for filename in (() if verified_native_reuse else CACHE_CREDENTIALS.values()):
        env['TILO_BUILD_ARGS'] += f' --secret id={filename},src={(CACHE_SECRETS / filename).as_posix()}'
    return env



def assembly_cache(environment, architecture, mode, native_id, adopted_receipt_sha256,
                   layer_epoch_anchor='', source_revision=''):
    """Enable public intermediate-layer reuse only after strict native import."""
    if (mode != 'full-build' or architecture not in ARCHITECTURES
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', native_id)
            or environment.get('TILO_NATIVE_BUILDER_IMAGE_ID') != native_id
            or environment.get('TILO_RAW_ROOTFS_ONLY') != '1'
            or environment.get('TILO_PUBLIC_TRUST_MODE') != 'development'
            or environment.get('TILO_WITH_BROWSER') != '1'
            or not re.fullmatch(r'[0-9a-f]{64}', adopted_receipt_sha256 or '')
            or environment.get('TILO_WPE_ADOPTION_SHA256') != adopted_receipt_sha256):
        raise ValueError('assembly cache requires verified native and adopted WPE raw development assembly')
    arguments = environment['TILO_BUILD_ARGS']
    if '--secret' in arguments or '--cache-' in arguments or '--layers' in arguments:
        raise ValueError('assembly cache arguments must be controller-owned without compiler secrets')
    normalization = None
    if layer_epoch_anchor:
        if not re.fullmatch(r'[0-9a-f]{40}', layer_epoch_anchor) or not re.fullmatch(r'[0-9a-f]{40}', source_revision):
            raise ValueError('layer anchor and source must be exact committed revisions')
        run('git', 'merge-base', '--is-ancestor', layer_epoch_anchor, source_revision)
        source_epoch = int(run('git', 'show', '-s', '--format=%ct', source_revision))
        layer_epoch = int(run('git', 'show', '-s', '--format=%ct', layer_epoch_anchor))
        if (environment.get('SOURCE_DATE_EPOCH') != str(source_epoch)
                or not 0 <= layer_epoch <= source_epoch <= 253402300799):
            raise ValueError('layer anchor or source epoch differs from admitted source')
        normalization = {'policy': 'raw-layer-ancestor-v1', 'anchor_revision': layer_epoch_anchor,
                         'layer_epoch': layer_epoch, 'source_revision': source_revision,
                         'source_date_epoch': source_epoch,
                         'scope': 'OCI raw layers only; source and signing epochs unchanged'}
        environment['TILO_RAW_LAYER_EPOCH'] = str(layer_epoch)
    repository = 'code.sandbox.example/qos/tilo-development-assembly-cache-' + architecture + '-v1'
    flags = ['--layers=true', '--skip-unused-stages=true', '--cache-from', repository, '--cache-to', repository]
    environment['TILO_BUILD_ARGS'] += ' ' + ' '.join(flags)
    return {'schema': 1, 'purpose': 'development-raw-assembly-registry-cache',
            'repository': repository, 'architecture': architecture, 'flags': flags,
            'native_image_id': native_id, 'wpe_receipt_sha256': adopted_receipt_sha256,
            'layer_normalization': normalization,
            'intermediate_uploads_during_build': True, 'private_signing_stages': False,
            'final_artifact_verification_required': True,
            'cross_run_cache_hits_verified': False,
            'limitations': 'Fresh public trust and source inputs can invalidate layers; cache is not artifact acceptance.'}


def retain_compilation(revision, generation, epoch, target, manifest, architecture='x86_64'):
    """Audit and retain one immutable raw build before its guest is destroyed.

    Native archive, registry and signature proofs passed on both architectures
    in run 7025. The actual build still must pass its own scoped layer audit.
    """
    spec = importlib.util.spec_from_file_location('compile_retention', REPO / 'os/dev/retain-development-rootfs.py')
    retention = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(retention)
    with (EVIDENCE / 'raw-rootfs.json').open('rb') as stream:
        raw = stream.read(16385)
    if len(raw) > 16384:
        raise ValueError('raw handoff exceeds receipt size limit')
    handoff = json.loads(raw, object_pairs_hook=retention.source.trust.unique_object)
    image_id = retention.validate_receipt(handoff, revision, architecture, manifest,
                                           target, int(generation), epoch)
    private_files = [PRIVATE / 'cosign.key', PRIVATE / 'owner.key',
                     PRIVATE / 'signer/pcr-signing.key', PRIVATE / 'signer/efi-signing.key',
                     CACHE_SECRETS / 'aws_secret_access_key']
    command = ['python3', str(CONTROLLER / 'os/dev/audit-rootfs-layers.py'), '--image-id', image_id]
    for path in private_files:
        command.extend(['--private-file', str(path)])
    audit_result = subprocess.run(command, cwd=REPO, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=1830, check=False)
    # Preserve the public rejection receipt, but never copy tool diagnostics.
    try:
        audit = json.loads(audit_result.stdout)
    except (ValueError, UnicodeError):
        raise ValueError('raw layer audit did not return a public receipt') from None
    write_receipt('raw-rootfs-audit.json', audit)
    if (audit_result.returncode or audit.get('status') != 'passed-scoped-scan'
            or audit.get('image_id') != image_id):
        raise ValueError('raw rootfs failed the private-material layer audit')
    retained = json.loads(run('python3', 'os/dev/retain-development-rootfs.py',
        '--receipt', str(EVIDENCE / 'raw-rootfs.json'), '--private-key', str(PRIVATE / 'cosign.key'),
        '--public-bundle', str(PRIVATE / 'public'), '--revision', revision,
        '--architecture', architecture, '--manifest-sha256', manifest, '--target', target,
        '--generation', str(generation), '--source-date-epoch', str(epoch)))
    if (retained.get('handoff') != handoff or retained.get('published') is not True
            or retained.get('installable') is not False):
        raise ValueError('retention returned an inconsistent handoff receipt')
    write_receipt('retained-rootfs.json', retained)
    return retained


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=('cache-only', 'native-payload', 'full-build'), default='cache-only')
    parser.add_argument('--native-builder-receipt', type=Path)
    parser.add_argument('--assembly-layer-epoch-anchor', default='',
                        help='exact ancestor commit timestamp for verified raw assembly layers')
    parser.add_argument('--native-builder-sha256', default='')
    parser.add_argument('--reconstructed-native-receipt', type=Path)
    parser.add_argument('--reconstructed-native-sha256', default='')
    parser.add_argument('--approved-reconstruction-sha256', default='')
    parser.add_argument('--native-source-compatibility', type=Path)
    parser.add_argument('--native-source-compatibility-sha256', default='')
    parser.add_argument('--resume-reconstructed-partial-admission', nargs=2, metavar=('JSON','SHA256'))
    parser.add_argument('--resume-checkpoint', nargs=2, action='append', metavar=('RECEIPT', 'SHA256'))
    parser.add_argument('--resume-thin-cache-approval', nargs=2, metavar=('APPROVAL', 'SHA256'))
    parser.add_argument('--source-dir', type=Path)
    parser.add_argument('--controller-revision', default='')
    parser.add_argument('--wpe-stage-receipt', type=Path)
    parser.add_argument('--wpe-adoption-receipt', type=Path)
    parser.add_argument('--wpe-adoption-sha256', default='')
    parser.add_argument('--architecture', choices=tuple(ARCHITECTURES), default='x86_64')
    parser.add_argument('--hardware-profile', default='generic',
                        help='Hardware definition; firmware must already be staged locally in os/hardware/firmware/PROFILE')
    parser.add_argument('--firmware-candidate', default='')
    parser.add_argument('--firmware-manifest-sha256', default='')
    parser.add_argument('--kernel-candidate', default='')
    parser.add_argument('--fex-payload-candidate', default='')
    parser.add_argument('--windows-guest-candidate', default='')
    parser.add_argument('--rpms-candidate', default='')
    parser.add_argument('--builder-candidate', default=BUILDER)
    for name in ('wpe', 'generation'):
        parser.add_argument('--' + name, default='')
    for name in ('revision', 'branch'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args(argv)
    if args.assembly_layer_epoch_anchor and (args.mode != 'full-build'
            or not args.reconstructed_native_receipt or not args.wpe_adoption_receipt):
        raise ValueError('layer anchor requires reconstructed-native and adopted-WPE full assembly')
    if args.resume_thin_cache_approval and (not args.resume_checkpoint or
            not re.fullmatch('[0-9a-f]{64}', args.resume_thin_cache_approval[1])):
        raise ValueError('thin cache approval requires explicit native resume and exact SHA256')
    if args.resume_reconstructed_partial_admission and (not args.resume_checkpoint
            or not re.fullmatch(r'[0-9a-f]{64}', args.resume_reconstructed_partial_admission[1])):
        raise ValueError('reconstructed partial admission requires pinned partial resume')
    os.environ['PYTHONDONTWRITEBYTECODE'] = '1'
    controller_source = select_source(args.source_dir, args.revision, args.controller_revision)
    validate_inputs(args.wpe, args.generation, args.revision, args.branch, args.mode, args.architecture, args.kernel_candidate,
                    args.wpe_stage_receipt, args.wpe_adoption_receipt, args.wpe_adoption_sha256,
                    args.fex_payload_candidate, args.windows_guest_candidate, args.hardware_profile,
                    args.firmware_candidate, args.firmware_manifest_sha256,
                    native_builder_receipt=args.native_builder_receipt,
                    native_builder_sha256=args.native_builder_sha256,
                    reconstructed_native_receipt=args.reconstructed_native_receipt,
                    reconstructed_native_sha256=args.reconstructed_native_sha256,
                    approved_reconstruction_sha256=args.approved_reconstruction_sha256,
                    resume_checkpoint=args.resume_checkpoint,
                    native_source_compatibility=args.native_source_compatibility,
                    native_source_compatibility_sha256=args.native_source_compatibility_sha256)
    if args.assembly_layer_epoch_anchor and '--source-date-epoch "$layer_epoch" --rewrite-timestamp' not in (REPO / 'os/dev/build-image.sh').read_text():
        raise ValueError('selected source lacks the explicit raw layer epoch API')
    source_compatibility = None
    if args.native_source_compatibility:
        import native_source_compatibility
        source_compatibility = native_source_compatibility.verify(REPO, args.revision,
            args.native_source_compatibility, args.native_source_compatibility_sha256,
            args.reconstructed_native_sha256, args.approved_reconstruction_sha256)
    for candidate in (args.rpms_candidate, args.builder_candidate):
        if candidate:
            immutable_candidate(candidate)
    if os.getuid() != 0 or platform.machine() != args.architecture:
        raise ValueError('already-root matching native Linux guest required')
    if run('git', 'rev-parse', 'HEAD') != args.revision or run('git', 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('checkout must be clean and match the requested revision')
    firmware_import = None
    if args.firmware_candidate:
        firmware_import = import_hardware_inputs(args.firmware_candidate, args.firmware_manifest_sha256,
                                                  args.hardware_profile, args.architecture)
    firmware = local_hardware_inputs(args.hardware_profile, args.architecture) if args.mode == 'full-build' else {}
    stage = adopted = adoption_raw = None
    if args.mode in ('full-build', 'native-payload'):
        if args.wpe_adoption_receipt:
            adopted, adoption_raw = adopted_wpe_receipt(args.wpe_adoption_receipt, args.wpe_adoption_sha256, args.architecture)
        else:
            stage = completed_wpe_receipt(args.wpe_stage_receipt, args.wpe, args.architecture)
    if source_compatibility is not None:
        native_source_compatibility.verify_selected_wpe(source_compatibility, adoption_raw or b'', args.wpe_adoption_sha256)
    os.umask(0o077)
    PRIVATE.mkdir(mode=0o700)
    EVIDENCE.mkdir(mode=0o755)
    if controller_source is not None:
        write_receipt('controller-source.json', controller_source)
    if source_compatibility is not None:
        write_receipt('native-source-compatibility.json', source_compatibility)
    if firmware_import is not None:
        write_receipt('firmware-import.json', firmware_import)
    cache = cache_configuration(os.environ)
    verify_remote_cache(cache)
    if args.mode == 'cache-only':
        write_receipt('result.json', {'status': 'remote-cache-probe-complete',
            'mode': args.mode, 'revision': args.revision, 'builder': args.builder_candidate,
            'installable': False, 'published': False, 'disk_created': False,
            'os_compiled': False, 'signing_identities_created': False,
            'scope': 'tools-container S3 compiler cache access; nested OS build not exercised'})
        return
    Path('/work/tmp').mkdir()
    if adopted is not None:
        (EVIDENCE / 'wpe-adoption.json').write_bytes(adoption_raw)
    else:
        write_receipt('wpe-stage.json', stage)
    pins = selected_pins(args.architecture, args.kernel_candidate, args.rpms_candidate,
                         require_kernel=args.mode == 'full-build')
    references = pins | {'BUILDER_REF': args.builder_candidate}
    if args.mode == 'native-payload':
        references.pop('KERNEL_RPMS_REF', None)  # This target never consumes kernel RPMs.
    references.update({'WPE_ADOPTED_REF': adopted['reference']} if adopted is not None
                      else {'WPE_BUILDROOT_REF': args.wpe})
    native_inputs = {name: verify_native_image(reference, args.architecture)
                     for name, reference in references.items()}
    write_receipt('native-inputs.json', native_inputs)
    write_receipt('inputs.json', {'purpose': ('retained-native-application-compilation'
        if args.mode == 'native-payload' else 'retained-full-source-compilation'),
        'installable': False, 'revision': args.revision, 'architecture': args.architecture,
        'hardware_profile': args.hardware_profile, 'firmware_sha256': firmware,
        'firmware_candidate': args.firmware_candidate or None,
        'firmware_manifest_sha256': args.firmware_manifest_sha256 or None,
        'generation': args.generation, 'wpe_candidate': args.wpe or None, 'builder': args.builder_candidate,
        'wpe_input_mode': 'adopted-development' if adopted is not None else 'current-producer',
        'wpe_adoption_sha256': args.wpe_adoption_sha256 or None,
        'pins': pins, 'browser': True, 'prebuilt_apps': False, 'prebuilt_desktop': False,
        'native_source_compatibility': source_compatibility})
    # Full source builds need real guest backing storage, not a small tmpfs.
    backing = {path: dict(zip(('total', 'used', 'free'), shutil.disk_usage(path)))
               for path in ('/work', '/var/lib/containers')}
    free = min(value['free'] for value in backing.values())
    admitted = capacity_decision(free, starting=True)
    write_receipt('admission-capacity.json', {'sample_unix': time.time(),
        'backing_filesystems_bytes': backing, 'required_free_bytes': INITIAL_FREE,
        'minimum_free_bytes': free, 'admitted': admitted,
        'capacity_sufficiency_proven': False})
    if not admitted:
        raise ValueError('capacity experiment requires 80 GiB free after tooling setup')
    write_receipt('memory-initial.json', memory_budget())
    info = json.loads(run('podman', '--remote=false', 'info', '--format', 'json'))
    if info['store']['graphDriverName'] != 'overlay':
        raise ValueError('private guest volume must support native Podman overlay storage')
    prepare_base_installation_tooling(args.mode, pins['BASE_REF'])
    write_receipt('tools.json', {name: run(*command) for name, command in {
        'podman': ('podman', '--version'), 'cargo': ('cargo', '--version'),
        'rustc': ('rustc', '--version'), 'cosign': ('cosign', 'version')}.items()})
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'))
    write_receipt('workspace.json', {'packages': sorted(package['name'] for package in metadata['packages'])})
    build_host_crypto()
    run('bash', 'os/tests/app_store.sh', capture=False)
    signer = json.loads(run('python3', 'os/dev/setup-development-signer.py', str(PRIVATE / 'signer')))
    identity_env = os.environ.copy()
    identity_env['COSIGN_PASSWORD'] = ''
    run('cosign', 'generate-key-pair', '--output-key-prefix', str(PRIVATE / 'cosign'), env=identity_env)
    run('/usr/local/bin/tilo-crypto', 'generate-ed25519-key',
        str(PRIVATE / 'owner.key'), str(PRIVATE / 'owner.pub'))
    trust = json.loads(run('python3', 'os/dev/development-trust.py', 'create',
        '--cosign-public', str(PRIVATE / 'cosign.pub'), '--pcr-public', signer['pcr_public'],
        '--efi-certificate', signer['efi_certificate'], '--registry-owner', str(PRIVATE / 'owner.pub'),
        '--output', str(PRIVATE / 'public')))
    shutil.copytree(PRIVATE / 'public', EVIDENCE / 'public-trust')
    env = build_environment(args.wpe, args.generation, args.revision, trust['manifest_sha256'], signer['signer'], cache, args.architecture, pins, args.wpe_adoption_sha256,
                            args.fex_payload_candidate, args.windows_guest_candidate, args.hardware_profile,
                            verified_native_reuse=bool(args.reconstructed_native_receipt and args.wpe_adoption_receipt))
    epoch = int(run('git', 'show', '-s', '--format=%ct', args.revision))
    env['SOURCE_DATE_EPOCH'] = str(epoch)
    if args.mode == 'native-payload' or args.native_builder_receipt or args.reconstructed_native_receipt:
        spec = importlib.util.spec_from_file_location('compile_source_native', REPO / 'os/dev/native_builder_artifact.py')
        native_builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native_builder)
        receipt_bytes = adoption_raw if adopted is not None else args.wpe_stage_receipt.read_bytes()
        # Stage receipts bind the completed WPE payload, not merely its buildroot.
        wpe_reference = adopted['reference'] if adopted is not None else stage['reference']
        native_inputs = native_builder.expected_inputs(REPO, args.revision, args.architecture,
            pins['BASE_REF'], pins['RPMS_REF'], wpe_reference,
            hashlib.sha256(receipt_bytes).hexdigest(), epoch)
        if source_compatibility is not None:
            native_inputs = native_source_compatibility.original_native_inputs(native_inputs, source_compatibility)
        if args.native_builder_receipt or args.reconstructed_native_receipt:
            native_id = import_selected_native(native_builder, args, native_inputs)
            env['TILO_NATIVE_BUILDER_IMAGE_ID'] = native_id
            if args.reconstructed_native_receipt and args.wpe_adoption_receipt:
                registry_cache = assembly_cache(env, args.architecture, args.mode, native_id, args.wpe_adoption_sha256,
                                                args.assembly_layer_epoch_anchor, args.revision)
                cache_receipt = json.loads((EVIDENCE / 'cache-configuration.json').read_text())
                cache_receipt['assembly_registry_cache'] = registry_cache
                write_receipt('cache-configuration.json', cache_receipt)
            if args.reconstructed_native_receipt:
                write_receipt('reconstructed-native-adoption.json', {
                    'schema': 1, 'source_revision': args.revision, 'image_id': native_id,
                    'receipt_sha256': args.reconstructed_native_sha256,
                    'approved_reconstruction_sha256': args.approved_reconstruction_sha256,
                    'inventory_verified': True, 'installable': False,
                    'native_inputs': native_inputs,
                    'source_compatibility': source_compatibility})
        else:
            env['TILO_NATIVE_BUILDER_ONLY'] = '1'
            env['TILO_NATIVE_BUILDER_IIDFILE'] = str(EVIDENCE / 'native-builder.iid')
    if args.resume_checkpoint:
        resume_selected_native(args, native_inputs, env)
        return
    compile_with_capacity_watchdog(env)
    if args.mode == 'native-payload':
        identity = (EVIDENCE / 'native-builder.iid').read_text().strip()
        retained_native = native_builder.export_artifact(identity, native_inputs,
            os.environ['GITHUB_RUN_ID'], os.environ.get('GITHUB_RUN_ATTEMPT', '1'))
        write_receipt('native-builder.json', retained_native)
        write_receipt('result.json', {'status': 'native-payload-retained',
            'installable': False, 'published': True, 'disk_created': False,
            'revision': args.revision, 'native_builder': retained_native})
        return
    retained = retain_compilation(args.revision, args.generation, epoch,
        env['TILO_TARGET_IMGREF'], trust['manifest_sha256'], args.architecture)
    write_receipt('raw-installation-tooling.json', installation_tooling(retained['handoff']['raw_rootfs_id']))
    write_receipt('result.json', {'status': 'raw-compilation-retained',
        'installable': False, 'published': True, 'sealed': False, 'disk_created': False,
        'reason': 'raw compilation; ephemeral trust must be rebound; disk/VM acceptance not performed',
        'retained_image': retained['image'], 'handoff_sha256': retained['handoff_sha256'],
        'revision': args.revision, 'generation': args.generation,
        'native_source_compatibility': source_compatibility,
        'public_manifest_sha256': trust['manifest_sha256'],
        'containerfile_sha256': hashlib.sha256((REPO / 'os/Containerfile').read_bytes()).hexdigest()})


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        # Captured key-generation output is intentionally never reproduced.
        raise SystemExit(f'development compile workflow failed: {type(error).__name__}: {error}') from None
