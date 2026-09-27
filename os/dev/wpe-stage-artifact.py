#!/usr/bin/env python3
"""Export/import completed development WPE stages; trusted registry writers only.

This is dependency provenance, not a production signature. Import never builds
or runs an image. Missing or mismatched evidence fails before OS compilation.
"""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import tempfile

ARCHES = {'x86_64': ('amd64', 62), 'aarch64': ('arm64', 183)}
PURPOSE = 'tilo-development-wpe-completed-stage'
DIGEST = r'sha256:[0-9a-f]{64}'
RECIPES = ('os/Containerfile.wpe', 'os/dev/build-wpe.sh')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and before.st_size <= limit, 'bounded regular input required')
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            data = stream.read(limit + 1)
        after = os.fstat(descriptor)
        require(len(data) <= limit and (before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'input changed or exceeds size bound')
        return data
    finally:
        os.close(descriptor)


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    def invalid_constant(_):
        raise ValueError('nonfinite JSON value')
    return json.loads(data, object_pairs_hook=pairs, parse_constant=invalid_constant)


def file_hash(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_size <= limit, 'bounded regular input required')
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            digest = hashlib.sha256()
            count = 0
            while chunk := stream.read(1024 * 1024):
                count += len(chunk)
                require(count <= limit, 'input exceeds size bound')
                digest.update(chunk)
        after = os.fstat(descriptor)
        require((info.st_size, info.st_mtime_ns, info.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'input changed while reading')
        return digest.hexdigest()
    finally:
        os.close(descriptor)


def dependency_inputs(repo, assets, architecture, epoch, buildroot):
    require(architecture in ARCHES, 'unsupported native architecture')
    require(bool(re.fullmatch(r'0|[1-9][0-9]{0,11}', epoch)) and
            int(epoch) <= 253402300799, 'invalid dependency epoch')
    require(bool(re.fullmatch(r'registry\.tilo\.so/tilo/tilo-wpe-buildroot@' + DIGEST, buildroot)),
            'immutable WPE buildroot required')
    recipes = {name: read_bounded(repo / name, 2 * 1024 * 1024) for name in RECIPES}
    script = recipes[RECIPES[1]].decode()
    pinned = []
    for variable in ('LIBWPE', 'FDO', 'WPEWK'):
        names = re.findall(r'^' + variable + r'=([a-z0-9.-]+\.tar\.xz)$', script, re.MULTILINE)
        hashes = re.findall(r'^' + variable + r'_SHA256=([0-9a-f]{64})$', script, re.MULTILINE)
        require(len(names) == len(hashes) == 1, 'exactly one pinned WPE archive required')
        actual = file_hash(assets / names[0], 512 * 1024 * 1024)
        require(actual == hashes[0], 'WPE archive checksum mismatch')
        pinned.append({'name': names[0], 'sha256': actual})
    return dict(schema=1, purpose=PURPOSE, architecture=architecture, epoch=epoch,
                wpe_buildroot=buildroot,
                recipes={name: sha(data) for name, data in recipes.items()},
                assets=sorted(pinned, key=lambda value: value['name']))


def repository(architecture):
    require(architecture in ARCHES, 'unsupported architecture')
    return 'registry.tilo.so/tilo/tilo-development-wpe-stage-' + architecture


def validate_receipt(receipt, inputs):
    require(type(receipt.get('schema')) is int and receipt['schema'] == 1 and
            receipt.get('purpose') == PURPOSE and receipt.get('production_signed') is False,
            'development stage receipt required')
    require(receipt.get('inputs') == inputs and receipt.get('dependency_key') == sha(canonical(inputs)),
            'dependency receipt mismatch')
    require(bool(re.fullmatch(re.escape(repository(inputs['architecture'])) + '@' + DIGEST,
                             receipt.get('reference', ''))), 'immutable development stage reference required')
    require(bool(re.fullmatch(DIGEST, receipt.get('config_digest', ''))), 'config digest required')
    provenance = receipt.get('build', {})
    require(bool(re.fullmatch(DIGEST, provenance.get('source_completed_stage_id', ''))),
            'completed source stage identity required')
    require(type(provenance.get('jobs')) is int and 1 <= provenance['jobs'] <= 64,
            'invalid producer parallelism')
    require(bool(re.fullmatch('[0-9a-f]{40}', provenance.get('revision', ''))), 'source revision required')
    for field in ('run_id', 'attempt'):
        require(bool(re.fullmatch('[1-9][0-9]*', provenance.get(field, ''))), 'CI run identity required')


def manifest_identity(raw, config_raw, reference, architecture):
    require(len(raw) <= 4 * 1024 * 1024 and len(config_raw) <= 4 * 1024 * 1024, 'metadata too large')
    require(reference.rsplit('@', 1)[-1] == 'sha256:' + sha(raw), 'registry manifest digest mismatch')
    manifest, config = strict_json(raw), strict_json(config_raw)
    require(manifest.get('schemaVersion') == 2 and 'manifests' not in manifest and
            manifest.get('mediaType') in ('application/vnd.oci.image.manifest.v1+json',
                                         'application/vnd.docker.distribution.manifest.v2+json'),
            'single native image manifest required')
    config_digest = 'sha256:' + sha(config_raw)
    require(manifest.get('config', {}).get('digest') == config_digest, 'registry config digest mismatch')
    require(config.get('architecture') == ARCHES[architecture][0] and config.get('os') == 'linux',
            'native Linux image required')
    require(isinstance(manifest.get('layers'), list) and manifest['layers'], 'completed image layers required')
    return config_digest


def command(*arguments):
    try:
        return subprocess.check_output(arguments, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError:
        raise ValueError('external verification command failed') from None


def native_host(architecture):
    require(platform.system() == 'Linux' and platform.machine() == architecture and os.getuid() == 0,
            'already-root native Linux required')
    require(not any(os.environ.get(name) for name in
                    ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST')),
            'remote container engines forbidden')


def inspect_local(reference, architecture, expected_id=None):
    images = strict_json(command('podman', '--remote=false', 'image', 'inspect', reference))
    require(isinstance(images, list) and len(images) == 1, 'one local image required')
    image = images[0]
    identity = 'sha256:' + image.get('Id', '').removeprefix('sha256:')
    require(bool(re.fullmatch(DIGEST, identity)) and image.get('Architecture') == ARCHES[architecture][0]
            and image.get('Os') == 'linux', 'local native image identity mismatch')
    if expected_id is not None:
        require(identity == expected_id, 'local config identity differs')
    if '@' in reference:
        require(reference in image.get('RepoDigests', []), 'pulled registry identity differs')
    return identity


@contextmanager
def mounted(identity):
    path = Path(command('podman', '--remote=false', 'image', 'mount', identity).decode().strip())
    try:
        require(path.is_absolute() and path.is_dir(), 'invalid image mount')
        yield path
    except BaseException:
        subprocess.run(['podman', '--remote=false', 'image', 'unmount', identity], check=False,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        raise
    else:
        command('podman', '--remote=false', 'image', 'unmount', identity)


def payload_identity(root, architecture):
    payload = root / 'wpe'
    require(payload.is_dir() and not payload.is_symlink(), 'completed WPE payload missing')
    result = {}
    for relative in ('usr/lib64/libWPEWebKit-2.0.so', 'usr/lib64/pkgconfig/wpe-webkit-2.0.pc',
                     'usr/libexec/wpe-webkit-2.0/WPEWebProcess'):
        path = payload / relative
        resolved = path.resolve(strict=True)
        require(resolved.is_relative_to(payload.resolve()), 'WPE payload link escapes image')
        require(resolved.is_file() and resolved.stat().st_size > 0, 'WPE payload is empty')
        if not relative.endswith('.pc'):
            with resolved.open('rb') as stream:
                header = stream.read(20)
            require(header[:6] == b'\x7fELF\x02\x01' and len(header) == 20 and
                    int.from_bytes(header[18:20], 'little') == ARCHES[architecture][1],
                    'WPE payload native ELF differs')
        result[relative] = file_hash(resolved, 2 * 1024 * 1024 * 1024)
    return result


def registry_identity(reference, architecture):
    return manifest_identity(command('skopeo', 'inspect', '--tls-verify=true', '--raw', 'docker://' + reference),
                             command('skopeo', 'inspect', '--tls-verify=true', '--config', '--raw', 'docker://' + reference),
                             reference, architecture)


def import_stage(receipt, inputs):
    validate_receipt(receipt, inputs)
    architecture = inputs['architecture']
    native_host(architecture)
    reference = receipt['reference']
    config = registry_identity(reference, architecture)
    require(config == receipt['config_digest'], 'receipt config differs')
    command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/' + ARCHES[architecture][0], reference)
    identity = inspect_local(reference, architecture, config)
    with mounted(identity) as root:
        require(payload_identity(root, architecture) == receipt.get('payload'), 'completed payload differs')
    return identity


def _export_stage(identity, inputs, jobs, revision, run_id, attempt, *, source_completed_stage_id=None):
    architecture = inputs['architecture']
    native_host(architecture)
    require(bool(re.fullmatch(DIGEST, identity)), 'full completed local image ID required')
    receipt = dict(schema=1, purpose=PURPOSE, production_signed=False, inputs=inputs,
                   dependency_key=sha(canonical(inputs)),
                   reference=repository(architecture) + '@sha256:' + '0' * 64,
                   config_digest=identity,
                   build=dict(jobs=jobs, revision=revision, run_id=run_id, attempt=attempt,
                              source_completed_stage_id=source_completed_stage_id or identity))
    validate_receipt(receipt, inputs)
    inspect_local(identity, architecture, identity)
    with mounted(identity) as root:
        receipt['payload'] = payload_identity(root, architecture)
    tag = repository(architecture) + f':candidate-{revision}-{run_id}-{attempt}'
    with tempfile.TemporaryDirectory(prefix='tilo-wpe-export-') as temporary:
        digest_path = Path(temporary) / 'manifest.digest'
        command('podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile', str(digest_path), identity, 'docker://' + tag)
        digest = read_bounded(digest_path, 128).decode().strip()
        require(bool(re.fullmatch(DIGEST, digest)), 'push did not return manifest digest')
        receipt['reference'] = repository(architecture) + '@' + digest
    receipt['config_digest'] = registry_identity(receipt['reference'], architecture)
    require(receipt['config_digest'] == identity, 'publication changed image config')
    import_stage(receipt, inputs)
    return receipt


def _build_stage(args):
    spec = importlib.util.spec_from_file_location('prepare_wpe_stage',
        Path(__file__).with_name('prepare-wpe-stage.py'))
    producer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(producer)
    return producer.prepare(args)


def _project_payload(identity, inputs):
    """Discard the completed compiler/toolchain layers without executing code."""
    architecture = inputs['architecture']
    native_host(architecture)
    require(bool(re.fullmatch(DIGEST, identity)), 'full completed image ID required')
    inspect_local(identity, architecture, identity)
    with mounted(identity) as root:
        expected = payload_identity(root, architecture)
    with tempfile.TemporaryDirectory(prefix='tilo-wpe-payload-') as temporary:
        context = Path(temporary)
        (context / 'Containerfile').write_text(
            'FROM scratch\nCOPY --from=compiled-wpe /wpe/ /wpe/\n')
        iid = context / 'payload.iid'
        command('podman', '--remote=false', 'build', '--pull=never', '--network=none',
                '--skip-unused-stages=true', '--source-date-epoch', inputs['epoch'], '--rewrite-timestamp',
                '--build-context', f'compiled-wpe=container-image://{identity}',
                '--iidfile', str(iid), str(context))
        projected = read_bounded(iid, 128).decode().strip()
    inspect_local(projected, architecture, projected)
    with mounted(projected) as root:
        require(payload_identity(root, architecture) == expected, 'payload projection changed bytes')
    return projected


def build_export(args):
    """Attest only this invocation's build; receipts require trusted producer CI."""
    inputs = dependency_inputs(args.repo, args.assets, args.architecture, args.epoch, args.wpe)
    require(bool(re.fullmatch('[0-9a-f]{40}', args.revision or '')), 'source revision required')
    for value in (args.run_id, args.attempt):
        require(bool(re.fullmatch('[1-9][0-9]*', value or '')), 'CI run identity required')
    verify_source(args.repo, args.revision)
    identity = _build_stage(args)
    verify_source(args.repo, args.revision)
    require(dependency_inputs(args.repo, args.assets, args.architecture, args.epoch, args.wpe) == inputs,
            'dependency inputs changed during compilation')
    projected = _project_payload(identity, inputs)
    return _export_stage(projected, inputs, args.jobs, args.revision, args.run_id, args.attempt,
                         source_completed_stage_id=identity)


def verify_source(repo, revision):
    require(command('git', '-C', str(repo), 'rev-parse', 'HEAD').decode().strip() == revision,
            'producer checkout revision differs')
    require(Path(command('git', '-C', str(repo), 'rev-parse', '--show-toplevel').decode().strip()).resolve()
            == repo.resolve(), 'producer repository root differs')
    command('git', '-C', str(repo), 'ls-files', '--error-unmatch', *RECIPES)
    require(not command('git', '-C', str(repo), 'status', '--porcelain', '--untracked-files=no').strip(),
            'producer tracked checkout is dirty')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('build-export', 'import'))
    for name in ('architecture', 'epoch', 'wpe'):
        parser.add_argument('--' + name, required=True)
    for name in ('repo', 'assets'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--cache')
    for name in ('revision', 'run-id', 'attempt'):
        parser.add_argument('--' + name)
    args = parser.parse_args()
    try:
        inputs = dependency_inputs(args.repo, args.assets, args.architecture, args.epoch, args.wpe)
        if args.mode == 'import':
            require(args.receipt is not None, 'bounded receipt required')
            print(import_stage(strict_json(read_bounded(args.receipt, 65536)), inputs))
        else:
            print(json.dumps(build_export(args), sort_keys=True, indent=2))
    except (OSError, ValueError, TypeError, KeyError, subprocess.CalledProcessError):
        parser.exit(1, 'WPE stage artifact: input validation or artifact operation failed\n')


if __name__ == '__main__':
    main()
