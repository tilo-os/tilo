#!/usr/bin/env python3
"""Rebind an authenticated retained rootfs in a disposable native container.

Only public files and the independently verified regenerated initramfs enter the
new image. No private key inputs, publication, signing, sealing or boot claims.
"""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


importer = module('transform_importer', 'import-development-rootfs.py')
completion = module('transform_completion', 'complete-initramfs-rebind.py')
rebind = completion.rebind
hardware = rebind.hardware
REQUIRED_MODULES = {'crypt', 'systemd-cryptsetup', 'tpm2-tss', 'plymouth', 'tilo-rollback'}
OMITTED_MODULES = {'network', 'network-legacy', 'network-manager', 'nfs', 'iscsi',
    'nvmf', 'nbd', 'fcoe', 'fcoe-uefi', 'nbde', 'clevis', 'clevis-pin-null',
    'clevis-pin-sss', 'clevis-pin-tang', 'clevis-pin-tpm2', 'crypt-ssh'}
BOARD_OMITTED_DRIVERS = {'lenovo-83ed': {'nouveau', 'amdgpu', 'xe'}}
MODULES = Path('usr/lib/tilo/initramfs-modules.txt')
OMITTED_DRIVERS = Path('usr/lib/tilo/initramfs-omitted-drivers.txt')
RUNTIME_FIELDS = ('Cmd', 'Entrypoint', 'Env', 'User', 'WorkingDir', 'StopSignal',
                  'Volumes', 'ExposedPorts', 'Healthcheck', 'Shell', 'OnBuild')
DIAGNOSTIC_LIMIT = 2048


class CommandFailure(ValueError):
    def __init__(self, step, reason, output=b''):
        super().__init__(reason)
        self.step = step
        self.reason = reason
        self.output = bytes(output)


def command_step(arguments):
    executable = Path(arguments[0]).name if arguments else 'unknown'
    operations = {'build', 'container', 'create', 'image', 'mount', 'pull', 'push',
                  'rm', 'start', 'unmount'}
    operation = next((value for value in arguments[1:] if value in operations), None)
    return executable + (':' + operation if operation else '')


def sanitized_diagnostic(value, limit=DIAGNOSTIC_LIMIT):
    """Bound one diagnostic while removing common credential representations."""
    text = value.decode('utf-8', 'replace') if isinstance(value, bytes) else str(value)
    text = re.sub(r'-----BEGIN [^-\r\n]*PRIVATE KEY-----.*?-----END [^-\r\n]*PRIVATE KEY-----',
                  '[REDACTED PRIVATE KEY]', text, flags=re.DOTALL | re.IGNORECASE)
    text = re.sub(r'(?i)Bearer\s+[^\s,;]+', 'Bearer [REDACTED]', text)
    text = re.sub(r'\bk8s-aws-v1\.[A-Za-z0-9._~+/-]+={0,2}', '[REDACTED AWS TOKEN]', text)
    text = re.sub(r'(?i)\b(authorization)\s*:\s*[^\r\n]+',
                  lambda match: match.group(1) + ': [REDACTED]', text)
    credential_name = (r'(?:[A-Za-z0-9]+_)*'
                       r'(?:password|passwd|token|secret|authorization|credential|'
                       r'api_key|access_key|private_key)'
                       r'(?:_[A-Za-z0-9]+)*')
    credential_value = r'(?:"[^"]*"|\'[^\']*\'|[^\s,;]+)'
    text = re.sub(r'(?i)\b(' + credential_name + r')\s*[:=]\s*' + credential_value,
                  lambda match: match.group(1) + '=[REDACTED]', text)
    text = re.sub(r'([A-Za-z][A-Za-z0-9+.-]*://)[^/@\s:]+:[^/@\s]+@', r'\1[REDACTED]@', text)
    text = ''.join(character if character in '\n\t' or 32 <= ord(character) < 127 else '\ufffd'
                   for character in text)
    if len(text) > limit:
        text = '[truncated] ' + text[-(limit - len('[truncated] ')):]
    return text


def dracut_failure_diagnostic(log):
    safe_log = sanitized_diagnostic(log, limit=8 * 1024 * 1024)
    errors = '\n'.join(line for line in safe_log.splitlines()
                       if re.search(r'dracut\[[EW]\]|\b(?:error|failed|cannot)\b|ldconfig:',
                                    line, re.IGNORECASE))
    return ('warnings=' + sanitized_diagnostic(errors, limit=1024)
            + '\nstart=' + safe_log[:1024]
            + '\ntail=' + safe_log[-1024:])


def dracut_succeeded(state):
    return (state.get('Status') in ('exited', 'stopped')
            and type(state.get('ExitCode')) is int and state['ExitCode'] == 0)


def failure_diagnostic(error):
    detail = {'schema': 1, 'error_type': type(error).__name__,
              'reason': sanitized_diagnostic(error)}
    if isinstance(error, CommandFailure):
        detail.update(step=error.step, output_tail=sanitized_diagnostic(error.output))
    return 'development rootfs transformation failed: ' + json.dumps(
        detail, sort_keys=True, separators=(',', ':')) + '\n'


def bounded_process(arguments, environment, limit=8 * 1024 * 1024, timeout=900):
    """Drain one bounded output pipe; abort the owned process group on overflow."""
    with subprocess.Popen(arguments, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          env=environment, start_new_session=True) as process:
        try:
            output = bytearray()
            deadline = time.monotonic() + timeout
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise CommandFailure(command_step(arguments),
                                             'container command exceeded time bound', output)
                    chunk = os.read(process.stdout.fileno(), min(65536, limit + 1 - len(output)))
                    if not chunk:
                        break
                    output.extend(chunk)
                    if len(output) > limit:
                        raise CommandFailure(command_step(arguments),
                                             'container command exceeded output bound', output[-limit:])
            try:
                returncode = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired as error:
                raise CommandFailure(command_step(arguments),
                                     'container command exceeded time bound', output) from error
            if returncode:
                raise CommandFailure(command_step(arguments),
                                     f'container command failed (exit {returncode})', output)
            return output.decode('utf-8')
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            raise


def command(arguments):
    environment = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'HOME': '/root'}
    environment.update({name: os.environ[name]
                        for name in importer.rootfs.LOCAL_STORAGE_ENVIRONMENT if name in os.environ})
    return bounded_process(['/usr/bin/podman', '--remote=false', *arguments], environment)


@contextmanager
def mounted(container):
    location = command(['mount', container]).strip()
    try:
        root = Path(location)
        if not root.is_absolute() or root.resolve() != root or root == Path('/') or '\n' in location:
            raise ValueError('invalid working-container mount')
        hardware.trusted_path(root, Path(), regular=False)
        yield root
    finally:
        command(['unmount', container])


def omitted_drivers(profile):
    # Match the production image dracut policy and the initramfs completion
    # verifier: Ext4 is a supported root; XFS remains rescue-only.
    return sorted({'xfs'} | BOARD_OMITTED_DRIVERS.get(profile, set()))


def create_arguments(image_id, kver, epoch, hooks, profile=None):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise ValueError('immutable image ID required')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,254}', kver):
        raise ValueError('unsafe kernel version')
    if type(epoch) is not int or not 0 <= epoch <= 253402300799:
        raise ValueError('invalid source epoch')
    return ['--hooks-dir', str(hooks), 'create', '--pull=never', '--network=none',
        '--http-proxy=false', '--image-volume=ignore', '--systemd=false', '--user=0',
        '--cap-drop=all', '--security-opt=no-new-privileges', '--unsetenv-all',
        '--env', 'PATH=/usr/sbin:/usr/bin:/sbin:/bin', '--env', 'LANG=C.UTF-8',
        '--env', f'SOURCE_DATE_EPOCH={epoch}', '--entrypoint=/usr/bin/dracut', image_id,
        '--force', '--reproducible', '--no-hostonly', '--no-hostonly-cmdline', '-v',
        '--kver', kver, '--add', ' '.join(sorted(REQUIRED_MODULES)),
        '--omit', ' '.join(sorted(OMITTED_MODULES)),
        '--omit-drivers', ' '.join(omitted_drivers(profile)),
        '--add-drivers', 'ext4',
        '--compress', 'zstd',
        f'/usr/lib/modules/{kver}/initramfs.img']


def module_evidence(log):
    modules = set(re.findall(r'Including module: ([a-z0-9_-]+)', log))
    if not REQUIRED_MODULES <= modules:
        raise ValueError('dracut did not report every required module')
    if modules & OMITTED_MODULES:
        raise ValueError('dracut included an omitted local-root module')
    return ('\n'.join(sorted(modules)) + '\n').encode()


def create_owned(arguments, cidfile):
    """Keep the owned ID even if create's stdout is noisy or it is interrupted."""
    position = arguments.index('create') + 1
    arguments = arguments[:position] + ['--cidfile', str(cidfile)] + arguments[position:]
    try:
        command(arguments)
        container = cidfile.read_text().strip()
        if not re.fullmatch('[0-9a-f]{64}', container):
            raise ValueError('invalid created container identity')
        return container
    except BaseException:
        if cidfile.is_file():
            container = cidfile.read_text().strip()
            if re.fullmatch('[0-9a-f]{64}', container):
                command(['rm', '--force', container])
        raise


def payload_paths(root, kver):
    directory = hardware.DEVELOPMENT_PATH
    paths = [Path('usr/share/tilo') / name for name in rebind.installer.PRODUCTION_FILES]
    paths += [directory / name for name in (*completion.trust.FILES, 'manifest.json',
                                          'origin.json', 'provenance.json')]
    paths += [Path('usr/share/tilo/trust/tilo-main-registry-owner.pub'),
              Path('etc/containers/policy.json'),
              Path('etc/containers/registries.d/tilo-development.yaml'),
              Path('etc/containers/registries.conf.d/99-tilo-development.conf'),
              completion.VERIFIED, MODULES, OMITTED_DRIVERS, Path('usr/lib/modules') / kver / 'initramfs.img']
    if hardware.read_profile(root) is not None:
        paths.append(hardware.PROFILE_PATH)
    return paths


def snapshot_payload(root, destination, kver):
    hashes = {}
    for relative in payload_paths(root, kver):
        source = hardware.trusted_path(root, relative, regular=True)
        limit = 512 * 1024 * 1024 if relative.name == 'initramfs.img' else 1024 * 1024
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        # Existing snapshot validates regular-file identity and bounds reads.
        data = importer.rootfs.loader.snapshot(source, target, limit)
        target.chmod(0o644)
        hashes[str(relative)] = hashlib.sha256(data).hexdigest()
    return hashes


def transform(args):
    # Validate epoch before any pull or container creation.
    if type(args.source_date_epoch) is not int or not 0 <= args.source_date_epoch <= 253402300799:
        raise ValueError('invalid source epoch')
    with importer.verified_rootfs(args) as imported:
        original = json.loads(importer.rootfs.command(
            ['podman', 'image', 'inspect', imported.image_id]))[0]
        if original.get('Config', {}).get('OnBuild'):
            raise ValueError('base ONBUILD instructions would execute image code during projection')
        with tempfile.TemporaryDirectory(prefix='tilo-rebind-rootfs-', dir='/var/lib') as temporary:
            work = Path(temporary)
            public = work / 'public'
            importer.public_snapshot(args.new_public_bundle, public, args.new_manifest_sha256)
            hooks = work / 'hooks'
            hooks.mkdir()
            kver = imported.receipt['kernel_version']
            container = create_owned(create_arguments(imported.image_id, kver,
                                      args.source_date_epoch, hooks,
                                      imported.receipt.get('hardware_profile')), work / 'container-id')
            try:
                info = json.loads(command(['container', 'inspect', container]))[0]
                if info.get('Image', '').removeprefix('sha256:') != imported.image_id.removeprefix('sha256:') or info.get('Mounts') != []:
                    raise ValueError('working container source or mounts differ')
                with mounted(container) as root:
                    replacement = rebind.replace(root, disposable=True,
                        old_bundle=imported.public_bundle, old_manifest=args.manifest_sha256,
                        old_target=args.target, new_bundle=public, new_manifest=args.new_manifest_sha256,
                        new_target=args.new_target, new_owner=args.new_owner,
                        architecture=args.architecture, generation=args.generation)
                log = command(['start', '--attach', container])
                state = json.loads(command(['container', 'inspect', container]))[0]['State']
                if not dracut_succeeded(state):
                    raise ValueError(f"dracut status={state.get('Status')!r} "
                                     f"exit={state.get('ExitCode')!r}: "
                                     + dracut_failure_diagnostic(log))
                modules = module_evidence(log)
                context = work / 'context'
                context.mkdir()
                with mounted(container) as root:
                    proof = completion.complete(root, disposable=True,
                        marker_sha256=replacement['pending_marker_sha256'], public_bundle=public,
                        manifest_sha256=args.new_manifest_sha256, target=args.new_target,
                        architecture=args.architecture, generation=args.generation)
                    module_path = hardware.trusted_path(root, MODULES, regular=True)
                    # Replace, never write through a pre-existing hard link.
                    with tempfile.NamedTemporaryFile(dir=module_path.parent, delete=False) as staged:
                        staged.write(modules)
                        staged_path = Path(staged.name)
                    try:
                        staged_path.chmod(0o644)
                        staged_path.replace(module_path)
                    finally:
                        staged_path.unlink(missing_ok=True)
                    omitted_path = hardware.trusted_path(root, OMITTED_DRIVERS, regular=True)
                    with tempfile.NamedTemporaryFile(dir=omitted_path.parent, delete=False) as staged:
                        staged.write(('\n'.join(omitted_drivers(
                            imported.receipt.get('hardware_profile'))) + '\n').encode())
                        staged_path = Path(staged.name)
                    try:
                        staged_path.chmod(0o644)
                        staged_path.replace(omitted_path)
                    finally:
                        staged_path.unlink(missing_ok=True)
                    hashes = snapshot_payload(root, context / 'payload', kver)
                (context / 'Containerfile').write_text(
                    'FROM raw-rootfs\nCOPY payload/ /\n')
                iidfile = work / 'image-id'
                command(['build', '--network=none', '--pull=never', '--no-cache', '--format=oci',
                         '--build-context', 'raw-rootfs=container-image://' + imported.image_id,
                         '--source-date-epoch', str(args.source_date_epoch), '--rewrite-timestamp',
                         '--iidfile', str(iidfile), '-f', str(context / 'Containerfile'), str(context)])
                result = iidfile.read_text().strip()
                if not re.fullmatch(r'sha256:[0-9a-f]{64}', result):
                    raise ValueError('invalid output image ID')
                final_info = importer.rootfs.command(['podman', 'image', 'inspect', result])
                importer.rootfs.identity(final_info, result, args.architecture, args.revision)
                final = json.loads(final_info)[0]
                if any(original.get('Config', {}).get(field) != final.get('Config', {}).get(field)
                       for field in RUNTIME_FIELDS):
                    raise ValueError('runtime configuration changed during public payload projection')
                with importer.rootfs.mounted(result) as root:
                    importer.source.mounted_identity(root, args.new_target, args.new_manifest_sha256, args.architecture)
                    if importer.retention.raw_kernel_pair(root) != kver:
                        raise ValueError('output kernel version differs')
                    if completion.file_hash(hardware.trusted_path(root, Path('usr/lib/modules') / kver / 'vmlinuz', regular=True)) != proof['kernel_sha256']:
                        raise ValueError('output kernel bytes differ')
                    generation = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
                    if completion.bounded(generation, 32) not in (str(args.generation).encode(), f'{args.generation}\n'.encode()):
                        raise ValueError('output generation differs')
                    for relative, digest in hashes.items():
                        if completion.file_hash(hardware.trusted_path(root, Path(relative), regular=True)) != digest:
                            raise ValueError('output payload differs')
                    image = hardware.trusted_path(root, Path('usr/lib/modules') / kver / 'initramfs.img', regular=True)
                    artifact = completion.verify_artifact(image, completion.trust.read_public(public / 'tpm2-pcr-signing.public.pem'))
                    if artifact != proof['artifact']:
                        raise ValueError('output initramfs proof differs')
                return {'schema': 1, 'purpose': 'development-rootfs-public-trust-rebound',
                        'source_id': imported.image_id, 'rootfs_id': result,
                        'manifest_sha256': args.new_manifest_sha256, 'target_update_ref': args.new_target,
                        'generation': args.generation, 'architecture': args.architecture,
                        'source_date_epoch': args.source_date_epoch, 'payload_sha256': hashes,
                        'initramfs': proof, 'installable': False, 'boot_proven': False, 'sealed': False}
            finally:
                command(['rm', '--force', container])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('receipt', 'public-bundle', 'new-public-bundle'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('revision', 'architecture', 'manifest-sha256', 'target',
                 'new-manifest-sha256', 'new-target', 'new-owner'):
        parser.add_argument('--' + name, required=True)
    for name in ('generation', 'source-date-epoch'):
        parser.add_argument('--' + name, required=True, type=int)
    try:
        print(json.dumps(transform(parser.parse_args()), sort_keys=True, indent=2))
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, failure_diagnostic(error))


if __name__ == '__main__':
    main()
