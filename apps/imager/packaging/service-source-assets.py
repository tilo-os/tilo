"""Stage complete measured public distributions; never launch or mint sessions.

Called by the canonical capture qualifier before its newly owned container.
The pinned operator manifest supplies public bytes, not source access rights.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat

COUNTS = {'electron': 72, 'slack': 122, 'app': 2}
MAXIMUM = 512 * 1024 * 1024
TOTAL = 1024 * 1024 * 1024
ASSET_MANIFEST_SHA256 = '22deea1fc1bcb2ad47b553e363594bb276ef412824ad236210034c9f318f6005'


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
    result = set() if result is None else result
    with os.scandir(root) as entries:
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
                require(len(result) <= 196, 'public tree inventory exceeds bound')
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
    require(hashlib.sha256((json.dumps(assets, sort_keys=True, indent=2) + '\n').encode()).hexdigest()
            == ASSET_MANIFEST_SHA256, 'official public asset manifest differs from frozen actual subjects')
    require(type(assets) is dict and assets.get('schema') == 1
            and set(assets['assets']) == set(COUNTS), 'fixed official asset groups required')
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
            for group, count in COUNTS.items():
                row = assets['assets'][group]
                require(set(row) == {'source_root', 'files'} and len(row['files']) == count,
                        'incomplete fixed public distribution')
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
            return {'schema': 1, 'files': sum(COUNTS.values()), 'native_executed': False}
        finally:
            for _, fd, _, _ in roots:
                os.close(fd)
    finally:
        os.close(held_candidate)
