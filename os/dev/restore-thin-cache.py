#!/usr/bin/env python3
"""Restore independently admitted incomplete Cargo cache, then let Cargo validate it."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
HELPER = '9d7a6b259c9b75b6c5031472bfed05b6461c1893'
MAX_BYTES = 10 * 1024**3
MAX_FILES = 30000


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def relative(value):
    path = PurePosixPath(value)
    require(value and str(path) == value and not path.is_absolute() and
            not any(part in ('..', '.') for part in path.parts), 'noncanonical cache path')
    return path


def read_json(path, digest):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= 32 * 1024**2,
            'metadata type/size')
    require(re.fullmatch('[0-9a-f]{64}', digest) and sha(path) == digest, 'metadata hash differs')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    return json.loads(path.read_bytes(), object_pairs_hook=unique)


def regular_beneath(root, name):
    path = root / relative(name)
    require(path.is_file() and not path.is_symlink() and path.resolve() == path,
            'source path missing or traverses symlink: ' + name)
    return path


def validate_sources(root, metadata):
    require(metadata['purpose'] == 'incomplete-thin-cargo-source-metadata' and
            metadata['source_revision'] == SOURCE and metadata['build_root'] == '/build',
            'source identity differs')
    records = metadata['tracked_files']
    require(0 < len(records) <= 20000 and len({r['path'] for r in records}) == len(records),
            'source inventory count/duplicates')
    updates = []
    for record in records:
        path = regular_beneath(root, record['path'])
        require(path.stat().st_size == record['size'] and sha(path) == record['sha256'],
                'source bytes differ: ' + record['path'])
        require(path.stat().st_mode & 0o777 == record['mode'] and
                isinstance(record['mtime_ns'], int) and record['mtime_ns'] >= 0,
                'source mode/timestamp differs')
        updates.append((path, record['mtime_ns']))
    # Capture omissions are not silently turned into timestamp assertions.
    for record in metadata.get('skipped', []):
        require(record['reason'] == 'absent-in-live-build', 'unknown source omission')
        path = root / relative(record['path'])
        require(not path.exists() and not path.is_symlink(), 'previously absent source now present')
    return updates


def validate_toolchain(metadata, command=subprocess.check_output):
    packages = metadata['rpm_nevras']
    require(0 < len(packages) <= 10000 and packages == sorted(packages) and
            hashlib.sha256(('\n'.join(packages)+'\n').encode()).hexdigest() == metadata['rpm_nevras_sha256'],
            'RPM inventory/hash differs')
    actual = command(['/usr/bin/rpm', '-qa', '--qf', '%{NAME}-%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n'])
    require(len(actual) <= 4*1024**2 and sorted(actual.decode().splitlines()) == packages,
            'installed RPM toolchain differs')
    records = metadata['inner_tools']
    require(len(records) == 4 and {r['command'] for r in records} == {'cargo', 'rustc', 'cc', 'ld'},
            'inner tool inventory differs')
    for record in records:
        path = Path(record['path'])
        require(path.is_absolute() and str(path.resolve()) == record['resolved_namespace_path'] and
                path.stat().st_size == record['size'] and sha(path) == record['sha256'],
                'inner tool bytes differ: ' + record['command'])
        flag = '-vV' if record['command'] == 'rustc' else ('-V' if record['command'] == 'cargo' else '--version')
        require(command([str(path), flag]).decode().splitlines() == record['version'],
                'inner tool version differs')


def unpack(archive, metadata, destination):
    records = metadata['inventory']
    require(0 < len(records) <= MAX_FILES, 'cache inventory bound')
    require(sum(record['size'] for record in records.values()) <= MAX_BYTES, 'cache byte bound')
    seen = set()
    with tarfile.open(archive, 'r:') as stream:
        for member in stream:
            relative(member.name)
            require(member.name in records and member.name not in seen, 'unexpected/duplicate tar member')
            seen.add(member.name)
            record = records[member.name]
            name = PurePosixPath(member.name).name
            require(member.isfile() and not member.mode & 0o7000 and
                    member.size == record['size'] and member.mode == record['mode'], 'cache entry type/metadata')
            require(not name.startswith('.cargo') and '.tmp' not in name and '.rcgu.' not in name and
                    not member.name.startswith('incremental/'), 'lock or temporary cache member')
            require(isinstance(record['mtime_ns'], int) and record['mtime_ns'] >= 0, 'cache timestamp')
            path = destination / member.name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open('xb') as output:
                shutil.copyfileobj(stream.extractfile(member), output, 1024 * 1024)
            require(sha(path) == record['sha256'], 'cache file hash differs')
            os.chmod(path, member.mode)
            os.utime(path, ns=(record['mtime_ns'], record['mtime_ns']))
    require(seen == set(records), 'cache inventory incomplete')


def restore(context, approval_sha, root=Path('/build')):
    approval = read_json(context / 'approval.json', approval_sha)
    require(approval['purpose'] == 'tilo-incomplete-thin-cache-admission' and
            approval['source_revision'] == SOURCE and approval['producer_helper_revision'] == HELPER,
            'admission identity differs')
    require(re.fullmatch(r'ghcr\.io/tilo-os/tilo-development-partial-thin-cache-aarch64@sha256:[0-9a-f]{64}',
                         approval['checkpoint_reference']), 'immutable cache reference required')
    require(approval['restore_helper_sha256'] == sha(Path(__file__)), 'reviewed restore helper differs')
    require(approval['cargo_revalidation_required'] is True and approval['incomplete_cache_reviewed'] is True,
            'cache not independently reviewed')
    metadata = read_json(context / 'cache-metadata.json', approval['metadata_sha256'])
    sources = read_json(context / 'source-metadata.json', approval['source_metadata_sha256'])
    require(metadata['purpose'] == 'tilo-development-incomplete-thin-cargo-cache' and
            metadata['source_revision'] == SOURCE and metadata['helper_revision'] == HELPER and
            metadata['source_path'] == '/build/target/thin-app' and
            all(metadata[k] is False for k in ('installable', 'resume_admitted', 'complete')) and
            metadata['cargo_revalidation_required'] is True, 'checkpoint identity/claims differ')
    archive = context / 'thin-cache.tar'
    require(archive.is_file() and not archive.is_symlink() and
            archive.stat().st_size == metadata['archive_bytes'] and archive.stat().st_size <= MAX_BYTES + 128*1024**2 and
            sha(archive) == metadata['archive_sha256'] == approval['archive_sha256'], 'cache archive differs')
    require(root.resolve() == root and root.is_dir(), 'build root differs')
    updates = validate_sources(root, sources)
    validate_toolchain(sources)
    target = root / 'target'
    require(target.is_dir() and target.resolve() == target, 'target directory differs')
    destination = target / 'thin-app'
    require(not destination.is_symlink() and (not destination.exists() or
            (destination.is_dir() and not any(destination.iterdir()))), 'thin target was not cleaned')
    with tempfile.TemporaryDirectory(prefix='thin-restore-', dir=target) as temporary:
        staged = Path(temporary) / 'thin-app'
        staged.mkdir()
        unpack(archive, metadata, staged)
        # All source bytes and cache bytes passed before the first timestamp write.
        for path, stamp in updates:
            os.utime(path, ns=(stamp, stamp))
        if destination.exists(): destination.rmdir()
        staged.rename(destination)
    evidence = dict(purpose='tilo-incomplete-thin-cache-restored', approval_sha256=approval_sha,
                    checkpoint_reference=approval['checkpoint_reference'], archive_sha256=approval['archive_sha256'],
                    source_metadata_sha256=approval['source_metadata_sha256'], metadata_sha256=approval['metadata_sha256'],
                    restored_files=len(metadata['inventory']), restored_bytes=sum(r['size'] for r in metadata['inventory'].values()),
                    verified_source_timestamps_restored=len(updates), dependency_timestamps_restored=False,
                    inner_tool_executables_verified=True, rpm_inventory_sha256=sources['rpm_nevras_sha256'],
                    cargo_revalidation_required=True,
                    cargo_freshness_proven=False, complete=False, installable=False)
    (root / 'thin-cache-restoration.json').write_text(json.dumps(evidence, sort_keys=True, indent=2)+'\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--approval-sha256', required=True)
    args = parser.parse_args()
    restore(Path('/tilo-thin-cache'), args.approval_sha256)
