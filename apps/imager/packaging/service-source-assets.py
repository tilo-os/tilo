"""Stage complete measured public distributions; never launch or mint sessions.

Called by the canonical capture qualifier before its newly owned container.
The pinned operator manifest supplies public bytes, not source access rights.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat

GROUPS = ('electron', 'slack', 'app')
DISTRIBUTION = {'platform': 'linux', 'architecture': 'x86_64', 'electron': '44.4.5',
                'slack': '4.52.178', 'slack_snap_revision': 263}
# Independent archive pins, not assertions supplied by an operator manifest.
ARCHIVE_SHA256 = {
    'electron_archive': '04586a0ec46c3283fbdaef85530f561f71f0b5e136ad0cb9ef63683615609780',
    'slack_archive': '2f52f41d38d20df48fe060978cdb3c7dad662df1099705d389f0d0c3444b194f',
}
ARCHIVE_URLS = {
    'electron_archive': 'https://github.com/electron/electron/releases/download/v44.4.5/electron-v44.4.5-linux-x64.zip',
    'slack_archive': 'https://api.snapcraft.io/api/v1/snaps/download/JUJH91Ved74jd4ZgJCpzMBtYbPOzTlsD_263.snap',
}
FIXTURE_ROOT = Path(__file__).resolve().parents[3] / 'os/scripts/service-session-source-fixture/app'
MAXIMUM = 512 * 1024 * 1024
TOTAL = 1024 * 1024 * 1024
# Canonical identity includes archives, every extracted file fact and fixed endpoints;
# only source_root/source_path locators are excluded. The checked-in subject ledger
# is measured public bytes, not evidence that any native runtime/session worked.
ASSET_MANIFEST_SHA256 = 'f088c5f9dec427570879bdf6c4e1582adb7722c242a4907c6552e2c8f196cd4e'


def subject(assets):
    require(type(assets) is dict and set(assets) in (
        {'schema', 'archives', 'assets', 'destinations', 'namespace_endpoints',
         'native_executed', 'provider_authentication_verified'},
        {'schema', 'archives', 'assets', 'destinations', 'namespace_endpoints',
         'native_executed', 'provider_authentication_verified', 'distribution'}),
        'official public asset manifest shape differs')
    require(type(assets['schema']) is int and assets['schema'] == 1
            and assets['native_executed'] is False
            and assets['provider_authentication_verified'] is False,
            'official public assets cannot claim native authentication')
    require(type(assets['assets']) is dict and set(assets['assets']) == set(GROUPS)
            and type(assets['archives']) is dict
            and set(assets['archives']) == set(ARCHIVE_SHA256),
            'official public asset manifest groups differ')
    result = dict(assets, distribution=assets.get('distribution', DISTRIBUTION))
    result['assets'] = {}
    result['archives'] = {}
    for group, row in assets['assets'].items():
        require(type(row) is dict and set(row) in ({'files'}, {'source_root', 'files'})
                and type(row['files']) is dict and 0 < len(row['files']) <= 4096,
                'official public asset manifest file ledger differs')
        result['assets'][group] = {'files': row['files']}
    for key, row in assets['archives'].items():
        require(type(row) is dict and set(row) in (
            {'bytes', 'mode', 'sha256'}, {'source_path', 'bytes', 'mode', 'sha256'})
                and row['sha256'] == ARCHIVE_SHA256[key],
                'official upstream archive digest differs')
        result['archives'][key] = {k: v for k, v in row.items() if k != 'source_path'}
    return result


def subject_digest(assets):
    return hashlib.sha256(json.dumps(subject(assets), sort_keys=True,
                                     separators=(',', ':')).encode()).hexdigest()


def validate(assets, locators=True):
    require(subject_digest(assets) == ASSET_MANIFEST_SHA256,
            'official public asset manifest differs from frozen actual subjects')
    if locators:
        for row in assets['assets'].values():
            require('source_root' in row, 'public asset root locator required')
            locator(row['source_root'])
        for row in assets['archives'].values():
            require('source_path' in row, 'public archive locator required')
            locator(row['source_path'])
    return assets


def locator(value):
    require(type(value) is str and '\x00' not in value, 'invalid public asset locator')
    path = Path(value)
    require(path.is_absolute() and '..' not in path.parts and path.as_posix() == value,
            'public asset locator must be a canonical absolute path')
    return path


def require(value, reason):
    if not value:
        raise ValueError(reason)


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def digest_shape(value):
    return type(value) is str and len(value) == 64 and all(c in '0123456789abcdef' for c in value)


def name(value):
    require(type(value) is str and value and '\x00' not in value and len(value.encode()) <= 1024,
            'invalid public asset name')
    path = PurePosixPath(value)
    require(len(path.parts) <= 16 and not path.is_absolute() and path.as_posix() == value
            and all(part not in ('', '.', '..') for part in path.parts), 'redirected public asset name')
    return path.parts


def directory(path):
    path = Path(path)
    require(path.is_absolute(), 'public asset root must be absolute')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in path.parts[1:]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                              dir_fd=fd)
            os.close(fd)
            fd = next_fd
        info = os.fstat(fd)
        require(not info.st_mode & 0o022, 'public asset directory is writable by another principal')
        return fd
    except BaseException:
        os.close(fd)
        raise


def open_leaf(root, relative):
    parts = name(relative)
    fd = os.dup(root)
    try:
        for part in parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                              dir_fd=fd)
            try:
                info = os.fstat(next_fd)
                require(not info.st_mode & 0o022, 'unsafe public asset ancestor')
            except BaseException:
                os.close(next_fd)
                raise
            os.close(fd)
            fd = next_fd
        leaf = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                       dir_fd=fd)
        return fd, parts[-1], leaf
    except BaseException:
        os.close(fd)
        raise


def enumerate_files(root, prefix='', result=None):
    # scandir(fd) may share the caller's directory stream offset. Reopen the
    # held directory, rather than dup it, so each inventory starts at the first
    # entry and repeated mutation checks do not depend on Python/libc behavior.
    fd = os.open('.', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                 dir_fd=root)
    try:
        return _enumerate_files(root, fd, prefix, result)
    finally:
        os.close(fd)


def _enumerate_files(root, scan, prefix, result):
    result = set() if result is None else result
    with os.scandir(scan) as entries:
        count = 0
        for entry in entries:
            count += 1
            require(count <= 256, 'public directory inventory exceeds bound')
            relative = prefix + entry.name
            name(relative)
            info = entry.stat(follow_symlinks=False)
            require(not stat.S_ISLNK(info.st_mode), 'public asset alias refused')
            if stat.S_ISDIR(info.st_mode):
                fd = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                             dir_fd=root)
                try:
                    before = os.fstat(fd)
                    require(stamp(before) == stamp(info) and not before.st_mode & 0o022,
                            'public inventory directory changed')
                    enumerate_files(fd, relative + '/', result)
                    require(stamp(before) == stamp(os.fstat(fd))
                            == stamp(os.stat(entry.name, dir_fd=root, follow_symlinks=False)),
                            'public inventory directory changed')
                finally:
                    os.close(fd)
            else:
                require(stat.S_ISREG(info.st_mode), 'nonregular public asset refused')
                result.add(relative)
                require(len(result) <= 4096, 'public tree inventory exceeds bound')
    return result


def measured_file(root, relative, expected, destination=None):
    require(type(expected) is dict and set(expected) == {'bytes', 'mode', 'sha256'}
            and type(expected['bytes']) is int and 0 < expected['bytes'] <= MAXIMUM
            and type(expected['mode']) is int and expected['mode'] in (0o644, 0o755)
            and digest_shape(expected['sha256']), 'public file fact shape refused')
    parent, leaf, fd = open_leaf(root, relative)
    target = None
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_size == expected['bytes']
                and stat.S_IMODE(before.st_mode) == expected['mode'], 'public file type/size/mode differs')
        if destination is not None:
            require(type(destination) is tuple and len(destination) == 2
                    and type(destination[0]) is int and len(name(destination[1])) == 1,
                    'staging destination must be an exact held-directory leaf')
            target = os.open(destination[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                             | os.O_CLOEXEC, 0o600, dir_fd=destination[0])
        digest = hashlib.sha256()
        total = 0
        while part := os.read(fd, min(1024 * 1024, expected['bytes'] + 1 - total)):
            total += len(part)
            require(total <= expected['bytes'], 'public asset grew beyond measured size')
            digest.update(part)
            if target is not None:
                view = memoryview(part)
                while view:
                    count = os.write(target, view)
                    require(count > 0, 'public staging write made no progress')
                    view = view[count:]
        require(total == expected['bytes'] and digest.hexdigest() == expected['sha256']
                and stamp(before) == stamp(os.fstat(fd))
                == stamp(os.stat(leaf, dir_fd=parent, follow_symlinks=False)),
                'public asset changed or digest differs')
        if target is not None:
            os.fchmod(target, expected['mode'])
            os.fsync(target)
    finally:
        if target is not None:
            os.close(target)
        os.close(fd)
        os.close(parent)


def new_directory(parent, leaf):
    require(len(name(leaf)) == 1, 'staging directory must be one leaf')
    os.mkdir(leaf, mode=0o700, dir_fd=parent)
    fd = os.open(leaf, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                 dir_fd=parent)
    try:
        info = os.fstat(fd)
        require(info.st_uid == 0 and stat.S_IMODE(info.st_mode) == 0o700
                and stamp(info) == stamp(os.stat(leaf, dir_fd=parent, follow_symlinks=False)),
                'new public staging directory changed')
        os.fchmod(fd, 0o755)
        return fd
    except BaseException:
        os.close(fd)
        raise


def staging_parent(root, parts):
    fd = os.dup(root)
    try:
        for part in parts:
            try:
                next_fd = new_directory(fd, part)
            except FileExistsError:
                next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                                  dir_fd=fd)
                try:
                    info = os.fstat(next_fd)
                    require(info.st_uid == 0 and stat.S_IMODE(info.st_mode) == 0o755,
                            'existing staging parent differs from owned public directory')
                except BaseException:
                    os.close(next_fd)
                    raise
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def stage(assets, candidate):
    """Copy only fixed public groups into fresh candidate/service-source.

    The canonical qualifier owns candidate cleanup on every error. This helper
    leaves failed staging to that same owner rather than deleting named paths.
    """
    validate(assets)
    require(os.geteuid() == 0, 'public candidate staging requires its actual root owner')
    candidate = Path(candidate)
    held_candidate = directory(candidate)
    try:
        candidate_info = os.fstat(held_candidate)
        require(candidate_info.st_uid == 0 and stat.S_IMODE(candidate_info.st_mode) in (0o700, 0o755),
                'candidate public staging owner differs')
        # Verify archive subjects too; neither a pathname nor old saved receipt
        # certifies the supplied distribution. Complete extracted trees below
        # remain independently bound to their exact frozen file ledger.
        for key in ('electron_archive', 'slack_archive'):
            row = dict(assets['archives'][key])
            path = Path(row.pop('source_path'))
            archive_parent = directory(path.parent)
            try:
                measured_file(archive_parent, path.name, row)
            finally:
                os.close(archive_parent)
        roots = []
        try:
            total = 0
            for group in GROUPS:
                row = assets['assets'][group]
                fd = directory(Path(row['source_root']))
                roots.append((group, fd, stamp(os.fstat(fd)), row))
                require(enumerate_files(fd) == set(row['files']), 'public tree contains missing or extra files')
                total += sum(item['bytes'] for item in row['files'].values())
            require(total <= TOTAL, 'public source closure exceeds aggregate bound')
            stage_fd = new_directory(held_candidate, 'service-source')
            try:
                for group, fd, before, row in roots:
                    group_fd = new_directory(stage_fd, group)
                    try:
                        for relative, expected in sorted(row['files'].items()):
                            parts = name(relative)
                            parent_fd = staging_parent(group_fd, parts[:-1])
                            try:
                                measured_file(fd, relative, expected, (parent_fd, parts[-1]))
                            finally:
                                os.close(parent_fd)
                        require(before == stamp(os.fstat(fd)) == stamp(Path(row['source_root']).lstat())
                                and enumerate_files(fd) == set(row['files']),
                                'public source inventory changed during staging')
                        require(enumerate_files(group_fd) == set(row['files']),
                                'staged public inventory differs')
                        for relative, expected in row['files'].items():
                            measured_file(group_fd, relative, expected)
                        os.fsync(group_fd)
                    finally:
                        os.close(group_fd)
                os.fsync(stage_fd)
            finally:
                os.close(stage_fd)
            require(stamp(candidate_info)[:5] == stamp(os.fstat(held_candidate))[:5]
                    == stamp(candidate.lstat())[:5], 'owned candidate namespace was replaced')
            return {'schema': 1, 'files': sum(len(row['files']) for row in assets['assets'].values()),
                    'subject_sha256': subject_digest(assets), 'distribution': dict(DISTRIBUTION),
                    'native_executed': False}
        finally:
            for _, fd, _, _ in roots:
                os.close(fd)
    finally:
        os.close(held_candidate)


def verify(assets):
    """Measure actual supplied archives and complete trees, without native execution."""
    validate(assets)
    for row in assets['archives'].values():
        path = locator(row['source_path'])
        fd = directory(path.parent)
        try:
            measured_file(fd, path.name, {k: v for k, v in row.items() if k != 'source_path'})
        finally:
            os.close(fd)
    total = 0
    for row in assets['assets'].values():
        path = locator(row['source_root'])
        fd = directory(path)
        try:
            before = stamp(os.fstat(fd))
            require(enumerate_files(fd) == set(row['files']),
                    'public tree contains missing or extra files')
            for relative, expected in row['files'].items():
                measured_file(fd, relative, expected)
                total += expected['bytes']
            require(before == stamp(os.fstat(fd)) == stamp(path.lstat())
                    and enumerate_files(fd) == set(row['files']), 'public inventory changed')
        finally:
            os.close(fd)
    require(total <= TOTAL, 'public source closure exceeds aggregate bound')
    return assets


def verify_tree(path, files):
    fd = directory(path)
    try:
        require(enumerate_files(fd) == set(files), 'public tree contains missing or extra files')
        for relative, expected in files.items():
            measured_file(fd, relative, expected)
    finally:
        os.close(fd)


def recover(root):
    """Recover public inputs on durable storage, never execute the test app.

    Existing bytes are verified, not overwritten or used to update any pin.
    The app is a checkout-owned synthetic fixture, NOT an upstream Slack app.
    """
    import shutil
    import subprocess
    import zipfile

    root = locator(str(root))
    require(root.parts[1] in ('srv', 'var', 'home') and root.name == 'capture-public-assets',
            'recovery requires durable capture-public-assets storage, not temporary memory')
    root.mkdir(mode=0o755, parents=True, exist_ok=True)
    fd = directory(root)
    os.close(fd)
    assets = json.loads(Path(__file__).with_name('service-source-subjects.json').read_text())
    validate(assets, locators=False)
    # Measure the actual checkout fixture before any download or copy.
    verify_tree(FIXTURE_ROOT, assets['assets']['app']['files'])
    for key, filename in (('electron_archive', 'electron-v44.4.5-linux-x64.zip'),
                          ('slack_archive', 'slack-263.snap')):
        path = root / filename
        if not os.path.lexists(path):
            partial = path.with_suffix(path.suffix + '.partial')
            try:
                with partial.open('xb') as output:
                    subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error',
                                    '--proto', '=https', '--proto-redir', '=https',
                                    '--connect-timeout', '30', '--max-time', '900',
                                    ARCHIVE_URLS[key]], stdout=output, check=True)
                partial.chmod(0o644)
                held = directory(root)
                try:
                    measured_file(held, partial.name, assets['archives'][key])
                finally:
                    os.close(held)
                require(not os.path.lexists(path), 'archive destination appeared during recovery')
                partial.rename(path)
            except BaseException as error:
                # Retain partial bytes for inspection; never admit them.
                raise ValueError('public archive recovery failed for ' + ARCHIVE_URLS[key]
                                 + ': ' + str(error)) from error
        held = directory(root)
        try:
            measured_file(held, path.name, assets['archives'][key])
        finally:
            os.close(held)
        assets['archives'][key]['source_path'] = str(path)

    electron = root / 'electron'
    if not os.path.lexists(electron):
        electron.mkdir(mode=0o755)
        try:
            with zipfile.ZipFile(assets['archives']['electron_archive']['source_path']) as archive:
                files = assets['assets']['electron']['files']
                entries = [entry for entry in archive.infolist() if not entry.is_dir()]
                require(len(entries) == len(files) and {entry.filename for entry in entries} == set(files),
                        'official Electron ZIP inventory differs')
                for entry in archive.infolist():
                    name(entry.filename.rstrip('/'))
                    require(not stat.S_ISLNK(entry.external_attr >> 16), 'Electron ZIP alias refused')
                for entry in entries:
                    destination = electron.joinpath(*name(entry.filename))
                    destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
                    with archive.open(entry) as source, destination.open('xb') as target:
                        shutil.copyfileobj(source, target)
                    # ZIP extraction mode policy is frozen by the measured ledger.
                    destination.chmod(files[entry.filename]['mode'])
            verify_tree(electron, files)
        except BaseException:
            shutil.rmtree(electron)
            raise
    slack = root / 'slack'
    if not os.path.lexists(slack):
        require(shutil.which('unsquashfs') is not None,
                'Slack archive extraction requires host squashfs-tools (unsquashfs)')
        try:
            subprocess.run(['unsquashfs', '-no-progress', '-processors', '1', '-d', str(slack),
                            assets['archives']['slack_archive']['source_path'], 'usr/lib/slack'],
                           check=True, stdout=subprocess.DEVNULL)
            verify_tree(slack / 'usr/lib/slack', assets['assets']['slack']['files'])
        except BaseException:
            if slack.exists():
                shutil.rmtree(slack)
            raise
    app = root / 'app'
    if not os.path.lexists(app):
        shutil.copytree(FIXTURE_ROOT, app, symlinks=True)
    for group, path in (('electron', electron), ('slack', slack / 'usr/lib/slack'), ('app', app)):
        assets['assets'][group]['source_root'] = str(path)
    return verify(assets)


def main():
    """Create portable operator inputs using only pinned, locally measured public bytes."""
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--subjects', type=Path,
                        default=Path(__file__).with_name('service-source-subjects.json'))
    parser.add_argument('--recover', type=Path,
                        help='durable absolute .../target/capture-public-assets directory')
    parser.add_argument('--electron-root')
    parser.add_argument('--electron-archive')
    parser.add_argument('--slack-root')
    parser.add_argument('--slack-archive')
    parser.add_argument('--app-root', default=str(FIXTURE_ROOT))
    args = parser.parse_args()
    if args.recover:
        require(args.subjects == Path(__file__).with_name('service-source-subjects.json')
                and not any((args.electron_root, args.electron_archive,
                             args.slack_root, args.slack_archive))
                and args.app_root == str(FIXTURE_ROOT),
                'recovery uses only frozen subjects and the actual checkout fixture')
        print(json.dumps(recover(args.recover), sort_keys=True, indent=2))
        return
    require(all((args.electron_root, args.electron_archive, args.slack_root, args.slack_archive)),
            'all archive and tree locators required unless --recover is supplied')
    assets = json.loads(args.subjects.read_text())
    validate(assets, locators=False)
    for group in GROUPS:
        assets['assets'][group]['source_root'] = str(locator(getattr(args, group + '_root')))
    for group in ('electron', 'slack'):
        assets['archives'][group + '_archive']['source_path'] = str(
            locator(getattr(args, group + '_archive')))
    verify(assets)
    print(json.dumps(assets, sort_keys=True, indent=2))


if __name__ == '__main__':
    main()
