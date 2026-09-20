#!/usr/bin/env python3
"""Explicitly adopt reviewed local WPE bytes; never attest a new engine build.

Audit and adoption receipts are independently SHA-pinned public inputs. Registry
writers and the reviewer selecting those pins are development trust boundaries.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import stat
import tempfile

spec = importlib.util.spec_from_file_location('adopt_wpe_artifact', Path(__file__).with_name('wpe-stage-artifact.py'))
wpe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wpe)
require = wpe.require
PURPOSE = 'tilo-development-wpe-local-adoption'
AUDIT_PURPOSE = 'tilo-development-wpe-local-adoption-audit'


def pinned_json(path, digest):
    require(isinstance(digest, str) and bool(re.fullmatch('[0-9a-f]{64}', digest)), 'independent SHA256 required')
    raw = wpe.read_bounded(path, 65536)
    require(wpe.sha(raw) == digest, 'independent public receipt digest differs')
    return wpe.strict_json(raw)


def source_archives(script, assets):
    result = {}
    for variable in ('LIBWPE', 'FDO', 'WPEWK'):
        names = re.findall(r'^' + variable + r'=([a-z0-9.-]+\.tar\.xz)$', script, re.MULTILINE)
        hashes = re.findall(r'^' + variable + r'_SHA256=([0-9a-f]{64})$', script, re.MULTILINE)
        require(len(names) == len(hashes) == 1, 'exact archive pins required')
        require(wpe.file_hash(assets / names[0], 512 * 1024 * 1024) == hashes[0], 'archive bytes differ')
        result[names[0]] = hashes[0]
    require(len(result) == 3, 'three distinct archives required')
    return result


def validate_audit(audit, repo, assets, architecture):
    require(isinstance(audit, dict) and set(audit) == {
        'schema', 'purpose', 'architecture', 'source_image_id', 'buildroot_image_id',
        'source_script_sha256', 'archives', 'compatible_recipes', 'original_build_epoch',
        'original_source_revision', 'original_jobs', 'compatibility_review'}, 'unexpected adoption audit schema')
    require(type(audit['schema']) is int and audit['schema'] == 1 and audit['purpose'] == AUDIT_PURPOSE,
            'development adoption audit required')
    require(architecture in wpe.ARCHES and audit['architecture'] == architecture, 'audit native architecture differs')
    require(audit['original_build_epoch'] is None and audit['original_source_revision'] is None,
            'legacy build epoch and revision must remain unknown')
    require(type(audit['original_jobs']) is int and 1 <= audit['original_jobs'] <= 64,
            'reviewed historical jobs required')
    require(audit['compatibility_review'] == 'explicit reviewer acceptance of retained recipe and current consumer',
            'explicit compatibility review required')
    for name in ('source_image_id', 'buildroot_image_id'):
        require(isinstance(audit[name], str) and bool(re.fullmatch(wpe.DIGEST, audit[name])), 'full local image IDs required')
    require(isinstance(audit['source_script_sha256'], str) and
            bool(re.fullmatch('[0-9a-f]{64}', audit['source_script_sha256'])), 'source script hash required')
    recipes = {name: wpe.read_bounded(repo / name, 2 * 1024 * 1024) for name in wpe.RECIPES}
    require(audit['compatible_recipes'] == {name: wpe.sha(data) for name, data in recipes.items()},
            'reviewed consumer recipes changed')
    require(audit['archives'] == source_archives(recipes['os/dev/build-wpe.sh'].decode(), assets),
            'reviewed source archives differ')
    return audit


def payload_digest(root, architecture):
    # Existing native ELF/package checks plus a digest of the entire copied tree.
    wpe.payload_identity(root, architecture)
    payload = root / 'wpe'
    entries, size = [], 0
    for path in sorted(payload.rglob('*')):
        info = path.lstat()
        require(len(entries) < 20000, 'payload entry bound exceeded')
        entry = [str(path.relative_to(payload)), stat.S_IMODE(info.st_mode), info.st_uid, info.st_gid]
        if stat.S_ISLNK(info.st_mode):
            require(path.resolve(strict=True).is_relative_to(payload.resolve()), 'payload link escapes')
            entry += ['link', str(path.readlink())]
        elif stat.S_ISREG(info.st_mode):
            size += info.st_size
            require(size <= 4 * 1024**3, 'payload byte bound exceeded')
            entry += ['file', wpe.file_hash(path, 2 * 1024**3)]
        elif stat.S_ISDIR(info.st_mode):
            entry += ['directory']
        else:
            raise ValueError('special payload entries forbidden')
        entries.append(entry)
    return wpe.sha(wpe.canonical(entries))


def verify_local(audit, assets):
    architecture = audit['architecture']
    source_id, base_id = audit['source_image_id'], audit['buildroot_image_id']
    records = []
    for identity in (source_id, base_id):
        wpe.inspect_local(identity, architecture, identity)
        records.append(wpe.strict_json(wpe.command('podman', '--remote=false', 'image', 'inspect', identity))[0])
    layers, base_layers = [record.get('RootFS', {}).get('Layers') for record in records]
    require(isinstance(layers, list) and isinstance(base_layers, list) and base_layers
            and len(layers) > len(base_layers) and layers[:len(base_layers)] == base_layers,
            'retained image does not inherit exact reviewed buildroot')
    with wpe.mounted(source_id) as root:
        script = wpe.read_bounded(root / 'build-wpe.sh', 2 * 1024 * 1024)
        require(wpe.sha(script) == audit['source_script_sha256'], 'retained build script differs')
        require(source_archives(script.decode(), assets) == audit['archives'], 'retained archive pins differ')
        return payload_digest(root, architecture)


def repository(architecture):
    require(architecture in wpe.ARCHES, 'unsupported architecture')
    return 'ghcr.io/tilo-os/tilo-development-wpe-adopted-' + architecture


def validate_receipt(receipt, audit, audit_sha256):
    require(isinstance(audit_sha256, str) and bool(re.fullmatch('[0-9a-f]{64}', audit_sha256)),
            'original audit pin required')
    require(isinstance(receipt, dict) and set(receipt) == {'schema', 'purpose', 'audit_sha256',
        'audit', 'projection_epoch', 'reference', 'config_digest', 'payload_sha256', 'production_signed'},
        'unexpected adoption receipt schema')
    require(type(receipt['schema']) is int and receipt['schema'] == 1 and receipt['purpose'] == PURPOSE
            and receipt['production_signed'] is False and receipt['audit'] == audit
            and receipt['audit_sha256'] == audit_sha256, 'adoption provenance differs')
    require(isinstance(receipt['projection_epoch'], str) and
            bool(re.fullmatch(r'0|[1-9][0-9]{0,11}', receipt['projection_epoch'])) and
            int(receipt['projection_epoch']) <= 253402300799, 'invalid projection epoch')
    require(isinstance(receipt['reference'], str) and bool(re.fullmatch(
        re.escape(repository(audit['architecture'])) + '@' + wpe.DIGEST, receipt['reference'])),
        'immutable dedicated adoption repository required')
    require(isinstance(receipt['config_digest'], str) and bool(re.fullmatch(wpe.DIGEST, receipt['config_digest']))
            and isinstance(receipt['payload_sha256'], str)
            and bool(re.fullmatch('[0-9a-f]{64}', receipt['payload_sha256'])), 'invalid adoption identity')


def import_adopted(receipt, audit, audit_sha256):
    validate_receipt(receipt, audit, audit_sha256)
    architecture = audit['architecture']
    wpe.native_host(architecture)
    reference = receipt['reference']
    require(wpe.registry_identity(reference, architecture) == receipt['config_digest'], 'published config differs')
    wpe.command('podman', '--remote=false', 'pull', '--tls-verify=true',
                '--platform=linux/' + wpe.ARCHES[architecture][0], reference)
    identity = wpe.inspect_local(reference, architecture, receipt['config_digest'])
    with wpe.mounted(identity) as root:
        require(payload_digest(root, architecture) == receipt['payload_sha256'], 'adopted payload differs')
    return identity


def adopt_export(audit, audit_sha256, epoch, assets):
    require(bool(re.fullmatch(r'0|[1-9][0-9]{0,11}', epoch)) and int(epoch) <= 253402300799,
            'canonical projection epoch required')
    architecture = audit['architecture']
    wpe.native_host(architecture)
    expected = verify_local(audit, assets)
    identity = wpe._project_payload(audit['source_image_id'], {'architecture': architecture, 'epoch': epoch})
    with wpe.mounted(identity) as root:
        require(payload_digest(root, architecture) == expected, 'projection changed retained payload')
    with tempfile.TemporaryDirectory(prefix='tilo-wpe-adoption-') as temporary:
        digest_file = Path(temporary) / 'digest'
        tag = repository(architecture) + ':adopt-' + audit_sha256
        wpe.command('podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile',
                    str(digest_file), identity, 'docker://' + tag)
        digest = wpe.read_bounded(digest_file, 128).decode().strip()
        require(bool(re.fullmatch(wpe.DIGEST, digest)), 'invalid publication digest')
    receipt = dict(schema=1, purpose=PURPOSE, audit_sha256=audit_sha256, audit=audit,
        projection_epoch=epoch, reference=repository(architecture) + '@' + digest,
        config_digest=identity, payload_sha256=expected, production_signed=False)
    import_adopted(receipt, audit, audit_sha256)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('adopt-export', 'import-adopted'))
    for name in ('repo', 'assets'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--audit', type=Path)
    parser.add_argument('--audit-sha256')
    parser.add_argument('--architecture', required=True)
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--receipt-sha256')
    parser.add_argument('--projection-epoch')
    args = parser.parse_args()
    try:
        if args.mode == 'import-adopted':
            require(args.receipt is not None, 'adoption receipt required')
            receipt = pinned_json(args.receipt, args.receipt_sha256)
            # The independent receipt hash binds its embedded audit and the
            # original audit-file hash. No second consumer file is needed.
            audit = validate_audit(receipt['audit'], args.repo, args.assets, args.architecture)
            print(import_adopted(receipt, audit, receipt['audit_sha256']))
        else:
            require(args.audit is not None, 'independently pinned audit required')
            audit = validate_audit(pinned_json(args.audit, args.audit_sha256), args.repo, args.assets, args.architecture)
            require(args.projection_epoch is not None, 'explicit projection epoch required')
            print(json.dumps(adopt_export(audit, args.audit_sha256, args.projection_epoch, args.assets), sort_keys=True, indent=2))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'development WPE adoption rejected\n')


if __name__ == '__main__':
    main()
