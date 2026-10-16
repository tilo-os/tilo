#!/usr/bin/env python3
"""Pinned app-channel reader and verified receipt adapter; no network or publishing.

Receipt creation delegates cryptography to an explicitly supplied, hash-pinned
trusted verifier. Reading needs only Python's standard library. Trust/digest pins
must come from the caller's trusted configuration, never from the input itself.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import tarfile
import tomllib
from urllib.parse import urlsplit

ARCHES = {'x86_64', 'aarch64'}
# Accepted serialized labels/aliases in pkg/src/manifest.rs; no inferred floor.
ISA_LABELS = {'x86_64': {'baseline', 'x86-64', 'v2', 'x86-64-v2', 'v3', 'x86-64-v3', 'v4', 'x86-64-v4'},
              'aarch64': {'baseline', 'aarch64', 'armv8-a', 'v8-a', 'armv8.2-a', 'v8.2-a', 'armv8.4-a', 'v8.4-a', 'armv9-a', 'v9-a'}}
RECEIPT_SCHEMA = 'tilo.app-publication/v1'
CHANNEL_SCHEMA = 'tilo.app-channel/v1'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value, length=64):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(length) + '}', value), 'invalid digest')
    return value


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate JSON key')
        result[key] = value
    return result


def read_json(raw):
    return json.loads(raw, object_pairs_hook=unique_object)


def pinned_json(raw, expected):
    require(hashlib.sha256(raw).hexdigest() == digest(expected), 'trusted digest mismatch')
    return read_json(raw)


def trusted_keys(policy):
    apps = policy['app_publishers']
    require(isinstance(apps, list) and apps, 'explicit trusted app publishers required')
    return {digest(key) for key in apps}, digest(policy['runtime_publisher'])


def archive_matches_tree(archive, tree):
    """Stream archive bytes, reject unsafe members, and bind to verified local tree."""
    files, names = set(), set()
    with tarfile.open(archive, 'r:gz') as stream:
        for member in stream:
            name = member.name.rstrip('/')
            require(name and not PurePosixPath(name).is_absolute() and '\\' not in name
                    and ':' not in name and all(p not in ('', '.', '..') for p in name.split('/')), 'unsafe archive path')
            require(name not in names and len(names) < 4096, 'duplicate or excessive archive entries')
            names.add(name)
            require(member.isdir() or (member.isfile() and not member.issparse()), 'archive links/special files rejected')
            require(name in {'tapp', 'runtime', 'index.json', 'SHA256SUMS'}
                    or name.startswith(('tapp/', 'runtime/')), 'unexpected collection member')
            path = tree / name
            require(not path.is_symlink() and all(not (tree / parent).is_symlink()
                    for parent in path.relative_to(tree).parents), 'symlink in local collection')
            if member.isdir():
                require(path.is_dir(), 'missing collection directory')
            else:
                require(path.is_file() and path.stat().st_size == member.size, 'collection size mismatch')
                with stream.extractfile(member) as content:
                    require(hashlib.file_digest(content, 'sha256').hexdigest() == sha(path), 'archive/tree digest mismatch')
                files.add(name)
    expected = {'index.json', 'SHA256SUMS'}
    for directory in ('tapp', 'runtime'):
        for path in (tree / directory).rglob('*'):
            require(not path.is_symlink() and (path.is_file() or path.is_dir()), 'nonregular local collection')
            if path.is_file():
                expected.add(path.relative_to(tree).as_posix())
    require(files == expected, 'archive/tree inventory mismatch')
    checks = {}
    for line in (tree / 'SHA256SUMS').read_text().splitlines():
        value, name = line.split('  ', 1)
        require(name in files and name not in checks, 'invalid checksum inventory')
        checks[name] = digest(value)
    require(set(checks) == files - {'SHA256SUMS'}, 'incomplete checksum inventory')
    require(all(sha(tree / name) == value for name, value in checks.items()), 'collection checksum mismatch')


def create_receipt(archive, tree, archive_sha256, source_sha, architecture, policy, verifier_path, verifier_sha256, audit_only=False,
                   canonical_binaries=None):
    require(architecture in ARCHES, 'unsupported architecture')
    digest(source_sha, 40)
    require(sha(archive) == digest(archive_sha256), 'archive digest mismatch')
    require(sha(verifier_path) == digest(verifier_sha256), 'trusted verifier digest mismatch')
    require(verifier_sha256 == digest(policy['verifier_sha256']), 'verifier not approved by trust policy')
    archive_matches_tree(archive, tree)
    spec = importlib.util.spec_from_file_location('trusted_app_artifact_verifier', verifier_path)
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    if canonical_binaries is not None:
        verifier = verifier.CanonicalVerifier(*canonical_binaries, policy)
    apps_trusted, runtime_trusted = trusted_keys(policy)
    index = read_json((tree / 'index.json').read_bytes())
    require(index['format'] == 1 and index['source'] == source_sha and index['architecture'] == architecture,
            'collection source or architecture mismatch')
    runtime = verifier.verify_runtime(tree / 'runtime', architecture)
    require(runtime['publisher'] == runtime_trusted, 'untrusted runtime publisher')
    runtime_manifest = tomllib.loads((tree / 'runtime/tilo-runtime' / runtime['version'] / 'bundle.toml').read_text())
    runtime['signature_hex'] = runtime_manifest['signature_hex']
    packages, names, compatibility_issues = [], set(), []
    for app in index['apps']:
        filename = app['file']
        require(re.fullmatch('[A-Za-z0-9][A-Za-z0-9._+-]*-' + architecture + r'\.tapp', filename), 'tapp architecture filename mismatch')
        require(filename not in names and app['runtime'] == 'tilo-runtime/' + runtime['version'], 'duplicate package or runtime mismatch')
        names.add(filename)
        path = tree / 'tapp' / filename
        require(sha(path) == app['sha256'], 'index package digest mismatch')
        verified = verifier.verify_tapp(path, app)
        require(verified['publisher'] in apps_trusted, 'untrusted app publisher')
        with tarfile.open(path, 'r:') as tapp:
            manifest = tomllib.loads(tapp.extractfile('manifest.toml').read().decode())
        isa = manifest['package']['isa']
        if (set(isa) != {architecture} or isa.get(architecture) not in ISA_LABELS[architecture]
                or isa.get(architecture) not in policy['allowed_package_isa'][architecture]):
            compatibility_issues.append({'file': filename, 'signed_isa': isa, 'reason': 'signed package ISA architecture unsupported'})
        packages.append({'file': filename, 'app_id': app['id'], 'version': app['version'], 'sha256': app['sha256'],
                         'package_id': verified['package_id'], 'publisher': verified['publisher'], 'isa': isa.get(architecture)})
    require(names and names == {p.name for p in (tree / 'tapp').iterdir()}, 'package inventory mismatch')
    result = {'schema': RECEIPT_SCHEMA, 'architecture': architecture, 'source_sha': source_sha,
            'collection': {'format': 'tilo-apps-tar-gzip/v1', 'sha256': archive_sha256, 'bytes': archive.stat().st_size},
            'runtime': runtime, 'packages': packages,
            'verification': {'verifier_sha256': verifier_sha256, 'package_identity': 'blake3-ed25519-signed-root-v1',
                             'runtime_identity': 'blake3-ed25519-runtime-bundle-v1'},
            'publication': {'status': 'unpublished'}}
    if canonical_binaries is not None:
        result['verification'].update(backend='canonical-cli-v1', canonical_binaries=verifier.binary_hashes)
        if 'canonical_bundle' in policy:
            result['verification']['canonical_bundle'] = policy['canonical_bundle']
    if audit_only:
        result.update(schema='tilo.app-collection-audit/v1', compatibility_issues=compatibility_issues,
                      compatible=not compatibility_issues, cryptographic_integrity_verified=True)
        return result
    require(not compatibility_issues, 'signed package ISA architecture unsupported: ' + ', '.join(x['file'] for x in compatibility_issues))
    validate_receipt(result, architecture, policy, require_published=False)
    return result


def validate_receipt(receipt, architecture, policy, require_published=True):
    apps_trusted, runtime_trusted = trusted_keys(policy)
    require(receipt['schema'] == RECEIPT_SCHEMA and receipt['architecture'] == architecture, 'receipt architecture/schema mismatch')
    source = digest(receipt['source_sha'], 40)
    collection = receipt['collection']
    require(collection['format'] == 'tilo-apps-tar-gzip/v1' and type(collection['bytes']) is int and collection['bytes'] > 0, 'invalid collection format/size')
    digest(collection['sha256'])
    runtime = receipt['runtime']
    digest(runtime['root_hash'])
    digest(runtime['signature_hex'], 128)
    require(digest(runtime['publisher']) == runtime_trusted and re.fullmatch('[A-Za-z0-9._+-]+', runtime['version']), 'runtime trust/identity mismatch')
    verification = receipt['verification']
    require(digest(verification['verifier_sha256']) == digest(policy['verifier_sha256']), 'unapproved verifier identity')
    require(verification['package_identity'] == 'blake3-ed25519-signed-root-v1'
            and verification['runtime_identity'] == 'blake3-ed25519-runtime-bundle-v1', 'unsupported verification identity')
    if 'canonical_verifier_bundles' in policy:
        bundle = verification.get('canonical_bundle', {})
        descriptor = policy['canonical_verifier_bundles'].get(bundle.get('architecture'))
        require(isinstance(descriptor, dict) and bundle.get('sha256') == descriptor.get('sha256')
                and verification.get('backend') == 'canonical-cli-v1'
                and verification.get('canonical_binaries') == descriptor.get('canonical_binaries'),
                'canonical verifier bundle not approved')
    if 'canonical_binaries' in policy:
        require(verification.get('backend') == 'canonical-cli-v1' and verification.get('canonical_binaries') == policy['canonical_binaries'],
                'canonical verification identity mismatch')
    names, app_ids = set(), set()
    for app in receipt['packages']:
        require(app['file'] not in names and re.fullmatch('[A-Za-z0-9][A-Za-z0-9._+-]*-' + architecture + r'\.tapp', app['file']), 'package architecture/inventory mismatch')
        names.add(app['file'])
        require(app['app_id'] not in app_ids and re.fullmatch('[A-Za-z0-9._-]+', app['app_id']), 'invalid/duplicate app identity')
        app_ids.add(app['app_id'])
        digest(app['sha256'])
        require(app['package_id'].startswith('tapp:'), 'invalid signed package identity')
        digest(app['package_id'][5:])
        require(digest(app['publisher']) in apps_trusted and app['isa'] in ISA_LABELS[architecture]
                and app['isa'] in policy['allowed_package_isa'][architecture], 'package trust/ISA mismatch')
    require(names, 'empty package receipt')
    publication = receipt['publication']
    require(publication['status'] in {'unpublished', 'published'}, 'unknown publication state')
    if require_published:
        require(publication['status'] == 'published', 'unpublished collection cannot be consumed')
    if publication['status'] == 'published':
        digest(publication['upload_receipt_sha256'])
        parsed = urlsplit(publication['url'])
        require(parsed.scheme == 'https' and parsed.netloc == 'code.tilo.so' and not parsed.query and not parsed.fragment,
                'unsupported publication URL origin')
        pattern = r'/api/packages/tilo/generic/tilo-app-collections/[^/]+/tilo-apps-' + architecture + r'\.tar\.gz'
        require(re.fullmatch(pattern, parsed.path) and (source in parsed.path or collection['sha256'] in parsed.path),
                'publication URL must pin full source or content identity')


def attach_publication(receipt, raw, expected_sha256, policy):
    """Bind a separately trusted uploader's receipt; does not assert remote reachability."""
    upload = pinned_json(raw, expected_sha256)
    require(upload['schema'] == 'tilo.app-upload/v1' and upload['source_sha'] == receipt['source_sha']
            and upload['architecture'] == receipt['architecture'] and upload['sha256'] == receipt['collection']['sha256']
            and upload['bytes'] == receipt['collection']['bytes'], 'uploader receipt does not match verified archive')
    result = dict(receipt, publication={'status': 'published', 'url': upload['url'], 'upload_receipt_sha256': expected_sha256})
    validate_receipt(result, receipt['architecture'], policy)
    return result


def select(raw, expected_sha256, resolved_commit, architecture, policy, require_published=True):
    digest(resolved_commit, 40)
    require(architecture in ARCHES, 'unsupported architecture')
    manifest = pinned_json(raw, expected_sha256)
    require(manifest['schema'] == CHANNEL_SCHEMA and type(manifest['generation']) is int and manifest['generation'] > 0,
            'unsupported channel schema/generation')
    digest(manifest['source_sha'], 40)
    require(set(manifest['architectures']) <= ARCHES and architecture in manifest['architectures'], 'requested architecture absent; fallback forbidden')
    receipt = manifest['architectures'][architecture]
    validate_receipt(receipt, architecture, policy, require_published)
    return {'channel_commit': resolved_commit, 'channel_manifest_sha256': expected_sha256,
            'architecture': architecture, 'source_sha': receipt['source_sha'], 'runtime': receipt['runtime'],
            'collection': receipt['collection'], 'publication': receipt['publication'],
            'usable_for_download': receipt['publication']['status'] == 'published'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--trust-policy', type=Path, required=True)
    commands = parser.add_subparsers(dest='command', required=True)
    create = commands.add_parser('create-receipt')
    for name in ('archive', 'tree', 'verifier', 'output'):
        create.add_argument('--' + name, type=Path, required=True)
    for name in ('archive-sha256', 'source-sha', 'architecture', 'verifier-sha256'):
        create.add_argument('--' + name, required=True)
    create.add_argument('--upload-receipt', type=Path)
    create.add_argument('--upload-receipt-sha256')
    create.add_argument('--audit-only', action='store_true', help='produce non-consumable compatibility audit, never a publication receipt')
    create.add_argument('--pkg-binary', type=Path, help='use canonical CLI adapter with this hash-pinned tilo-app-package binary')
    create.add_argument('--runtime-binary', type=Path)
    for name in ('select', 'inspect-candidate'):
        command = commands.add_parser(name)
        command.add_argument('--manifest', type=Path, required=True)
        command.add_argument('--manifest-sha256', required=True)
        command.add_argument('--resolved-commit', required=True)
        command.add_argument('--architecture', required=True)
    args = parser.parse_args()
    policy = read_json(args.trust_policy.read_bytes())
    if args.command == 'create-receipt':
        require(bool(args.pkg_binary) == bool(args.runtime_binary), 'both canonical verifier binaries are required')
        result = create_receipt(args.archive, args.tree, args.archive_sha256, args.source_sha, args.architecture,
                                policy, args.verifier, args.verifier_sha256, audit_only=args.audit_only,
                                canonical_binaries=(args.pkg_binary, args.runtime_binary) if args.pkg_binary else None)
        require(bool(args.upload_receipt) == bool(args.upload_receipt_sha256), 'uploader receipt and trusted digest required together')
        if args.upload_receipt:
            require(not args.audit_only, 'audit cannot be promoted to publication receipt')
            result = attach_publication(result, args.upload_receipt.read_bytes(), args.upload_receipt_sha256, policy)
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    else:
        result = select(args.manifest.read_bytes(), args.manifest_sha256, args.resolved_commit, args.architecture, policy,
                        require_published=args.command == 'select')
        print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
