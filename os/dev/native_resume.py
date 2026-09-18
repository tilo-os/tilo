"""Explicit development reconstruction of complete native payloads from partials.

The old native-builder purpose is deliberately not reused. Consumers require an
independently pinned reconstruction hash and still verify the complete inventory.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import struct

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
PURPOSE = 'tilo-development-reconstructed-native-builder'
PARTIAL = 'tilo-development-partial-native-checkpoint'
REPOSITORY = 'code.sandbox.example/qos/tilo-development-reconstructed-native-aarch64'
WINDOWS = {'tilo-windows-vm-control', 'tilo-windows-rail'}


def load_native(repo):
    spec = importlib.util.spec_from_file_location('resume_original_native', Path(repo) / 'os/dev/native_builder_artifact.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require(ok, message):
    if not ok:
        raise ValueError(message)


def replace_once(text, before, after):
    require(text.count(before) == 1, 'frozen recipe seam changed')
    return text.replace(before, after, 1)


def derive_recipe(original, retained, required):
    """Keep original package/features graph; omit only completed binary targets."""
    allowed = set(required) | {'browser'}
    require(set(retained) <= allowed and retained, 'invalid checkpoint targets')
    missing = allowed - set(retained)
    release = sorted(missing - WINDOWS)
    windows = sorted(missing & WINDOWS)
    text = original
    marker = '    && cargo clean --profile thin-app \\\n'
    text = replace_once(text, marker, marker + '    && mkdir -p target/release \\\n    && cp /tilo-resume/partial/* target/release/ \\\n')
    # A readonly named image context; no executable helper from the checkpoint.
    start = text.index('RUN --mount=type=cache,target=/build/.cargo-home')
    text = text[:start] + text[start:].replace('RUN --mount=type=cache,target=/build/.cargo-home',
        'RUN --mount=type=bind,from=tilo-resume,target=/tilo-resume,ro \\\n    --mount=type=cache,target=/build/.cargo-home', 1)
    before = '       cargo build --release ${RELEASE_ARGS} -p tilo-relay --bins ${BROWSER_PKG}'
    command = ('cargo build --release ${RELEASE_ARGS} -p tilo-relay ' +
               ' '.join('--bin ' + name for name in release) + ' ${BROWSER_PKG}') if release else 'true'
    text = replace_once(text, before, '       ' + command)
    before = '       cargo build --release -p windows-vm \\\n         --bin tilo-windows-vm-control --bin tilo-windows-rail'
    command = 'cargo build --release -p windows-vm ' + ' '.join('--bin ' + name for name in windows) if windows else 'true'
    text = replace_once(text, before, '       ' + command)
    return text, {'release': release, 'windows': windows}


def validate_partial(native, receipt, expected, builder, destination, union):
    t = native.tools
    require(set(receipt) == {'purpose', 'source_revision', 'reference', 'config_digest', 'metadata_sha256',
                            'inventory', 'installable', 'complete_native_builder', 'build_success'}, 'partial receipt schema differs')
    require(receipt['purpose'] == PARTIAL and receipt['source_revision'] == SOURCE and
            all(receipt[k] is False for k in ('installable', 'complete_native_builder', 'build_success')), 'not an honest partial checkpoint')
    require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-partial-native-aarch64@sha256:[0-9a-f]{64}', receipt['reference']), 'partial repository differs')
    require(t.registry_identity(receipt['reference'], 'aarch64') == receipt['config_digest'], 'partial registry config differs')
    t.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/arm64', receipt['reference'])
    identity = t.inspect_local(receipt['reference'], 'aarch64', receipt['config_digest'])
    with t.mounted(identity) as root:
        metadata = t.read_bounded(root / 'partial-checkpoint.json', 1024 * 1024)
        require(t.sha(metadata) == receipt['metadata_sha256'], 'partial metadata hash differs')
        value = t.strict_json(metadata)
        require(value['schema'] == 1 and value['purpose'] == PARTIAL
                and all(value[k] is False for k in ('installable', 'complete_native_builder', 'build_success'))
                and value['source_revision'] == SOURCE and value['architecture'] == 'aarch64'
                and value['run_id'] == 20866 and value['run_number'] == 7489
                and value['inventory'] == receipt['inventory'], 'partial metadata identity differs')
        inputs = value['inputs']
        require(inputs['revision'] == SOURCE and inputs['architecture'] == 'aarch64' and inputs['builder'] == builder
                and inputs['pins']['BASE_REF'] == expected['base'] and inputs['pins']['RPMS_REF'] == expected['rpms']
                and inputs['wpe_adoption_sha256'] == expected['wpe_receipt_sha256'], 'partial source/toolchain inputs differ')
        partial = root / 'partial'
        require(partial.is_dir() and not partial.is_symlink(), 'invalid partial root')
        require(set(p.name for p in partial.iterdir()) == set(receipt['inventory']), 'partial file set differs')
        for name, record in receipt['inventory'].items():
            require(name in set(native.REQUIRED) | {'browser'}, 'unexpected partial binary')
            path = partial / name
            require(path.is_file() and not path.is_symlink() and path.stat().st_mode & 0o111, 'invalid partial ELF')
            actual, header = native.file_record(path)
            require(actual == record and len(header) == 64 and header[:7] == b'\x7fELF\x02\x01\x01'
                    and struct.unpack_from('<H', header, 18)[0] == 183
                    and struct.unpack_from('<H', header, 16)[0] in (2, 3), 'partial ELF bytes/architecture differ')
            require(name not in union or union[name] == record, 'overlapping checkpoint binaries differ')
            if name not in union:
                shutil.copyfile(path, destination / name)
                (destination / name).chmod(0o755)
                require(native.file_record(destination / name)[0] == record, 'copied checkpoint differs')
                union[name] = record


def check_complete(native, identity, inputs, metadata_sha256, reconstruction_sha256):
    t = native.tools
    with t.mounted(identity) as root:
        raw = t.read_bounded(root / native.METADATA, 16 * 1024 * 1024)
        require(t.sha(raw) == metadata_sha256, 'reconstructed metadata hash differs')
        value = t.strict_json(raw)
        require(set(value) == {'schema', 'purpose', 'inputs', 'inventory', 'reconstruction'}
                and value['schema'] == 1 and value['purpose'] == PURPOSE and value['inputs'] == inputs,
                'reconstructed complete metadata differs')
        require(t.sha(t.canonical(value['reconstruction'])) == reconstruction_sha256, 'reconstruction approval differs')
        require(value['inventory'] == native.inventory(root, inputs), 'complete native inventory differs')


def import_reconstructed(native, receipt_path, receipt_sha256, inputs, approved_reconstruction_sha256):
    """Separate opt-in consumer seam; never accepted by the legacy importer."""
    t = native.tools
    require(re.fullmatch('[0-9a-f]{64}', approved_reconstruction_sha256 or ''), 'independent reconstruction approval required')
    raw = t.read_bounded(Path(receipt_path), 1024 * 1024)
    require(t.sha(raw) == receipt_sha256, 'reconstructed receipt hash differs')
    receipt = t.strict_json(raw)
    require(set(receipt) == {'schema', 'purpose', 'inputs', 'reference', 'config_digest', 'metadata_sha256',
                            'reconstruction', 'reconstruction_sha256', 'complete_native_builder',
                            'production_signed', 'installable', 'consumer_adoption_required'}, 'reconstructed receipt schema differs')
    require(receipt['schema'] == 1 and receipt['purpose'] == PURPOSE and receipt['inputs'] == inputs
            and receipt['complete_native_builder'] is True and receipt['production_signed'] is False,
            'reconstructed receipt identity differs')
    require(receipt['installable'] is False and receipt['consumer_adoption_required'] is True
            and receipt['reconstruction_sha256'] == approved_reconstruction_sha256
            and t.sha(t.canonical(receipt['reconstruction'])) == approved_reconstruction_sha256,
            'reconstruction was not independently approved')
    require(re.fullmatch(re.escape(REPOSITORY) + '@sha256:[0-9a-f]{64}', receipt['reference']), 'reconstructed repository differs')
    t.native_host('aarch64')
    require(t.registry_identity(receipt['reference'], 'aarch64') == receipt['config_digest'], 'reconstructed registry config differs')
    t.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/arm64', receipt['reference'])
    identity = t.inspect_local(receipt['reference'], 'aarch64', receipt['config_digest'])
    check_complete(native, identity, inputs, receipt['metadata_sha256'], approved_reconstruction_sha256)
    return identity
