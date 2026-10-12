"""Read-only kernel witnesses for actual signed Browser/CEF namespace isolation.

No PID is signalled here. The existing source/target namespace owner retains
failure cleanup, and normal desktop window close owns the launched programs.
"""
import ctypes
import hashlib
import os
from pathlib import Path, PurePosixPath
import select
import stat
import struct

MAX_MOUNTS = 1024 * 1024


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def read(fd, maximum):
    result = bytearray()
    while part := os.read(fd, min(65536, maximum + 1 - len(result))):
        result.extend(part)
        require(len(result) <= maximum, 'kernel service observation exceeds bound')
    return bytes(result)


def file_bytes(path, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        return read(fd, maximum)
    finally:
        os.close(fd)


def rows(raw):
    output = []
    for line in raw.decode('utf-8', errors='strict').splitlines():
        fields = line.split()
        require(len(fields) >= 10 and '-' in fields, 'kernel mount row malformed')
        separator = fields.index('-')
        require(separator >= 6 and len(fields) >= separator + 4, 'kernel mount fields malformed')
        def unescape(value):
            return value.replace(r'\040', ' ').replace(r'\011', '\t').replace(r'\012', '\n').replace(r'\134', '\\')
        root, point = unescape(fields[3]), unescape(fields[4])
        require(root.startswith('/') and point.startswith('/'), 'kernel mount path is not absolute')
        output.append({'id': int(fields[0]), 'device': fields[2], 'root': root, 'point': point})
        require(len(output) <= 8192, 'kernel mount inventory exceeds bound')
    return output


def directory(path):
    require(path.is_absolute() and path.resolve(strict=True) == path, 'vault coordinate is redirected')
    fd = os.open('/', os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in path.parts[1:]:
            child = os.open(part, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=fd)
            os.close(fd); fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def coordinate(account):
    # accounts::config_root()/accounts_dir_in() are the canonical actual
    # receiving root. Read no metadata store or encrypted/plaintext vault bytes.
    path = (Path(os.environ['XDG_CONFIG_HOME']) / 'tilo/accounts').resolve(strict=True)
    fd = directory(path)
    try:
        before = os.fstat(fd)
        require(before.st_uid == account.pw_uid and not before.st_mode & 0o022,
                'actual account custody directory owner differs')
        buffer = ctypes.create_string_buffer(256)
        libc = ctypes.CDLL(None, use_errno=True)
        require(libc.statx(fd, b'', 0x1000, 0x1000, buffer) == 0,
                'actual account kernel mount identity unavailable')
        require(struct.unpack_from('=I', buffer.raw, 0)[0] & 0x1000,
                'kernel omitted actual account mount identity')
        mount_id = struct.unpack_from('=Q', buffer.raw, 144)[0]
        table = rows(file_bytes('/proc/self/mountinfo', MAX_MOUNTS))
        matches = [row for row in table if row['id'] == mount_id]
        require(len(matches) == 1, 'account mount row is not unique')
        row = matches[0]
        point = PurePosixPath(row['point'])
        require(PurePosixPath(path).is_relative_to(point), 'account path differs from held kernel mount')
        relative = PurePosixPath(path).relative_to(point)
        location = PurePosixPath(row['root']) / relative
        require(stamp(before) == stamp(os.fstat(fd)) == stamp(path.lstat()),
                'actual account coordinate changed')
        return {'device': row['device'], 'root': str(location)}
    finally:
        os.close(fd)


def no_vault(table, vault):
    sensitive = PurePosixPath(vault['root'])
    for row in table:
        if row['device'] != vault['device']:
            continue
        root = PurePosixPath(row['root'])
        require(not sensitive.is_relative_to(root) and not root.is_relative_to(sensitive),
                'actual Browser/CEF namespace exposes account custody')


def process_start(raw):
    # comm can contain spaces and parentheses; only kernel fields after the
    # final ')' carry the documented starttime (field22).
    suffix = raw.rsplit(b')', 1)[-1].split()
    require(len(suffix) >= 20 and suffix[19].isdigit(), 'kernel process start identity malformed')
    return int(suffix[19])


def live(pidfd):
    poll = select.poll(); poll.register(pidfd, select.POLLIN | select.POLLERR | select.POLLHUP)
    return not poll.poll(0)


def observe(subjects, account, vault):
    require(type(subjects) is dict and set(subjects) == {
        'browser_sha256', 'browser_bytes', 'cef_sha256', 'cef_bytes'}, 'authenticated executable subjects missing')
    for kind in ('browser', 'cef'):
        require(type(subjects[kind + '_bytes']) is int and 0 < subjects[kind + '_bytes'] <= 256 * 1024 * 1024
                and type(subjects[kind + '_sha256']) is str and len(subjects[kind + '_sha256']) == 64
                and all(c in '0123456789abcdef' for c in subjects[kind + '_sha256']),
                'authenticated executable subject shape differs')
    owned, count = [], 0
    try:
        with os.scandir('/proc') as entries:
            for entry in entries:
                count += 1
                require(count <= 32768, 'kernel process inventory exceeds bound')
                if not entry.name.isdecimal():
                    continue
                pid = int(entry.name)
                base = Path('/proc') / entry.name
                try:
                    if base.stat().st_uid != account.pw_uid:
                        continue
                    pidfd = os.pidfd_open(pid, 0)
                except ProcessLookupError:
                    continue
                matched = False
                try:
                    start = process_start(file_bytes(base / 'stat', 8192))
                    # proc/exe is a kernel-held executable reference. Following
                    # this magic link is intentional; no caller path is opened.
                    fd = os.open(base / 'exe', os.O_RDONLY | os.O_CLOEXEC)
                    try:
                        before = os.fstat(fd)
                        kinds = [kind for kind in ('browser', 'cef')
                                 if before.st_size == subjects[kind + '_bytes']]
                        if not kinds:
                            continue
                        require(stat.S_ISREG(before.st_mode), 'kernel executable is not regular')
                        digest = hashlib.sha256(read(fd, before.st_size)).hexdigest()
                        require(stamp(before) == stamp(os.fstat(fd)), 'kernel executable changed while held')
                    finally:
                        os.close(fd)
                    kinds = [kind for kind in kinds if digest == subjects[kind + '_sha256']]
                    arguments = file_bytes(base / 'cmdline', 32768).split(b'\0') if kinds else []
                    require(not any(argument in (b'--no-sandbox', b'--disable-setuid-sandbox',
                        b'--disable-seccomp-filter-sandbox') for argument in arguments),
                        'actual CEF consumer disabled its native sandbox')
                    if not kinds:
                        continue
                    require(len(kinds) == 1 and live(pidfd), 'actual signed process identity is ambiguous or retired')
                    namespace = os.open(base / 'ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
                    try:
                        ns = os.fstat(namespace)
                        parent_ns = Path('/proc/self/ns/mnt').stat()
                        require((ns.st_dev, ns.st_ino) != (parent_ns.st_dev, parent_ns.st_ino),
                                'actual consumer has no separate sandbox mount namespace')
                        no_vault(rows(file_bytes(base / 'mountinfo', MAX_MOUNTS)), vault)
                        named = (base / 'ns/mnt').stat()
                        require((ns.st_dev, ns.st_ino) == (named.st_dev, named.st_ino)
                                and start == process_start(file_bytes(base / 'stat', 8192)) and live(pidfd),
                                'actual consumer namespace/process changed during observation')
                    finally:
                        os.close(namespace)
                    owned.append({'pidfd': pidfd, 'kind': kinds[0], 'pid': pid, 'start': start, 'arguments': arguments})
                    matched = True
                    require(len(owned) <= 8, 'actual signed consumers exceed owned fixture bound')
                except (FileNotFoundError, ProcessLookupError):
                    # A disappeared process is not isolation evidence. It may
                    # be observed afresh while the real import is still live.
                    pass
                finally:
                    if not matched:
                        os.close(pidfd)
        return owned
    except BaseException:
        close(owned)
        raise


def close(observations):
    for row in observations:
        os.close(row['pidfd'])


def private_profile(observations, selected, account):
    matches = []
    for row in observations:
        if row['kind'] != 'cef':
            continue
        arguments = row['arguments']
        if ('--service-session=' + selected['id']).encode() not in arguments:
            continue
        paths = [argument[len(b'--profile='):] for argument in arguments
                 if argument.startswith(b'--profile=')]
        require(len(paths) == 1, 'actual service engine lacks one dedicated profile')
        path = PurePosixPath(os.fsdecode(paths[0]))
        require(path.is_absolute() and all(part not in ('.', '..') for part in path.parts),
                'actual service profile path is redirected')
        parts = path.parts
        indexes = [index for index, part in enumerate(parts) if part == 'service-sessions']
        require(len(indexes) == 1, 'actual service profile is not isolated')
        index = indexes[0]
        require(len(parts) == index + 4 and len(parts[index + 1]) == 64
                and all(char in '0123456789abcdef' for char in parts[index + 1])
                and parts[index + 2] == selected['id'] and parts[index + 3].startswith('attempt-'),
                'actual service profile differs from the selected identity/account')
        fd = os.open('/proc/' + str(row['pid']) + '/root', os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            for part in parts[1:]:
                child = os.open(part, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                                dir_fd=fd)
                os.close(fd); fd = child
            info = os.fstat(fd)
            require(info.st_uid == account.pw_uid and stat.S_IMODE(info.st_mode) == 0o700
                    and live(row['pidfd'])
                    and row['start'] == process_start(file_bytes('/proc/' + str(row['pid']) + '/stat', 8192)),
                    'actual private service profile/process changed')
        finally:
            os.close(fd)
        matches.append(row)
    require(len(matches) == 1, 'actual live dedicated CEF service engine is not unique')
