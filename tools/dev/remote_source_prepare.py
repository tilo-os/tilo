#!/usr/bin/env python3
"""Prepare a bounded source-only mirror for the native import experiment.

Preparation is local and read-only against the application owner's worktree.
Only a reviewed mirror should be sent by a separately configured remote build tool.
"""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess

ROOT_FILES = {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'rustfmt.toml', 'clippy.toml'}
ROOTS = {'apps', 'crates', 'examples', 'vendor', '.cargo'}
# Exact public build inputs outside the source roots. Rust includes these
# assets directly; omitting them makes an otherwise complete locked workspace
# fail before its selected tests can run. Keep this list explicit rather than
# allowing arbitrary tools, docs, environment files or imported payloads.
SOURCE_HELPERS = {
    'tools/cargo-runner.sh', 'tools/with-test-environment.sh',
    'tools/test-assets-build.rs', 'tools/test-assets.py',
    'tools/font-catalog.py',
    'tools/protocols/airdrop_sender.py',
    'os/legacy-guest-snapshot.env', 'dist/VERSION',
    'docs/shell-navigation-keyboard-map.md',
    'docs/architecture/dictation-whistle-aot-review.json',
    'docs/architecture/dictation-whistle-aot-image.json',
    'docs/architecture/dictation-whistle-aot-image-120s.json',
}
BUILD_ROOTS = ('dist/dev/', 'dist/ci/')
FORBIDDEN = {'.git', '.worktrees', 'target', '.aws', '.ssh', '.config', 'credentials', 'imports', 'imported', 'datasets', 'secrets', 'keys'}
UNTRACKED_SOURCE = {'.rs', '.toml', '.ftl'}
MAX_BYTES = 256 * 1024 * 1024
MAX_FILES = 16000


def allowed(name, tracked):
    path = Path(name)
    if path.is_absolute() or '..' in path.parts:
        return False
    if any(part.lower() in FORBIDDEN or part.lower().startswith('.env') for part in path.parts):
        return False
    if path.suffix.lower() in {'.pem', '.key', '.p12', '.pfx', '.jsonl', '.sqlite', '.db'}:
        return False
    selected = name in ROOT_FILES or name in SOURCE_HELPERS or path.parts[0] in ROOTS or name.startswith(BUILD_ROOTS)
    return selected and (tracked or path.suffix in UNTRACKED_SOURCE)


def source_files(source):
    tracked = set(subprocess.check_output(['git', '-C', str(source), 'ls-files', '-z']).decode().split('\0'))
    untracked = set(subprocess.check_output(['git', '-C', str(source), 'ls-files', '-z', '--others', '--exclude-standard']).decode().split('\0'))
    files = []
    size = 0
    for name in sorted((tracked | untracked) - {''}):
        if not allowed(name, name in tracked):
            continue
        path = source / name
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(info.st_mode) or path.resolve().is_relative_to(source) is False:
            raise ValueError('selected input must be a regular source file: ' + name)
        # Reject symlink ancestors as well as symlink leaves.
        if any(parent.is_symlink() for parent in path.parents if parent != source.parent):
            raise ValueError('selected input has a symlink ancestor: ' + name)
        size += info.st_size
        if size > MAX_BYTES or len(files) >= MAX_FILES:
            raise ValueError('source budget exceeded; do not broaden the upload blindly')
        files.append((name, info))
    required = ROOT_FILES - {'rustfmt.toml', 'clippy.toml'}
    if not required.issubset({name for name, _ in files}):
        raise ValueError('missing locked workspace/toolchain inputs')
    return files, size


def prepare(source, mirror, receipt):
    source = source.resolve()
    if mirror.resolve().is_relative_to(source) or source.is_relative_to(mirror.resolve()):
        raise ValueError("source owner worktree and build mirror must be disjoint")
    if mirror.exists() and not (mirror / '.source-build-owner').exists():
        raise ValueError('refuse an unowned mirror')
    if shutil.disk_usage(mirror.parent).free < 3 * MAX_BYTES:
        raise ValueError("insufficient disk headroom for bounded source mirror")
    mirror.mkdir(parents=True, exist_ok=True)
    (mirror / '.source-build-owner').touch()
    files, size = source_files(source)
    manifest = {}
    for name, before in files:
        payload = (source / name).read_bytes()
        after = (source / name).lstat()
        if (before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError('source changed while preparing; wait for owner edits and retry: ' + name)
        destination = mirror / name
        if destination.is_symlink() or any(parent.is_symlink() for parent in destination.parents):
            raise ValueError("owned mirror contains a symlink; refuse writing source")
        destination.parent.mkdir(parents=True, exist_ok=True)
        digest = hashlib.sha256(payload).hexdigest()
        if not destination.exists() or hashlib.sha256(destination.read_bytes()).hexdigest() != digest:
            destination.write_bytes(payload)
        destination.chmod(stat.S_IMODE(before.st_mode))
        manifest[name] = digest
    # Remove only paths recorded by the previous successful preparation.
    previous = mirror / '.source-manifest.json'
    if previous.exists():
        for name in json.loads(previous.read_text()):
            if name not in manifest and allowed(name, True):
                (mirror / name).unlink(missing_ok=True)
    if [(name, info.st_mtime_ns, info.st_ctime_ns) for name, info in source_files(source)[0]] != [(name, info.st_mtime_ns, info.st_ctime_ns) for name, info in files]:
        raise ValueError('source selection changed during preparation; retry after owner edits settle')
    previous.write_text(json.dumps(manifest, sort_keys=True))
    if not (mirror / '.git').exists():
        subprocess.run(['git', 'init', '-q', str(mirror)], check=True)
    (mirror / '.git/info/exclude').write_text('target/\n.source-build-owner\n.source-manifest.json\n')
    # No builder identity/credentials, receipts or imported data live in the mirror.
    actual = subprocess.check_output(['git', '-C', str(mirror), 'ls-files', '-z', '-co', '--exclude-standard']).decode().split('\0')
    if set(actual) - {''} != set(manifest):
        raise ValueError('mirror contains unexpected upload paths; refuse source egress')
    result = {'source': str(source), 'mirror': str(mirror), 'files': len(files), 'bytes': size,
              'snapshot_sha256': hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest(),
              'source_only': True, 'imported_payloads_uploaded': False, 'build_executed': False}
    receipt.write_text(json.dumps(result, indent=2) + '\n')
    return result



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--mirror', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    if args.receipt.resolve().is_relative_to(args.mirror.resolve()):
        parser.error('receipt/config must stay outside the upload mirror')
    with args.receipt.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result = prepare(args.source, args.mirror, args.receipt)
        print(json.dumps(result))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
