"""Retain and import a complete native builder payload without recompiling it.

Receipts require an independent SHA256. The payload includes its complete
inventory; neither producer nor consumer executes artifact code. Key scanning
covers PEM private keys and Tilo's serialized publisher identities, not every
possible secret format.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import struct
import tempfile

spec = importlib.util.spec_from_file_location('registry_artifact', Path(__file__).with_name('registry_artifact.py'))
tools = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tools)
require = tools.require
METADATA = 'out/tilo-native-builder-artifact.json'
ROOTS = ('out', 'usr/lib/tilo-runtime', 'usr/share/tilo-apps')
ANCESTORS = {'usr', 'usr/lib', 'usr/share'}
IMAGE_TREE = re.compile(r'usr/share/tilo-apps/pkgs/[0-9a-f]{64}/.+')
PURPOSE = 'tilo-development-native-builder'
REQUIRED = ('shell fs tilo-model-manager tilo-speech-worker tilo-model-gateway '
            'tilo-relay tilo-relay-admin tilo-enroll tilo-home-peer-control tilo-greeter tilo-installer '
            'tilo-runtime-mcp tilo-runtime-bundle tilo-launcher tilo-app-host tilo-unlock-helper tilo-rollback tilo-crypto tilo-image-trust tilo-device-key-init '
            'tilo tilo-app-package tilo-install tilo-cli-shim tilo-ush ush-editor/bash-editor.so tilo-system-extension-manager '
            'tilo-projector tilo-windows-vm-control tilo-windows-rail tilo-windows-runtime').split()


def immutable(value):
    require(isinstance(value, str) and re.fullmatch(r'(?:registry|code)\.tilo\.so/tilo/[a-z0-9][a-z0-9._-]*@sha256:[0-9a-f]{64}', value),
            'immutable registry input required')
    return value


def expected_inputs(repo, revision, architecture, base, rpms, epoch, jobs=2,
                    runtime_version='2026.07', engine_runtime_version='2026.10.1-chromium'):
    repo = Path(repo)
    require(re.fullmatch('[0-9a-f]{40}', revision) and architecture in tools.ARCHES, 'source/native architecture required')
    require(tools.command('git', '-C', str(repo), 'rev-parse', 'HEAD').decode().strip() == revision,
            'builder source revision differs')
    require(not tools.command('git', '-C', str(repo), 'status', '--porcelain', '--untracked-files=all').strip(),
            'builder source checkout must be clean')
    require(type(epoch) is int and 0 <= epoch <= 253402300799 and type(jobs) is int and 1 <= jobs <= 128,
            'bounded epoch/jobs required')
    for version in (runtime_version, engine_runtime_version):
        require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,63}', version), 'invalid runtime version')
    return {'schema': 1, 'revision': revision, 'architecture': architecture,
            'base': immutable(base), 'rpms': immutable(rpms), 'epoch': epoch, 'jobs': jobs,
            'runtime_version': runtime_version, 'engine_runtime_version': engine_runtime_version,
            'browser': True, 'prebuilt_apps': False,
            'prebuilt_desktop': False, 'app_collection_publisher': '',
            'containerfile_sha256': tools.file_hash(repo / 'os/Containerfile', 2 * 1024 * 1024)}


def repository(architecture):
    require(architecture in tools.ARCHES, 'unsupported architecture')
    return 'registry.tilo.so/tilo/tilo-development-native-builder-' + architecture


def file_record(path):
    """Hash while scanning bounded chunks, avoiding whole executable reads."""
    digest, tail, total = hashlib.sha256(), b'', 0
    with path.open('rb') as stream:
        header = stream.read(64)
        stream.seek(0)
        while block := stream.read(1024 * 1024):
            total += len(block)
            require(total <= 4 * 1024**3, 'payload file exceeds bound')
            sample = tail + block
            require(not re.search(rb'-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----\s+[A-Za-z0-9+/=\s]+-----END (?:[A-Z0-9]+ )?PRIVATE KEY-----', sample)
                    and not re.search(rb'"secret_key_hex"\s*:\s*"[0-9a-fA-F]{64}"', sample),
                    'private signing material in builder payload')
            tail = sample[-65536:]
            digest.update(block)
    return {'size': total, 'sha256': digest.hexdigest()}, header


def inventory(root, inputs):
    root = Path(root)
    result = {}
    for path in sorted(root.rglob('*')):
        name = path.relative_to(root).as_posix()
        require(name in ANCESTORS or any(name == p or name.startswith(p + '/') for p in ROOTS),
                'unexpected builder artifact path')
        info = path.lstat()
        require(info.st_uid == 0 and info.st_gid == 0
                and (stat.S_ISLNK(info.st_mode) or not info.st_mode & 0o6002),
                'builder payload ownership or mode differs')
        if name == METADATA:
            require(stat.S_ISREG(info.st_mode), 'artifact metadata must be regular')
            continue
        row = {'mode': stat.S_IMODE(info.st_mode)}
        if stat.S_ISLNK(info.st_mode):
            # Application trees never hold links; an environment image's tree
            # (the env-runtime) does, signed with it, and its targets are read
            # inside an environment, never on this host. Record them verbatim.
            if not IMAGE_TREE.match(name):
                require(not path.readlink().is_absolute(), 'absolute artifact symlink forbidden')
                path.resolve(strict=True).relative_to(root)
            row.update(type='symlink', target=str(path.readlink()))
        elif stat.S_ISDIR(info.st_mode):
            row['type'] = 'directory'
        else:
            require(stat.S_ISREG(info.st_mode), 'regular payload file required')
            data, header = file_record(path)
            if header.startswith(b'\x7fELF'):
                require(len(header) == 64 and header[4:7] == b'\x02\x01\x01'
                        and struct.unpack_from('<H', header, 16)[0] in (2, 3)
                        and struct.unpack_from('<H', header, 18)[0] == tools.ARCHES[inputs['architecture']][1],
                        'foreign or malformed native ELF in builder payload')
            row.update(type='file', **data)
        result[name] = row
        require(len(result) <= 100000, 'builder payload file count exceeds bound')
    engine = f"usr/lib/tilo-runtime/{inputs['engine_runtime_version']}/lib/chromium/tilo-chromium-host"
    for name in (*('out/' + name for name in REQUIRED), engine):
        row = result.get(name, {})
        require(row.get('type') == 'file' and row.get('mode', 0) & 0o111 and row.get('size', 0),
                'required native builder output missing: ' + name)
        with (root / name).open('rb') as stream:
            require(stream.read(4) == b'\x7fELF', 'required builder executable is not ELF')
    for name in ('runtime-bundle-publisher.hex', 'app-publisher.hex'):
        require(re.fullmatch(rb'[0-9a-f]{64}\n?', tools.read_bounded(root / 'out' / name, 128)),
                'builder publisher identity missing')
    # The runtime is baked under its release and ABI digest.
    require(len(list((root / 'usr/lib/tilo-runtime').glob(inputs['runtime_version'] + '+native.*'))) == 1
            and (root / 'usr/share/tilo-apps/registry.toml').is_file(), 'runtime or app store missing')
    return result


def check_payload(identity, inputs, metadata_sha256):
    with tools.mounted(identity) as root:
        raw = tools.read_bounded(root / METADATA, 16 * 1024 * 1024)
        require(tools.sha(raw) == metadata_sha256, 'builder metadata hash differs')
        metadata = tools.strict_json(raw)
        require(set(metadata) == {'schema', 'purpose', 'inputs', 'inventory'}
                and metadata['schema'] == 1 and metadata['purpose'] == PURPOSE
                and metadata['inputs'] == inputs, 'builder metadata inputs differ')
        require(metadata['inventory'] == inventory(root, inputs), 'builder payload inventory differs')


def import_artifact(receipt_path, receipt_sha256, inputs):
    require(re.fullmatch('[0-9a-f]{64}', receipt_sha256), 'independent builder receipt hash required')
    raw = tools.read_bounded(Path(receipt_path), 65536)
    require(tools.sha(raw) == receipt_sha256, 'builder receipt hash differs')
    receipt = tools.strict_json(raw)
    require(set(receipt) == {'schema', 'purpose', 'inputs', 'reference', 'config_digest', 'metadata_sha256',
                            'source_payload_id', 'run_id', 'attempt', 'production_signed'}, 'unexpected builder receipt schema')
    require(receipt['schema'] == 1 and receipt['purpose'] == PURPOSE and receipt['inputs'] == inputs
            and receipt['production_signed'] is False, 'builder receipt inputs differ')
    architecture = inputs['architecture']
    require(re.fullmatch(re.escape(repository(architecture)) + '@' + tools.DIGEST, receipt['reference'])
            and re.fullmatch(tools.DIGEST, receipt['config_digest'])
            and re.fullmatch('[0-9a-f]{64}', receipt['metadata_sha256']), 'invalid builder artifact identity')
    tools.native_host(architecture)
    require(tools.registry_identity(receipt['reference'], architecture) == receipt['config_digest'], 'registry config differs')
    tools.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/' + tools.ARCHES[architecture][0], receipt['reference'])
    identity = tools.inspect_local(receipt['reference'], architecture, receipt['config_digest'])
    check_payload(identity, inputs, receipt['metadata_sha256'])
    return identity


def builder_context(receipt_path, receipt_sha256, inputs):
    return 'builder=container-image://' + import_artifact(receipt_path, receipt_sha256, inputs)


def export_artifact(identity, inputs, run_id, attempt):
    architecture = inputs['architecture']
    tools.native_host(architecture)
    require(re.fullmatch(tools.DIGEST, identity), 'immutable completed payload ID required')
    require(re.fullmatch('[0-9]+', str(run_id)) and re.fullmatch('[0-9]+', str(attempt)), 'numeric run identity required')
    tools.inspect_local(identity, architecture, identity)
    with tools.mounted(identity) as root:
        require(not (root / METADATA).exists(), 'producer stage must not contain prior artifact metadata')
        files = inventory(root, inputs)
    raw = tools.canonical({'schema': 1, 'purpose': PURPOSE, 'inputs': inputs, 'inventory': files})
    require(len(raw) <= 16 * 1024 * 1024, 'builder metadata exceeds bound')
    with tempfile.TemporaryDirectory(prefix='tilo-native-builder-') as temporary:
        work = Path(temporary)
        (work / 'metadata.json').write_bytes(raw)
        (work / 'Containerfile').write_text('FROM scratch\nCOPY --from=payload /out/ /out/\n'
            'COPY --from=payload /usr/lib/tilo-runtime/ /usr/lib/tilo-runtime/\n'
            'COPY --from=payload /usr/share/tilo-apps/ /usr/share/tilo-apps/\n'
            f'COPY metadata.json /{METADATA}\n')
        iid = work / 'image.iid'
        tools.command('podman', '--remote=false', 'build', '--network=none', '--pull=never',
            '--source-date-epoch', str(inputs['epoch']), '--rewrite-timestamp',
            '--build-context', 'payload=container-image://' + identity, '--iidfile', str(iid), str(work))
        projected = tools.read_bounded(iid, 128).decode().strip()
        tools.inspect_local(projected, architecture, projected)
        check_payload(projected, inputs, tools.sha(raw))
        digest_file = work / 'manifest.digest'
        tag = repository(architecture) + f':candidate-{inputs["revision"]}-{run_id}-{attempt}'
        tools.command('podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile', str(digest_file), projected, 'docker://' + tag)
        reference = repository(architecture) + '@' + tools.read_bounded(digest_file, 128).decode().strip()
        require(tools.registry_identity(reference, architecture) == projected, 'published builder config differs')
    receipt = {'schema': 1, 'purpose': PURPOSE, 'inputs': inputs, 'reference': reference,
               'config_digest': projected, 'metadata_sha256': tools.sha(raw), 'source_payload_id': identity,
               'run_id': str(run_id), 'attempt': str(attempt), 'production_signed': False}
    require(len(tools.canonical(receipt)) <= 65536, 'builder receipt exceeds bound')
    return receipt
