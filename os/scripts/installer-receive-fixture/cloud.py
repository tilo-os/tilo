"""Actual imported DAV account -> default helper -> Files -> hydrated FUSE bytes.

The caller owns the real user session bus/keyring and Services children. This
module never connects an account, opens plaintext credentials or substitutes a
vault provider. DAV expectations describe the disposable outer server only.
"""
import hashlib
import os
from pathlib import Path
import pwd
import stat
import time


def user_environment(account):
    current = pwd.getpwuid(os.getuid())
    if (os.getuid() == 0 or os.getresuid() != (current.pw_uid,) * 3
            or account.pw_uid != current.pw_uid or account.pw_dir != current.pw_dir):
        raise RuntimeError('cloud consumer is not the actual receiving account')
    home = Path(current.pw_dir)
    expected = {'HOME': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
                'XDG_DATA_HOME': str(home / '.local/share'),
                'XDG_STATE_HOME': str(home / '.local/state'),
                'XDG_RUNTIME_DIR': '/run/user/' + str(current.pw_uid)}
    if any(os.environ.get(name) != value for name, value in expected.items()):
        raise RuntimeError('cloud consumer paths differ from the receiving account')
    if (not os.environ.get('DBUS_SESSION_BUS_ADDRESS')
            or any(name in os.environ for name in ('TILO_SECRETS_DIR', 'TILO_IDENTITY_FILE_SECRETS',
                                                   'TILO_ACCOUNT_CREDENTIAL_HELPER'))):
        raise RuntimeError('actual session keyring and default credential helper required')
    info = home.stat(follow_symlinks=False)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != current.pw_uid or info.st_mode & 0o022:
        raise RuntimeError('receiving Home is not owned by the actual account')
    return dict(os.environ), home


def start_services(services, account, deadline):
    environment, home = user_environment(account)
    runtime = Path(environment['XDG_RUNTIME_DIR'])
    mounts = home / 'Remote'
    state = Path(environment['XDG_STATE_HOME']) / 'tilo/cloud'
    for path in (mounts, state, runtime / 'tilo-fs-cloud', runtime / 'tilo-fs-cloud-files'):
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = path.stat(follow_symlinks=False)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != account.pw_uid or info.st_mode & (0o022 if path == mounts else 0o077):
            raise RuntimeError('cloud service directory is not private to the receiving account')
    if time.monotonic() >= deadline:
        raise RuntimeError('cloud receiving deadline elapsed')
    services.spawn(['/usr/bin/fs', 'cloud-service', '--mounts-root', str(mounts),
                    '--state-root', str(state), '--control-socket', str(runtime / 'tilo-fs-cloud/control.sock'),
                    '--file-socket', str(runtime / 'tilo-fs-cloud-files/files.sock')],
                   environment=environment)
    return home


def private_bytes(path, maximum):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            raise RuntimeError('cloud receiving byte subject exceeds bound')
        result = bytearray()
        while part := os.read(fd, min(65536, maximum + 1 - len(result))):
            result.extend(part)
            if len(result) > maximum:
                raise RuntimeError('cloud receiving byte subject grew beyond bound')
        after = os.fstat(fd)
        identity = lambda info: (info.st_dev, info.st_ino, info.st_size, info.st_mode,
                                 info.st_uid, info.st_gid, info.st_mtime_ns, info.st_ctime_ns)
        if identity(before) != identity(after) or len(result) != before.st_size:
            raise RuntimeError('cloud receiving byte subject changed while held')
        return bytes(result)
    finally:
        os.close(fd)


def mounted(path):
    fd = os.open('/proc/self/mountinfo', os.O_RDONLY | os.O_CLOEXEC)
    try:
        data = bytearray()
        while part := os.read(fd, min(65536, 1024 * 1024 + 1 - len(data))):
            data.extend(part)
            if len(data) > 1024 * 1024:
                raise RuntimeError('cloud mount observation exceeds bound')
    finally:
        os.close(fd)
    rows = []
    for line in data.decode('utf-8', errors='strict').splitlines():
        fields = line.split()
        if len(fields) < 10 or '-' not in fields:
            raise RuntimeError('cloud kernel mount observation malformed')
        point = fields[4].replace(r'\040', ' ').replace(r'\011', '\t').replace(r'\012', '\n').replace(r'\134', '\\')
        if point == str(path):
            separator = fields.index('-')
            rows.append(fields[separator + 1:separator + 3])
    if not rows:
        return False
    if rows != [['fuse.tilo-cloud', 'tilo-cloud']]:
        raise RuntimeError('cloud folder is not the actual cloud FUSE mount')
    return True


def expected_data(dav):
    fields = {'account_id', 'mount_name', 'relative', 'expected_hex', 'write_relative', 'write_hex'}
    if type(dav) is not dict or set(dav) != fields or any(type(value) is not str for value in dav.values()):
        raise RuntimeError('actual source cloud observation missing')
    if not dav['account_id'] or len(dav['account_id'].encode()) > 1024:
        raise RuntimeError('selected cloud account identity invalid')
    name = dav['mount_name']
    if not name or name in ('.', '..') or '/' in name or '\x00' in name or len(name.encode()) > 255:
        raise RuntimeError('source cloud mount observation invalid')
    for key in ('relative', 'write_relative'):
        path = Path(dav[key])
        if path.is_absolute() or len(path.parts) != 2 or path.parts[0] != 'Documents' or path.name in ('.', '..') or '\x00' in dav[key]:
            raise RuntimeError('owned cloud object observation invalid')
    decoded = []
    for key in ('expected_hex', 'write_hex'):
        value = dav[key]
        if not value or len(value) > 128 * 1024 or len(value) % 2 or any(c not in '0123456789abcdef' for c in value):
            raise RuntimeError('owned cloud object bytes invalid')
        decoded.append(bytes.fromhex(value))
    return decoded


def offered(control, identity, deadline, services, provider=None):
    while time.monotonic() < deadline:
        services.check_live()
        rows = [row for row in control.rows() if row.get('id') == identity
                and (provider is None or row.get('provider') == provider)]
        if len(rows) > 1:
            raise RuntimeError('selected cloud action is ambiguous')
        if rows:
            return rows[0]
        time.sleep(.05)
    raise RuntimeError('selected cloud action did not publish before deadline')


def entry_identity(path):
    value = os.fsencode(path)
    digest = hashlib.sha256(b'tilo.files.local-resource.v1\\0' + len(value).to_bytes(8, 'big') + value)
    return 'files.entry:' + digest.hexdigest()[:32]


def verify(control, account, dav, services, private_log, deadline):
    expected, written = expected_data(dav)
    _, home = user_environment(account)
    config = home / '.config/tilo/accounts'
    vault = config / 'vault'
    cipher = private_bytes(vault / 'vault.enc', 16 * 1024 * 1024)
    local_key = vault / 'vault.key'
    info = local_key.stat(follow_symlinks=False)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != account.pw_uid or info.st_mode & 0o077:
        raise RuntimeError('genuine imported local custody not present before consumption')
    start_services(services, account, deadline)
    mount = home / 'Remote' / dav['mount_name']
    while not mounted(mount):
        services.check_live()
        if time.monotonic() >= deadline:
            raise RuntimeError('default cloud consumer did not mount selected account')
        time.sleep(.05)
    resource = 'files.remote-account:' + dav['account_id'].encode().hex()
    opened = offered(control, 'open-remote-account@' + resource, deadline, services)
    control.terminal(control.invoke(opened))
    # Opening the genuine account folder must publish the actual Documents
    # entry. Follow that same Files action rather than a parallel file browser.
    documents = offered(control, 'open@' + entry_identity(mount / 'Documents'), deadline, services, opened['provider'])
    control.terminal(control.invoke(documents))
    offered(control, 'open@' + entry_identity(mount / dav['relative']), deadline, services, opened['provider'])
    actual = private_bytes(mount / dav['relative'], 65536)
    if actual != expected:
        raise RuntimeError('cloud FUSE hydration differs from actual server object')
    target = mount / dav['write_relative']
    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        offset = 0
        while offset < len(written):
            if time.monotonic() >= deadline:
                raise RuntimeError('actual cloud write exceeded deadline')
            offset += os.write(fd, written[offset:])
        os.fsync(fd)
    finally:
        os.close(fd)
    if os.path.lexists(local_key) or private_bytes(vault / 'vault.enc', 16 * 1024 * 1024) != cipher:
        raise RuntimeError('default account helper did not preserve imported grant custody')
    if not mounted(mount):
        raise RuntimeError('cloud mount disappeared during actual receiving use')
    services.check_live()
    # Outer owned DAV server must additionally observe exact GET/PUT bytes;
    # a local queued write does not assert successful server upload by itself.
    return {'schema': 1, 'files_account_opened': True, 'cloud_fuse_mounted': True,
            'hydrated_sha256': hashlib.sha256(actual).hexdigest(),
            'written_sha256': hashlib.sha256(written).hexdigest(),
            'local_key_retired': True, 'imported_ciphertext_preserved': True,
            'server_upload_observed': False}
