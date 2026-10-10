#!/usr/bin/env python3
"""Export exact Cargo compiler units into a separately signed developer bundle.

The runtime is sealed first. This interface binds that independently selected
runtime; it is never installed into an end-user runtime layer.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import tempfile
import tomllib


def run(args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE).stdout.decode().strip()


def opened(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    stream = os.fdopen(fd, 'rb')
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        stream.close()
        raise ValueError('compiler input must be a regular file')
    return stream


def unchanged(path, before, after):
    current = path.stat(follow_symlinks=False)
    fields = ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns')
    if any(getattr(before, f) != getattr(after, f) or getattr(before, f) != getattr(current, f)
           for f in fields):
        raise ValueError('compiler artifact changed while being inspected')


def digest(path):
    with opened(path) as stream:
        before = os.fstat(stream.fileno())
        result = hashlib.file_digest(stream, 'sha256').hexdigest()
        unchanged(path, before, os.fstat(stream.fileno()))
    return result


def copy(path, dest):
    with opened(path) as stream, dest.open('xb') as output:
        before = os.fstat(stream.fileno())
        expected = hashlib.file_digest(stream, 'sha256').hexdigest()
        stream.seek(0)
        shutil.copyfileobj(stream, output, 64 * 1024)
        unchanged(path, before, os.fstat(stream.fileno()))
    if digest(dest) != expected:
        raise ValueError('compiler artifact changed while being copied')
    return expected


def metadata_archive(source, dest, member):
    if member.name != 'lib.rmeta':
        raise ValueError('compiler metadata archive requires its canonical member name')
    with opened(source) as stream:
        before = os.fstat(stream.fileno())
        expected = hashlib.file_digest(stream, 'sha256').hexdigest()
        stream.seek(0)
        # Pin the original compiled archive across ar's read; pathname replacement
        # cannot substitute another unit between inspection and extraction.
        with member.open('xb') as output:
            subprocess.run(['ar', 'p', '/proc/self/fd/' + str(stream.fileno()), 'lib.rmeta'],
                           check=True, stdout=output, pass_fds=(stream.fileno(),))
        unchanged(source, before, os.fstat(stream.fileno()))
    if not member.stat().st_size or member.stat().st_size > 256 * 1024 * 1024:
        raise ValueError('compiler metadata member is missing or oversized')
    if dest.exists():
        raise ValueError('duplicate compiler unit artifact')
    subprocess.run(['ar', 'crD', str(dest), str(member)], check=True)
    member.unlink()
    return expected


def units(path, repo):
    result = []
    with opened(path) as stream:
        before = os.fstat(stream.fileno())
        if before.st_size > 64 * 1024 * 1024:
            raise ValueError('Cargo compiler artifact stream exceeds its bound')
        lines = stream.read().splitlines()
        unchanged(path, before, os.fstat(stream.fileno()))
    for line in lines:
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get('reason') != 'compiler-artifact':
            continue
        if any(k in row['target']['kind'] for k in ('bin', 'custom-build')):
            continue
        for artifact in row['filenames']:
            source = Path(artifact)
            if source.is_symlink() or not source.resolve().is_relative_to(repo / 'target'):
                raise ValueError('compiler artifact is outside the actual build target')
        result.append(row)
    if len(result) > 4096:
        raise ValueError('compiler unit inventory exceeds its bound')
    if not result:
        raise ValueError('actual Cargo compiler artifact messages are required')
    return result


def export_units(rows, directory, repo):
    artifacts = directory / 'artifacts'
    artifacts.mkdir()
    records, ui, toolkit = [], None, None
    for row in rows:
        outputs = []
        for name in row['filenames']:
            source = Path(name)
            if source.name == 'libruntime_dylib.so':
                if toolkit and toolkit != source:
                    raise ValueError('ambiguous actual runtime artifact')
                toolkit = source
                continue
            if source.suffix == '.rlib':
                dest = artifacts / source.name
                original = metadata_archive(source, dest, directory / 'lib.rmeta')
            elif source.suffix == '.so' and 'proc-macro' in row['target']['kind']:
                dest = artifacts / source.name
                if dest.exists():
                    raise ValueError('duplicate compiler plugin artifact')
                original = copy(source, dest)
            else:
                continue
            outputs.append(dict(name=dest.name, sha256=digest(dest),
                                size=dest.stat().st_size, compiled_input_sha256=original))
            if row['target']['name'] == 'tilo_app' and source.suffix == '.rlib':
                if ui and ui != dest.name:
                    raise ValueError('ambiguous public SDK compiler unit')
                ui = dest.name
        if outputs:
            records.append(dict(package_id=row['package_id'].replace(str(repo), '${REPO}'),
                                target=row['target']['name'], kind=row['target']['kind'],
                                profile=row['profile'], features=row['features'], artifacts=outputs))
    if toolkit is None or ui is None:
        raise ValueError('actual runtime and public SDK compiler units are required')
    records.sort(key=lambda row: json.dumps(row, sort_keys=True))
    return records, ui, toolkit


def capture(args):
    repo = args.repo.resolve()
    directory = args.capture.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    records, ui, toolkit = export_units(units(args.units, repo), directory, repo)
    compiler = run(['rustc', '--version', '--verbose'])
    host = next(line[6:] for line in compiler.splitlines() if line.startswith('host: '))
    descriptor = dict(schema=1, compiler=compiler, host=host,
                      architecture=platform.machine(), profile='thin-app', ui=ui,
                      toolkit_sha256=digest(toolkit),
                      cargo_lock_sha256=digest(repo / 'Cargo.lock'), units=records,
                      resources=json.loads(args.resources.read_text()))
    (directory / 'manifest.json').write_text(json.dumps(descriptor, sort_keys=True, indent=2) + '\n')
    print(json.dumps(dict(captured=str(directory), toolkit_sha256=descriptor['toolkit_sha256'],
                         compiler=compiler, host=host)))


def checked_capture(directory):
    manifest = directory / 'manifest.json'
    with opened(manifest) as stream:
        if os.fstat(stream.fileno()).st_size > 8 * 1024 * 1024:
            raise ValueError('compiler descriptor exceeds its bound')
        descriptor = json.load(stream)
    if (descriptor.get('schema') != 1 or descriptor.get('profile') != 'thin-app'
            or not isinstance(descriptor.get('resources'), list)):
        raise ValueError('unsupported compiler interface')
    if len(descriptor['units']) > 4096 or len(descriptor['resources']) > 4096:
        raise ValueError('compiler descriptor exceeds its bound')
    if any(not isinstance(k, str) or not k or len(k) > 256 or any(ord(c) < 32 for c in k)
           for k in descriptor['resources']):
        raise ValueError('invalid SDK resource declaration')
    names = set()
    for unit in descriptor['units']:
        for artifact in unit['artifacts']:
            name = artifact['name']
            if Path(name).name != name or name in names:
                raise ValueError('unsafe or duplicate compiler artifact')
            names.add(name)
            path = directory / 'artifacts' / name
            if path.is_symlink() or path.stat().st_size != artifact['size'] or digest(path) != artifact['sha256']:
                raise ValueError('compiler artifact differs from captured unit')
    if names != {p.name for p in (directory / 'artifacts').iterdir()} or descriptor['ui'] not in names:
        raise ValueError('compiler interface inventory is incomplete')
    return descriptor


def snapshot_capture(source, destination, expected):
    destination.mkdir()
    artifacts = destination / 'artifacts'
    artifacts.mkdir()
    copy(source / 'manifest.json', destination / 'manifest.json')
    for unit in expected['units']:
        for artifact in unit['artifacts']:
            copy(source / 'artifacts' / artifact['name'], artifacts / artifact['name'])
    if checked_capture(destination) != expected:
        raise ValueError('compiler capture changed while being snapshotted')


def verify_runtime_exports(runtime, compiler):
    # Native libraries for proc-macro loading must be the selected SDK's bytes,
    # not just arbitrary files signed by the same publisher during a race.
    expected = {row['path']: (row['size'], row['hash']) for row in runtime['files']
                if row['path'].startswith('lib/') and row['path'] != 'lib/libruntime_dylib.so'}
    actual = {row['path']: (row['size'], row['hash']) for row in compiler['files']}
    if any(actual.get(path) != identity for path, identity in expected.items()):
        raise ValueError('compiler native closure differs from the selected signed runtime')


def export(args):
    sdk = args.sdk.resolve()
    tool = str(args.bundle_tool.resolve())
    run([tool, 'verify', str(sdk), '--require-publisher', args.sdk_publisher,
         '--require-signature', '--require-arch', platform.machine()])
    runtime = tomllib.loads((sdk / 'bundle.toml').read_text())
    if runtime['root_hash'] != args.sdk_root:
        raise ValueError('runtime root differs from independent selection')
    if run([tool, 'key', 'pubkey', '--identity', str(args.identity)]) != args.sdk_publisher:
        raise ValueError('compiler interface signer differs from runtime publisher')
    captured = checked_capture(args.capture)
    if captured['architecture'] != runtime['arch'] or not captured['host'].startswith(runtime['arch'] + '-'):
        raise ValueError('cross-host compiler interfaces are not supported')
    toolkit_hash = digest(sdk / 'lib/libruntime_dylib.so')
    if captured['toolkit_sha256'] != toolkit_hash:
        raise ValueError('compiled runtime differs from selected signed runtime')
    args.out_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='compiler-interface-', dir=args.out_root) as scratch:
        temporary = Path(scratch)
        directory = temporary / 'compiler-interface'
        snapshot_capture(args.capture, directory, captured)
        descriptor = checked_capture(directory)
        descriptor['runtime'] = dict(version=runtime['version'], root=runtime['root_hash'],
                                     publisher=runtime['publisher_hex'], toolkit_sha256=toolkit_hash)
        (directory / 'manifest.json').write_text(json.dumps(descriptor, sort_keys=True, indent=2) + '\n')
        empty = temporary / 'empty'
        empty.mkdir()
        version_file = temporary / 'version'
        command = [tool, 'build', '--release', 'sdk-compiler-1', '--arch', runtime['arch'],
                   '--from', str(empty), '--identity', str(args.identity),
                   '--out-root', str(args.out_root), '--version-out', str(version_file),
                   '--include', str(directory)]
        for path in sorted((sdk / 'lib').iterdir()):
            if path.name != 'libruntime_dylib.so':
                command += ['--include', str(path)]
        run(command)
        bundle = args.out_root / version_file.read_text().strip()
        run([tool, 'verify', str(bundle), '--require-publisher', args.sdk_publisher,
             '--require-signature', '--require-arch', runtime['arch']])
        run([tool, 'verify', str(sdk), '--require-publisher', args.sdk_publisher,
             '--require-signature', '--require-arch', runtime['arch']])
        final_runtime = tomllib.loads((sdk / 'bundle.toml').read_text())
        if final_runtime != runtime or digest(sdk / 'lib/libruntime_dylib.so') != toolkit_hash:
            raise ValueError('selected signed runtime changed during compiler export')
        manifest = tomllib.loads((bundle / 'bundle.toml').read_text())
        verify_runtime_exports(runtime, manifest)
        if checked_capture(args.capture) != captured:
            raise ValueError('compiler capture changed during export')
        print(json.dumps(dict(bundle=str(bundle), root=manifest['root_hash'],
                              publisher=manifest['publisher_hex'], runtime=descriptor['runtime'],
                              compiler=descriptor['compiler'], host=descriptor['host'], architecture=runtime['arch'])))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture-only', action='store_true')
    parser.add_argument('--capture', type=Path, required=True)
    for name in ('repo', 'units', 'resources', 'sdk', 'identity', 'bundle-tool', 'out-root'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--sdk-root')
    parser.add_argument('--sdk-publisher')
    args = parser.parse_args()
    required = ('repo', 'units', 'resources') if args.capture_only else ('sdk', 'identity', 'bundle_tool', 'out_root', 'sdk_root', 'sdk_publisher')
    if any(getattr(args, name) is None for name in required):
        parser.error('required capture or sealing input is missing')
    (capture if args.capture_only else export)(args)


if __name__ == '__main__':
    main()
