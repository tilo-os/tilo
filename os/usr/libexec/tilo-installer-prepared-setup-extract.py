#!/usr/bin/env python3
"""Carry a bounded opaque encrypted setup file from granted mounted media.

This does not authenticate/decrypt a bundle or authorize identity adoption. The
native installer must require its chosen password and the PreparedSetup purpose.
An unmounted EFI filesystem on the boot medium can be held read-only during
extraction. No sector writes, tail offsets or filesystem creation exist.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import stat
import subprocess
import tempfile
from contextlib import contextmanager

DIRECTORY = '.tilo-installer'
FILE = 'prepared-setup.sealed'
MAX_BYTES = 32 * 1024 * 1024 + 16384
RECEIPT_FILE = 'receipt.json'
UNUSED_STAGING_SLOT = object()


def validate_receipt(receipt, data):
    validate_receipt_digest(receipt, len(data), hashlib.sha256(data).hexdigest())


def validate_receipt_digest(receipt, size, digest):
    if (not isinstance(receipt, dict) or set(receipt) != {'schema', 'sealed_bytes', 'ciphertext_sha256'}
            or type(receipt['schema']) is not int or receipt['schema'] != 1
            or type(receipt['sealed_bytes']) is not int or not 48 <= receipt['sealed_bytes'] <= MAX_BYTES
            or not isinstance(receipt['ciphertext_sha256'], str)
            or len(receipt['ciphertext_sha256']) != 64
            or any(c not in '0123456789abcdef' for c in receipt['ciphertext_sha256'])
            or size != receipt['sealed_bytes']
            or digest != receipt['ciphertext_sha256']):
        raise ValueError('expected prepared setup ciphertext is missing, changed or malformed')


def read_receipt(parent, owner, private=True):
    fd = os.open(RECEIPT_FILE, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or (owner is not None and before.st_uid != owner)
                or (private and before.st_mode & 0o077) or before.st_size > 1024):
            raise ValueError('prepared setup receipt is not a bounded private file')
        data = os.read(fd, 1025)
        if len(data) != before.st_size or snapshot(os.fstat(fd)) != snapshot(before):
            raise ValueError('prepared setup receipt changed')
        if len(data) == 1024 and data == b' ' * 1024:
            return UNUSED_STAGING_SLOT  # Typed absence, never JSON null or a receipt.
        return json.loads(data)
    finally:
        os.close(fd)


def preflight(source_path, receipt_json):
    if len(receipt_json) > 1024:
        raise ValueError('prepared setup receipt exceeds bound')
    receipt = json.loads(receipt_json)
    parent = directory(source_path)
    try:
        facts = os.fstat(parent)
        if facts.st_uid != os.geteuid() or facts.st_mode & 0o022:
            raise ValueError('prepared setup source directory is not protected')
        data = read_ciphertext(parent, os.geteuid())
        validate_receipt(receipt, data)
        publish_ciphertext(parent, json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode(),
                           os.geteuid(), os.getegid(), RECEIPT_FILE)
    finally:
        os.close(parent)


def preflight_profile(source_path):
    """Public profile expectations remain enforced even if graphical repair drops them."""
    try:
        parent = directory(source_path)
    except FileNotFoundError:
        return
    try:
        try:
            os.stat('prepared-setup.error', dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise ValueError('prepared setup extraction reported a failure')
        try:
            fd = os.open('profile.json', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                         dir_fd=parent)
        except FileNotFoundError:
            fd = None
        receipt = None
        if fd is not None:
            try:
                before = os.fstat(fd)
                if not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid() or before.st_mode & 0o022 or before.st_size > 32768:
                    raise ValueError('installation profile is not a bounded protected file')
                data = os.read(fd, 32769)
                if len(data) != before.st_size or snapshot(os.fstat(fd)) != snapshot(before):
                    raise ValueError('installation profile changed')
                profile = json.loads(data)
                if not isinstance(profile, dict):
                    raise ValueError('installation profile is not an object')
                if 'prepared_setup' in profile:
                    receipt = profile['prepared_setup']
                    # A present malformed expectation must not become absence.
                    if not isinstance(receipt, dict):
                        raise ValueError('prepared setup expectation is malformed')
            finally:
                os.close(fd)
        if receipt is not None:
            preflight(source_path, json.dumps(receipt))
        else:
            try:
                delivered = read_receipt(parent, os.geteuid())
            except FileNotFoundError:
                delivered = None
            if delivered is not None:
                preflight(source_path, json.dumps(delivered))
                return
            for name in (FILE, 'prepared-setup.error'):
                try:
                    os.stat(name, dir_fd=parent, follow_symlinks=False)
                except FileNotFoundError:
                    continue
                raise ValueError('prepared setup artifact has no matching public expectation')
    finally:
        os.close(parent)


def directory(path):
    path = Path(path)
    if not path.is_absolute() or any(part in ('', '.', '..') for part in path.parts[1:]):
        raise ValueError('setup delivery requires an explicit absolute directory grant')
    descriptor = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in path.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def source(root):
    parent = directory(root)
    child = None
    try:
        child = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                        dir_fd=parent)
        descriptor = os.open(FILE, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                             dir_fd=child)
        try:
            receipt = read_receipt(child, None, private=False)
        except FileNotFoundError as error:
            os.close(descriptor)
            raise ValueError('media prepared setup is missing its expected receipt') from error
        except BaseException:
            os.close(descriptor)
            raise
    except FileNotFoundError:
        if child is not None:
            try:
                os.stat(RECEIPT_FILE, dir_fd=child, follow_symlinks=False)
            except FileNotFoundError:
                pass
            else:
                raise ValueError('media receipt expects a missing prepared setup artifact')
        return None
    finally:
        if child is not None:
            os.close(child)
        os.close(parent)
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or not 48 <= info.st_size <= MAX_BYTES:
            raise ValueError('encrypted setup file has invalid type or bounded size')
        if receipt is UNUSED_STAGING_SLOT:
            if info.st_size != MAX_BYTES:
                raise ValueError('unused prepared setup reservation has invalid size')
            while True:
                chunk = os.read(descriptor, 65536)
                if not chunk:
                    break
                if chunk != b' ' * len(chunk):
                    raise ValueError('unused prepared setup reservation contains unreceipted data')
            if snapshot(os.fstat(descriptor)) != snapshot(info):
                raise ValueError('unused prepared setup reservation changed')
            os.close(descriptor)
            return None
        validate_receipt_digest(receipt, receipt.get('sealed_bytes') if isinstance(receipt, dict) else None,
                                receipt.get('ciphertext_sha256') if isinstance(receipt, dict) else None)
        if info.st_size < receipt['sealed_bytes']:
            raise ValueError('prepared setup reservation is truncated')
        if os.read(descriptor, 8) != b'TILOMIG1':
            raise ValueError('prepared setup file is not a sealed migration package')
        os.lseek(descriptor, 0, os.SEEK_SET)
        return descriptor, info, receipt
    except BaseException:
        os.close(descriptor)
        raise


def snapshot(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def private_child(parent, name, uid, gid, private=True):
    try:
        os.mkdir(name, 0o700, dir_fd=parent)
        created = True
    except FileExistsError:
        created = False
    child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                    dir_fd=parent)
    try:
        if created:
            os.fchown(child, uid, gid)
            os.fchmod(child, 0o700)
            os.fsync(parent)
        facts = os.fstat(child)
        if facts.st_uid != uid or facts.st_mode & (0o077 if private else 0o022):
            raise ValueError('prepared setup directory is not private to its owner')
        return child
    except BaseException:
        os.close(child)
        raise


def read_ciphertext(parent, owner):
    fd = os.open(FILE, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                 dir_fd=parent)
    try:
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != owner
                or before.st_mode & 0o077 or not 48 <= before.st_size <= MAX_BYTES):
            raise ValueError('prepared setup seed is not a bounded private regular file')
        data = bytearray()
        while len(data) <= before.st_size:
            chunk = os.read(fd, min(65536, before.st_size + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if (len(data) != before.st_size or snapshot(os.fstat(fd)) != snapshot(before)
                or data[:8] != b'TILOMIG1'):
            raise ValueError('prepared setup seed changed or has invalid framing')
        return bytes(data)
    finally:
        os.close(fd)


def publish_ciphertext(parent, data, uid, gid, name=FILE):
    try:
        existing = (read_ciphertext(parent, uid) if name == FILE else
                    json.dumps(read_receipt(parent, uid), sort_keys=True, separators=(',', ':')).encode())
        if existing != data:
            raise ValueError('existing prepared setup differs; refusing replacement')
        return
    except FileNotFoundError:
        pass
    temporary = '.prepared-setup-' + secrets.token_hex(16)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                 | os.O_CLOEXEC, 0o600, dir_fd=parent)
    try:
        os.fchown(fd, uid, gid)
        os.fchmod(fd, 0o600)
        pending = memoryview(data)
        while pending:
            written = os.write(fd, pending)
            if not written:
                raise ValueError('prepared setup write stopped')
            pending = pending[written:]
        os.fsync(fd)
        os.link(temporary, name, src_dir_fd=parent, dst_dir_fd=parent,
                follow_symlinks=False)
        os.fsync(parent)
        actual = (read_ciphertext(parent, uid) if name == FILE else
                  json.dumps(read_receipt(parent, uid), sort_keys=True, separators=(',', ':')).encode())
        if actual != data:
            raise ValueError('published prepared setup failed byte verification')
    finally:
        os.close(fd)
        os.unlink(temporary, dir_fd=parent)


def carry(source_path, target_path, components, uid, gid, source_components=()):
    """Copy ciphertext through pinned, protected chains without replacing data."""
    source_fd = directory(source_path)
    destination = directory(target_path)
    chain = []
    try:
        for component in source_components:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
                            | os.O_CLOEXEC, dir_fd=source_fd)
            os.close(source_fd)
            source_fd = child
        facts = os.fstat(source_fd)
        if facts.st_uid != os.geteuid() or facts.st_mode & 0o022:
            raise ValueError('prepared setup source directory has unsafe ownership or mode')
        data = read_ciphertext(source_fd, os.geteuid())
        receipt = read_receipt(source_fd, os.geteuid())
        validate_receipt(receipt, data)
        facts = os.fstat(destination)
        if facts.st_uid != uid or facts.st_mode & 0o022:
            raise ValueError('prepared setup target root has unsafe ownership or mode')
        for index, component in enumerate(components):
            child = private_child(destination, component, uid, gid, index == len(components) - 1)
            facts = os.fstat(child)
            chain.append((os.dup(destination), component, facts.st_dev, facts.st_ino))
            os.close(destination)
            destination = child
        def validate():
            for parent, component, dev, ino in chain:
                current = os.stat(component, dir_fd=parent, follow_symlinks=False)
                if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (dev, ino):
                    raise ValueError('prepared setup target directory was replaced')
        validate()
        publish_ciphertext(destination, data, uid, gid)
        publish_ciphertext(destination, json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode(),
                           uid, gid, RECEIPT_FILE)
        validate()
        return hashlib.sha256(data).hexdigest()
    finally:
        os.close(source_fd)
        os.close(destination)
        for parent, _, _, _ in chain:
            os.close(parent)


def retain(source_path, target_var):
    return carry(source_path, target_var, ('lib', 'tilo', 'prepared-setup'),
                 os.geteuid(), os.getegid())


def protected_root_directory(fd):
    info = os.fstat(fd)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise ValueError('prepared binding directory is not root protected')
    return info


def binding_home_snapshot(info):
    return snapshot(info) + (info.st_uid, info.st_gid, info.st_mode)


def validate_first_use_origin(origin):
    if (not isinstance(origin, dict)
            or set(origin) != {'schema', 'identity_public_key', 'setup_binding', 'sealed_bytes', 'ciphertext_sha256'}
            or type(origin['schema']) is not int or origin['schema'] != 1):
        raise ValueError('prepared first-use origin has an invalid schema')
    person = origin['identity_public_key']
    setup = origin['setup_binding']
    digest = origin['ciphertext_sha256']
    if (not isinstance(person, list) or len(person) != 32 or not any(person)
            or any(type(value) is not int or not 0 <= value <= 255 for value in person)
            or not isinstance(setup, str) or not setup or len(setup.encode()) > 256
            or any(ord(value) < 32 or ord(value) == 127 for value in setup)
            or type(origin['sealed_bytes']) is not int
            or not 48 <= origin['sealed_bytes'] <= 33570816
            or not isinstance(digest, str) or len(digest) != 64
            or any(value not in '0123456789abcdef' for value in digest)):
        raise ValueError('prepared first-use origin identity or ciphertext differs')
    return origin


def retain_authenticated_binding(source_path, target_var, authenticated):
    """Persist public setup choices only after the canonical native importer.

    This root-only entry is called with that importer's bounded stdout. It
    carries no private identity and grants no source or application access.
    """
    if os.geteuid() != 0:
        raise ValueError('prepared first-use binding requires the root installer')
    preflight_authenticated(source_path, authenticated)
    person = authenticated.get('identity_public_key')
    setup = authenticated.get('setup_binding')
    if (not isinstance(person, list) or len(person) != 32 or not any(person)
            or any(type(value) is not int or not 0 <= value <= 255 for value in person)
            or not isinstance(setup, str) or not setup or len(setup.encode()) > 256
            or any(ord(value) < 32 for value in setup)):
        raise ValueError('authenticated prepared identity or setup binding differs')
    source = directory(source_path)
    target = directory(target_var)
    try:
        protected_root_directory(source)
        protected_root_directory(target)
        expected = read_receipt(source, 0)
        validate_receipt(expected, read_ciphertext(source, 0))
        for component in ('lib', 'tilo', 'prepared-setup'):
            child = private_child(target, component, 0, 0, component == 'prepared-setup')
            os.close(target)
            target = child
        if read_receipt(target, 0) != expected:
            raise ValueError('installed ciphertext expectation differs from authenticated source')
        validate_receipt(expected, read_ciphertext(target, 0))
        publish_home_json(target, 'first-use-origin.json', validate_first_use_origin({
            'schema': 1, 'identity_public_key': person, 'setup_binding': setup,
            'sealed_bytes': expected['sealed_bytes'],
            'ciphertext_sha256': expected['ciphertext_sha256'],
        }), 0, 0)
    finally:
        os.close(source)
        os.close(target)


def deliver_first_use_binding(account, delivered_digest):
    """Bind real NSS and the held delivered Home to the root install origin."""
    source = directory('/var/lib/tilo/prepared-setup')
    home = directory(account.pw_dir)
    system = directory('/var/lib/tilo')
    binding = None
    try:
        protected_root_directory(source)
        protected_root_directory(system)
        origin = validate_first_use_origin(json_file(source, 'first-use-origin.json', 8192, 0, True))
        if origin['ciphertext_sha256'] != delivered_digest:
            raise ValueError('prepared delivery differs from its root-authenticated origin')
        expected = read_receipt(source, 0)
        if (origin['sealed_bytes'] != expected['sealed_bytes']
                or origin['ciphertext_sha256'] != expected['ciphertext_sha256']):
            raise ValueError('root prepared binding no longer matches ciphertext')
        info = os.fstat(home)
        if info.st_uid != account.pw_uid or info.st_gid != account.pw_gid or info.st_mode & 0o022:
            raise ValueError('prepared Home is not controlled by the real target account')
        if os.fstat(system).st_uid != 0 or os.fstat(system).st_mode & 0o022:
            raise ValueError('prepared binding root is not protected')
        binding = private_child(system, 'prepared-first-use', 0, 0, False)
        os.fchmod(binding, 0o755)
        public = dict(origin, uid=account.pw_uid, gid=account.pw_gid,
                      home=account.pw_dir, home_device=info.st_dev, home_inode=info.st_ino)
        name = str(account.pw_uid) + '.json'
        try:
            if json_file(binding, name, 8192, 0) != public:
                raise ValueError('existing first-use binding differs; refusing replacement')
        except FileNotFoundError:
            publish_home_json(binding, name, public, 0, 0)
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=binding)
            try:
                os.fchmod(fd, 0o644)
                os.fsync(fd)
            finally:
                os.close(fd)
        os.fsync(binding)
        os.fsync(system)
        if binding_home_snapshot(os.fstat(home)) != binding_home_snapshot(info):
            raise ValueError('prepared Home changed during binding publication')
    finally:
        for fd in (binding, source, home, system):
            if fd is not None:
                os.close(fd)


def deliver_user(username):
    import pwd
    if os.geteuid() != 0:
        raise ValueError('installed setup delivery requires the firstboot root service')
    account = pwd.getpwnam(username)
    digest = carry('/var/lib/tilo/prepared-setup', account.pw_dir,
                   ('.local', 'state', 'tilo', 'prepared-setup'), account.pw_uid, account.pw_gid)
    deliver_first_use_binding(account, digest)
    return digest


def extract(roots, output):
    """Roots are existing filesystem grants, not disk names or mount requests."""
    output_fd = directory(output)
    candidates = {}
    temporary = None
    try:
        info = os.fstat(output_fd)
        if info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise ValueError('installer output directory is not protected from other writers')
        for root in roots:
            candidate = source(root)
            if candidate is None:
                continue
            descriptor, info, _ = candidate
            key = (info.st_dev, info.st_ino)
            if key in candidates:
                os.close(descriptor)
                if candidate[2] != candidates[key][2]:
                    raise ValueError('aliased media artifacts have conflicting expectations')
            else:
                candidates[key] = candidate
        if not candidates:
            return None
        if len(candidates) != 1:
            raise ValueError('more than one mounted medium contains encrypted prepared setup')
        descriptor, before, receipt = next(iter(candidates.values()))
        temporary = '.prepared-setup-' + secrets.token_hex(16)
        destination = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                              | os.O_CLOEXEC, 0o600, dir_fd=output_fd)
        digest = hashlib.sha256()
        try:
            total = 0
            while True:
                chunk = os.read(descriptor, 65536)
                if not chunk:
                    break
                total += len(chunk)
                if total > before.st_size or total > MAX_BYTES:
                    raise ValueError('encrypted setup file changed during delivery')
                remaining = max(0, receipt['sealed_bytes'] - (total - len(chunk)))
                sealed = chunk[:remaining]
                padding = chunk[len(sealed):]
                if padding != b' ' * len(padding):
                    raise ValueError('prepared setup reservation contains nonblank trailing data')
                digest.update(sealed)
                pending = memoryview(sealed)
                while pending:
                    written = os.write(destination, pending)
                    if not written:
                        raise ValueError('encrypted setup output stopped writing')
                    pending = pending[written:]
            if total != before.st_size or snapshot(os.fstat(descriptor)) != snapshot(before):
                raise ValueError('encrypted setup file changed during delivery')
            validate_receipt_digest(receipt, receipt['sealed_bytes'], digest.hexdigest())
            os.fsync(destination)
        finally:
            os.close(destination)
        # Hard linking is an atomic no-replacement publication in root-private /run.
        os.link(temporary, FILE, src_dir_fd=output_fd, dst_dir_fd=output_fd,
                follow_symlinks=False)
        os.unlink(temporary, dir_fd=output_fd)
        temporary = None
        os.fsync(output_fd)
        publish_ciphertext(output_fd, json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode(),
                           os.geteuid(), os.getegid(), RECEIPT_FILE)
        return {'sealed_bytes': receipt['sealed_bytes'], 'ciphertext_sha256': digest.hexdigest()}
    finally:
        if temporary is not None:
            os.unlink(temporary, dir_fd=output_fd)
        for descriptor, _, _ in candidates.values():
            os.close(descriptor)
        os.close(output_fd)


def mounted_roots(devices):
    """Read filesystem mounts belonging to existing canonical boot candidates."""
    roots = set()
    for device in devices:
        info = os.stat(device, follow_symlinks=False)
        if not stat.S_ISBLK(info.st_mode):
            raise ValueError('prepared setup discovery requires a real medium block device')
        result = subprocess.run(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output',
                                 'NAME,TYPE,MOUNTPOINTS', device], check=True,
                                capture_output=True, timeout=10,
                                env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
        if len(result.stdout) > 4 * 1024 * 1024:
            raise ValueError('media filesystem discovery exceeded its bound')
        devices_rows = json.loads(result.stdout).get('blockdevices', [])
        def visit(row, depth=0):
            if depth > 16:
                raise ValueError('media filesystem topology exceeded its bound')
            for point in row.get('mountpoints') or []:
                if point and Path(point).is_absolute():
                    roots.add(point)
            for child in row.get('children') or []:
                visit(child, depth + 1)
        for row in devices_rows:
            visit(row)
    return sorted(roots)


def device_inventory(device):
    info = os.stat(device, follow_symlinks=False)
    if not stat.S_ISBLK(info.st_mode) or info.st_uid != 0:
        raise ValueError('boot medium is not a root-owned block device')
    result = subprocess.run(['/usr/bin/lsblk', '--json', '--bytes', '--paths', '--output',
                             'NAME,TYPE,FSTYPE,PARTTYPE,PARTLABEL,PARTUUID,UUID,MAJ:MIN,MOUNTPOINTS', device],
                            check=True, capture_output=True, timeout=10,
                            env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
    if len(result.stdout) > 4 * 1024 * 1024:
        raise ValueError('boot medium inventory exceeds bound')
    rows = json.loads(result.stdout).get('blockdevices')
    if not isinstance(rows, list) or len(rows) != 1 or rows[0].get('name') != device:
        raise ValueError('boot medium inventory changed or is ambiguous')
    after = os.stat(device, follow_symlinks=False)
    if (after.st_dev, after.st_ino, after.st_rdev) != (info.st_dev, info.st_ino, info.st_rdev):
        raise ValueError('boot medium changed during discovery')
    return rows[0]


@contextmanager
def media_roots(devices):
    """Retain existing grants and temporarily mount only a unique FAT EFI child."""
    roots = set(mounted_roots(devices))
    leases = []
    try:
        for device in dict.fromkeys(devices):
            row = device_inventory(device)
            if row.get('type') != 'disk':
                continue
            efi = [part for part in row.get('children') or []
                   if str(part.get('parttype') or '').lower() == 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b']
            home_parts = [part for part in row.get('children') or []
                          if str(part.get('parttype') or '').lower() == HOME_PART_TYPE]
            if len(efi) > 1 or len(home_parts) > 1:
                raise ValueError('boot medium has ambiguous EFI or Home filesystems')
            for part in efi + home_parts:
                if any(part.get('mountpoints') or []):
                    continue
                name = part.get('name')
                home_partition = part in home_parts
                expected_fs = 'ext4' if home_partition else 'vfat'
                if (part.get('type') != 'part' or part.get('fstype') != expected_fs
                        or (home_partition and part.get('partlabel') != 'Tilo migration')
                        or part.get('children') or not part.get('uuid') or not part.get('partuuid')
                        or not isinstance(name, str) or not name.startswith('/dev/')
                        or Path(name).parent != Path('/dev')):
                    raise ValueError('boot customization filesystem identity is unavailable')
                before = os.stat(name, follow_symlinks=False)
                number = f'{os.major(before.st_rdev)}:{os.minor(before.st_rdev)}'
                if not stat.S_ISBLK(before.st_mode) or before.st_uid != 0 or number != part.get('maj:min'):
                    raise ValueError('boot EFI kernel device changed')
                root = Path(tempfile.mkdtemp(prefix='tilo-setup-media-'))
                original_root = os.stat(root, follow_symlinks=False)
                # Never recursively clean a mountpoint, including after mount timeout.
                leases.append((root, original_root.st_dev, original_root.st_ino, before.st_rdev))
                options = 'ro,nodev,nosuid,noexec,noload' if home_partition else 'ro,nodev,nosuid,noexec,umask=0077'
                subprocess.run(['/usr/bin/mount', '-t', expected_fs, '-o', options,
                                name, str(root)], check=True, capture_output=True, timeout=15,
                               env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
                after = os.stat(name, follow_symlinks=False)
                current = device_inventory(device)
                children = [item for item in current.get('children') or [] if item.get('name') == name]
                if (len(children) != 1 or children[0].get('uuid') != part['uuid']
                        or children[0].get('partuuid') != part['partuuid']
                        or (after.st_dev, after.st_ino, after.st_rdev) != (before.st_dev, before.st_ino, before.st_rdev)
                        or os.stat(root, follow_symlinks=False).st_dev != before.st_rdev
                        or not os.statvfs(root).f_flag & os.ST_RDONLY):
                    raise ValueError('readonly EFI grant changed or was not established')
                roots.add(str(root))
        yield sorted(roots)
    finally:
        cleanup_error = None
        for root, original_device, original_inode, device_number in reversed(leases):
            try:
                info = os.stat(root, follow_symlinks=False)
                if info.st_dev == device_number:
                    subprocess.run(['/usr/bin/umount', str(root)], check=True, capture_output=True, timeout=15,
                                   env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
                    info = os.stat(root, follow_symlinks=False)
                if (info.st_dev, info.st_ino) != (original_device, original_inode):
                    raise ValueError('readonly media lease changed before release')
                # rmdir refuses nonempty directories and never touches mounted data.
                os.rmdir(root)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                cleanup_error = cleanup_error or error
        if cleanup_error is not None:
            raise cleanup_error


# Large Home custody uses bounded metadata and streaming ciphertext only.
HOME_SELECTION = 'home-selection.json'
HOME_LOCATOR = 'portable-home.json'
HOME_EXPECTATION = 'expectation.json'
HOME_PART_TYPE = '7c4f89e9-0bd4-45ac-8ea6-319ab6795a91'
HOME_RESERVE = 16 * 1024 * 1024
APPLICATIONS_SELECTION = 'applications-selection.json'
APPLICATIONS_LOCATOR = 'portable-applications.json'
BROWSER_SELECTION = 'browser-selection.json'
BROWSER_LOCATOR = 'portable-browser.json'
PAYLOAD_KINDS = ('home', 'applications', 'browser')
BROWSER_SLOT_BYTES = 130 * 1024 * 1024
BROWSER_RECEIPT = 'browser-receipt.json'
BROWSER_PACKAGE = 'browser.sealed'


def payload_names(kind):
    if kind == 'home':
        return HOME_SELECTION, HOME_LOCATOR, 'prepared-home', 'home', 'home'
    if kind == 'applications':
        return APPLICATIONS_SELECTION, APPLICATIONS_LOCATOR, 'prepared-applications', 'flatpak_applications', 'applications'
    if kind == 'browser':
        return BROWSER_SELECTION, BROWSER_LOCATOR, 'prepared-browser', 'browser_profiles', 'browser'
    raise ValueError('unsupported portable payload kind')


def home_expectation(value, kind='home'):
    _, _, _, purpose, prefix = payload_names(kind)
    keys = {'schema', 'operation', 'purpose', 'filename', 'setup_binding', 'sealed_bytes',
            'plaintext_bytes', 'entries', 'ciphertext_sha256', 'limits'}
    logical_field = {'home': 'runtime_sockets', 'applications': 'manifest_sha256', 'browser': 'plan_sha256'}[kind]
    keys.add(logical_field)
    if not isinstance(value, dict) or set(value) != keys:
        raise ValueError('portable Home receipt has unsupported fields')
    operation = value['operation']
    limits = value['limits']
    if (type(value['schema']) is not int or value['schema'] != 1 or value['purpose'] != purpose
            or not isinstance(operation, str) or not 1 <= len(operation) <= 96
            or any(not (c.isascii() and (c.isalnum() or c in '-_')) for c in operation)
            or value['filename'] != prefix + '-' + operation + '.sealed'
            or not isinstance(value['setup_binding'], str) or not 1 <= len(value['setup_binding']) <= 256
            or any(ord(c) < 32 or ord(c) == 127 for c in value['setup_binding'])
            or not isinstance(limits, dict) or set(limits) != {'plaintext_bytes', 'entries', 'metadata_bytes'}):
        raise ValueError('portable Home receipt binding is invalid')
    for field in ('sealed_bytes', 'plaintext_bytes', 'entries') + (('runtime_sockets',) if kind == 'home' else ()):
        if type(value[field]) is not int or not 0 <= value[field] <= (1 << 63) - 1:
            raise ValueError('portable Home receipt length is invalid')
    for field in limits:
        if type(limits[field]) is not int or not 1 <= limits[field] <= (1 << 63) - 1:
            raise ValueError('portable Home receipt limits are invalid')
    bound = limits['plaintext_bytes'] + 2048 + ((limits['plaintext_bytes'] + 1048575) // 1048576) * 256
    digest = value['ciphertext_sha256']
    if (not 48 <= value['sealed_bytes'] <= bound or not 1 <= value['plaintext_bytes'] <= limits['plaintext_bytes']
            or not 1 <= value['entries'] <= limits['entries'] <= 1000000
            or not 0 <= value.get('runtime_sockets', 0) <= limits['entries']
            or limits['metadata_bytes'] > 512 * 1024 * 1024
            or not isinstance(digest, list) or len(digest) != 32
            or any(type(n) is not int or not 0 <= n <= 255 for n in digest)):
        raise ValueError('portable Home receipt exceeds bounds')
    if kind in ('applications', 'browser'):
        import uuid
        if str(uuid.UUID(operation)) != operation:
            raise ValueError('portable application operation is invalid')
        logical_digest = value[logical_field]
        if (not isinstance(logical_digest, list) or len(logical_digest) != 32
                or any(type(n) is not int or not 0 <= n <= 255 for n in logical_digest)):
            raise ValueError('portable application manifest digest is invalid')
    if kind == 'browser' and (limits['plaintext_bytes'] > 129 * 1024 * 1024
                              or limits['entries'] > 128 or limits['metadata_bytes'] > 1024 * 1024):
        raise ValueError('portable browser receipt exceeds fixed bounds')
    return value


def json_file(parent, name, bound, owner=None, private=False, padded=False, reserved=False):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or before.st_size > bound
                or (reserved and before.st_size != 4096)
                or (owner is not None and before.st_uid != owner)
                or (private and before.st_mode & 0o077)):
            raise ValueError('Home metadata is not a bounded protected regular file')
        data = bytearray()
        while len(data) <= bound:
            chunk = os.read(fd, min(65536, bound + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > bound or snapshot(os.fstat(fd)) != snapshot(before):
            raise ValueError('Home metadata changed during read')
        if padded:
            if len(data) == 4096 and data == b' ' * 4096:
                return UNUSED_STAGING_SLOT
            text = data.decode('utf-8')
            value, end = json.JSONDecoder().raw_decode(text)
            if any(c != ' ' for c in text[end:]):
                raise ValueError('browser metadata padding is invalid')
            return value
        return json.loads(data)
    finally:
        os.close(fd)


def publish_home_json(parent, name, value, uid, gid):
    data = json.dumps(value, sort_keys=True, separators=(',', ':')).encode()
    if len(data) > 32768:
        raise ValueError('Home transport metadata exceeds bounds')
    try:
        if json_file(parent, name, 32768, uid, True) != value:
            raise ValueError('existing Home metadata conflicts; refusing replacement')
        return
    except FileNotFoundError:
        pass
    temporary = '.home-metadata-' + secrets.token_hex(16)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=parent)
    try:
        os.fchown(fd, uid, gid)
        pending = memoryview(data)
        while pending:
            count = os.write(fd, pending)
            if not count:
                raise ValueError('Home metadata write stopped')
            pending = pending[count:]
        os.fsync(fd)
        os.link(temporary, name, src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
        os.fsync(parent)
    finally:
        os.close(fd)
        os.unlink(temporary, dir_fd=parent)


def home_package(parent, expected, owner=None):
    fixed = False
    try:
        fd = os.open(expected['filename'], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK, dir_fd=parent)
    except FileNotFoundError:
        if owner is not None or expected['purpose'] != 'browser_profiles':
            raise
        child = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
        try:
            fd = os.open(BROWSER_PACKAGE, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK, dir_fd=child)
        finally:
            os.close(child)
        fixed = True
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_size != (BROWSER_SLOT_BYTES if fixed else expected['sealed_bytes'])
                or (owner is not None and (info.st_uid != owner or info.st_mode & 0o077))
                or os.read(fd, 8) != b'TILOMIG1'):
            raise ValueError('Home ciphertext has invalid size, ownership or framing')
        os.lseek(fd, 0, os.SEEK_SET)
        return fd, info
    except BaseException:
        os.close(fd)
        raise


def stream_home(source_fd, before, expected, destination=None):
    os.lseek(source_fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    total = 0
    fixed = expected['purpose'] == 'browser_profiles' and before.st_size == BROWSER_SLOT_BYTES
    remaining = expected['sealed_bytes']
    while remaining:
        chunk = os.read(source_fd, min(65536, remaining))
        if not chunk:
            break
        total += len(chunk)
        remaining -= len(chunk)
        digest.update(chunk)
        if destination is not None:
            pending = memoryview(chunk)
            while pending:
                count = os.write(destination, pending)
                if not count:
                    raise ValueError('Home ciphertext write stopped')
                pending = pending[count:]
    if fixed:
        tail = before.st_size - expected['sealed_bytes']
        while tail:
            chunk = os.read(source_fd, min(65536, tail))
            if not chunk or chunk != b' ' * len(chunk):
                raise ValueError('browser ciphertext reservation padding is invalid')
            tail -= len(chunk)
    if os.read(source_fd, 1):
        raise ValueError('Home ciphertext grew while streaming')
    if (total != expected['sealed_bytes'] or snapshot(os.fstat(source_fd)) != snapshot(before)
            or digest.digest() != bytes(expected['ciphertext_sha256'])):
        raise ValueError('Home ciphertext changed or differs from its expected digest')


def root_binding(root, devices):
    fd = directory(root)
    try:
        facts = os.fstat(fd)
        if not devices:
            return {'kind': 'directory', 'path': str(Path(root)), 'device': facts.st_dev, 'inode': facts.st_ino}
        for device in devices:
            row = device_inventory(device)
            disk = os.stat(device, follow_symlinks=False)
            for part in row.get('children') or []:
                name = part.get('name')
                if not isinstance(name, str) or not name.startswith('/dev/') or Path(name).parent != Path('/dev'):
                    continue
                info = os.stat(name, follow_symlinks=False)
                if stat.S_ISBLK(info.st_mode) and info.st_rdev == facts.st_dev:
                    part_type = str(part.get('parttype') or '').lower()
                    valid = ((part_type == HOME_PART_TYPE and part.get('fstype') == 'ext4' and part.get('partlabel') == 'Tilo migration')
                             or (part_type == 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b' and part.get('fstype') == 'vfat'))
                    if not valid or not part.get('uuid') or not part.get('partuuid'):
                        raise ValueError('Home source is not a uniquely identified customization filesystem')
                    if not os.fstatvfs(fd).f_flag & os.ST_RDONLY:
                        raise ValueError('Home source filesystem grant must be genuinely read-only')
                    return {'kind': 'filesystem', 'disk': device, 'disk_identity': [disk.st_dev, disk.st_ino, disk.st_rdev],
                            'partition': name, 'partition_identity': [info.st_dev, info.st_ino, info.st_rdev],
                            'uuid': part.get('uuid'), 'partuuid': part.get('partuuid'), 'fstype': part.get('fstype')}
        raise ValueError('Home source root does not belong to the selected boot medium')
    finally:
        os.close(fd)


def validate_unused_browser(parent):
    child = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
    try:
        if json_file(child, BROWSER_RECEIPT, 4096, padded=True, reserved=True) is not UNUSED_STAGING_SLOT:
            raise ValueError('unused browser reservation has a selected receipt')
        fd = os.open(BROWSER_PACKAGE, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=child)
        try:
            before = os.fstat(fd)
            if not stat.S_ISREG(before.st_mode) or before.st_size != BROWSER_SLOT_BYTES:
                raise ValueError('unused browser reservation has invalid size')
            while True:
                chunk = os.read(fd, 65536)
                if not chunk:
                    break
                if chunk != b' ' * len(chunk):
                    raise ValueError('unused browser reservation contains unselected data')
            if snapshot(os.fstat(fd)) != snapshot(before):
                raise ValueError('unused browser reservation changed')
        finally:
            os.close(fd)
    finally:
        os.close(child)


def stage_home(roots, output, prepared_receipt, devices=(), kind='home'):
    selection_name, locator_name, _, _, _ = payload_names(kind)
    selection_field = kind
    # Repeated aliases of one held directory do not invent another medium.
    unique, identities = [], set()
    for root in roots:
        fd = directory(root)
        try:
            facts = os.fstat(fd)
            identity = (facts.st_dev, facts.st_ino)
            if identity not in identities:
                identities.add(identity)
                unique.append(root)
        finally:
            os.close(fd)
    roots = unique
    selections = []
    for root in roots:
        fd = directory(root)
        try:
            try:
                child = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            except FileNotFoundError:
                continue
            try:
                try:
                    value = json_file(child, selection_name, 4096 if kind == 'browser' else 18432, padded=kind == 'browser')
                except FileNotFoundError:
                    if kind == 'browser':
                        for slot in (BROWSER_RECEIPT, BROWSER_PACKAGE):
                            try:
                                os.stat(slot, dir_fd=child, follow_symlinks=False)
                            except FileNotFoundError:
                                continue
                            raise ValueError('browser reservation is missing its selection')
                    continue
            finally:
                os.close(child)
            if value is UNUSED_STAGING_SLOT:
                validate_unused_browser(fd)
                continue
            if (not isinstance(value, dict) or set(value) != {'schema', 'prepared_setup', selection_field}
                    or type(value['schema']) is not int or value['schema'] != 1
                    or value['prepared_setup'] != prepared_receipt):
                raise ValueError('Home selection does not match the delivered prepared setup')
            selections.append((root_binding(root, devices), home_expectation(value[selection_field], kind)))
        finally:
            os.close(fd)
    if not selections:
        return None
    if len(selections) != 1:
        raise ValueError('more than one medium declares selected Home data')
    selection_binding, expected = selections[0]
    found = []
    for root in roots:
        fd = directory(root)
        try:
            try:
                receipt = json_file(fd, expected['operation'] + '.receipt.json', 16384)
            except FileNotFoundError:
                if kind != 'browser':
                    continue
                try:
                    child = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
                except FileNotFoundError:
                    continue
                try:
                    try:
                        receipt = json_file(child, BROWSER_RECEIPT, 4096, padded=True, reserved=True)
                    except FileNotFoundError:
                        continue
                    if receipt is UNUSED_STAGING_SLOT:
                        continue
                    try:
                        os.stat(expected['filename'], dir_fd=fd, follow_symlinks=False)
                    except FileNotFoundError:
                        pass
                    else:
                        raise ValueError('browser fixed receipt conflicts with a dynamic package')
                finally:
                    os.close(child)
            if receipt != expected:
                raise ValueError('Home media receipt differs from the selected setup')
            binding = root_binding(root, devices)
            if devices and binding.get('disk_identity') != selection_binding.get('disk_identity'):
                raise ValueError('Home data is outside the selected boot medium')
            package, info = home_package(fd, expected)
            try:
                # Early boot reads no giant archive: only bounded receipt and inode facts.
                found.append({'schema': 1, 'binding': binding, 'expectation': expected,
                              'source_identity': list(snapshot(info)), 'prepared_setup': prepared_receipt})
            finally:
                os.close(package)
        finally:
            os.close(fd)
    if len(found) != 1:
        raise ValueError('selected Home ciphertext is missing or ambiguous')
    output_fd = directory(output)
    try:
        publish_home_json(output_fd, locator_name, found[0], os.geteuid(), os.getegid())
    finally:
        os.close(output_fd)
    return found[0]


@contextmanager
def home_root(binding):
    if not isinstance(binding, dict):
        raise ValueError('Home source binding has unsupported fields')
    if binding.get('kind') == 'directory' and set(binding) == {'kind', 'path', 'device', 'inode'}:
        fd = directory(binding['path'])
        try:
            facts = os.fstat(fd)
            if (facts.st_dev, facts.st_ino) != (binding['device'], binding['inode']):
                raise ValueError('explicit Home source grant was replaced')
            yield fd
        finally:
            os.close(fd)
        return
    fields = {'kind', 'disk', 'disk_identity', 'partition', 'partition_identity', 'uuid', 'partuuid', 'fstype'}
    if not isinstance(binding, dict) or set(binding) != fields or binding['kind'] != 'filesystem':
        raise ValueError('Home source binding has unsupported fields')
    disk = os.stat(binding['disk'], follow_symlinks=False)
    part = os.stat(binding['partition'], follow_symlinks=False)
    if (not stat.S_ISBLK(disk.st_mode) or not stat.S_ISBLK(part.st_mode)
            or [disk.st_dev, disk.st_ino, disk.st_rdev] != binding['disk_identity']
            or [part.st_dev, part.st_ino, part.st_rdev] != binding['partition_identity']):
        raise ValueError('Home source kernel device changed')
    current = device_inventory(binding['disk'])
    matches = [item for item in current.get('children') or [] if item.get('name') == binding['partition']]
    if (len(matches) != 1 or any(matches[0].get(key) != binding[key] for key in ('uuid', 'partuuid', 'fstype'))):
        raise ValueError('Home source partition identity changed')
    with media_roots([binding['disk']]) as roots:
        candidates = []
        for root in roots:
            fd = directory(root)
            if os.fstat(fd).st_dev == part.st_rdev:
                candidates.append(fd)
            else:
                os.close(fd)
        try:
            if len(candidates) != 1 or not os.fstatvfs(candidates[0]).f_flag & os.ST_RDONLY:
                raise ValueError('Home source lacks a unique kernel read-only grant')
            yield candidates[0]
        finally:
            for fd in candidates:
                os.close(fd)


def staged_home(source, kind='home'):
    _, locator_name, _, _, _ = payload_names(kind)
    fd = directory(source)
    try:
        facts = os.fstat(fd)
        if facts.st_uid != os.geteuid() or facts.st_mode & 0o022:
            raise ValueError('Home locator directory has unsafe custody')
        value = json_file(fd, locator_name, 32768, os.geteuid(), True)
        if (not isinstance(value, dict) or set(value) != {'schema', 'binding', 'expectation', 'source_identity', 'prepared_setup'}
                or type(value['schema']) is not int or value['schema'] != 1
                or not isinstance(value['source_identity'], list)
                or len(value['source_identity']) != 5
                or any(type(n) is not int or n < 0 for n in value['source_identity'])):
            raise ValueError('retained Home locator is invalid')
        home_expectation(value['expectation'], kind)
        if read_receipt(fd, os.geteuid()) != value['prepared_setup']:
            raise ValueError('Home locator belongs to another prepared setup')
        if isinstance(value['binding'], dict) and value['binding'].get('kind') == 'completed_generation':
            validate_completed_source(source, value['binding'])
        return value
    finally:
        os.close(fd)


def optional_staged_home(source, kind='home'):
    _, locator_name, _, _, _ = payload_names(kind)
    try:
        parent = directory(source)
    except FileNotFoundError:
        return None
    try:
        try:
            os.stat(locator_name, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            return None
    finally:
        os.close(parent)
    # A present locator with a missing/changed receipt is an error, not absence.
    return staged_home(source, kind)


def reported_btrfs_resize(authenticated):
    """Validate reported effective geometry without computing or applying it."""
    import uuid
    resize = authenticated.get('planned_btrfs_resize')
    if resize is None:
        return
    keys = {'schema', 'original_plan_sha256', 'reviewed_plan_sha256', 'reviewed_plan',
            'filesystem_uuid', 'partition_guid', 'device_id', 'from_bytes', 'to_bytes',
            'new_last_lba'}
    if (not isinstance(resize, dict) or set(resize) != keys
            or type(resize['schema']) is not int or resize['schema'] != 1
            or type(resize['device_id']) is not int or resize['device_id'] != 1):
        raise ValueError('reported resize schema differs')
    for key in ('original_plan_sha256', 'reviewed_plan_sha256'):
        digest = resize[key]
        if (not isinstance(digest, list) or len(digest) != 32
                or any(type(value) is not int or not 0 <= value <= 255 for value in digest)
                or not any(digest)):
            raise ValueError('reported resize digest is invalid')
    for key in ('filesystem_uuid', 'partition_guid'):
        value = resize[key]
        if not isinstance(value, str) or len(value) != 36:
            raise ValueError('reported resize identity is invalid')
        parsed = uuid.UUID(value)
        if parsed.int == 0 or str(parsed) != value:
            raise ValueError('reported resize identity is not canonical')
    review = resize['reviewed_plan']
    if (not isinstance(review, dict)
            or len(json.dumps(review, separators=(',', ':'), ensure_ascii=False).encode()) > 16 * 1024):
        raise ValueError('reported resize review exceeds bounds')
    for key in ('from_bytes', 'to_bytes', 'new_last_lba'):
        if type(resize[key]) is not int or not 0 <= resize[key] < (1 << 64):
            raise ValueError('reported resize number is invalid')
    plan = authenticated.get('retained_home')
    volume = plan.get('home', {}).get('volume') if isinstance(plan, dict) and isinstance(plan.get('home'), dict) else None
    if not isinstance(volume, dict):
        raise ValueError('reported resize lacks effective retained source')
    first, last, sector = (volume.get(key) for key in ('first_lba', 'last_lba', 'sector_bytes'))
    if (any(type(value) is not int or not 0 <= value < (1 << 64) for value in (first, last, sector))
            or not 512 <= sector <= 65536 or sector & (sector - 1)
            or last < first or last != resize['new_last_lba']
            or not 0 < resize['to_bytes'] < resize['from_bytes']
            or resize['to_bytes'] % (1 << 20)
            or (last - first + 1) * sector != resize['to_bytes']
            or volume.get('filesystem') != 'btrfs'
            or not isinstance(volume.get('filesystem_uuid'), str)
            or volume['filesystem_uuid'].lower() != resize['filesystem_uuid']
            or not isinstance(volume.get('partition_guid'), str)
            or volume['partition_guid'].lower() != resize['partition_guid']):
        raise ValueError('reported resize differs from effective retained source')


def computer_apps_scope(authenticated):
    """Check descriptive coexistence lineage; never open or grant a source."""
    import uuid
    import unicodedata
    scope = authenticated.get('computer_apps_scope')
    required = (authenticated.get('retained_home') is not None
                and authenticated.get('completed_applications_only', False) is True)
    if (scope is not None) != required:
        raise ValueError('Computer applications scope is missing or unexpected')
    if not required:
        return False
    keys = {'schema', 'source_operation', 'applications_operation', 'staging_operation',
            'staging_plan_sha256', 'setup_binding', 'identity_public_key'}
    if not isinstance(scope, dict) or set(scope) != keys:
        raise ValueError('Computer applications scope fields differ')
    if type(scope['schema']) is not int or scope['schema'] != 1:
        raise ValueError('Computer applications scope schema differs')
    for key in ('source_operation', 'applications_operation', 'staging_operation'):
        value = scope[key]
        if not isinstance(value, str) or len(value) != 36:
            raise ValueError('Computer applications operation is invalid')
        parsed = uuid.UUID(value)
        if parsed.int == 0 or str(parsed) != value:
            raise ValueError('Computer applications operation is not canonical')
    digest = scope['staging_plan_sha256']
    if (not isinstance(digest, str) or len(digest) != 64
            or any(c not in '0123456789abcdef' for c in digest)):
        raise ValueError('Computer applications plan digest is invalid')
    person = scope['identity_public_key']
    binding = scope['setup_binding']
    if (not isinstance(person, list) or len(person) != 32
            or any(type(value) is not int or not 0 <= value <= 255 for value in person)
            or person != authenticated.get('identity_public_key')
            or not isinstance(binding, str) or not 1 <= len(binding.encode('utf-8')) <= 256
            or any(unicodedata.category(c) == 'Cc' for c in binding)
            or binding != authenticated.get('setup_binding')):
        raise ValueError('Computer applications person or setup differs')
    plan = authenticated.get('retained_home')
    applications = authenticated.get('completed_applications')
    if (not isinstance(plan, dict) or not isinstance(applications, dict)
            or plan.get('home_policy') != 'access-read-write'
            or not isinstance(plan.get('categories'), dict)
            or plan['categories'].get('home') is not True
            or plan.get('operation_id') != scope['source_operation']
            or applications.get('operation') != scope['applications_operation']
            or authenticated.get('completed_home') is not None):
        raise ValueError('Computer applications source or completed output differs')
    return True



def pending_computer_applications_scope(authenticated):
    """Closed routing metadata only; the root installed record admits the copy."""
    import uuid
    import unicodedata
    scope = authenticated.get('pending_computer_applications_scope')
    if scope is None:
        return None
    keys = {'schema', 'source_operation', 'applications_operation', 'staging_operation',
            'staging_plan_sha256', 'setup_binding', 'identity_public_key'}
    if not isinstance(scope, dict) or set(scope) != keys or type(scope['schema']) is not int or scope['schema'] != 1:
        raise ValueError('pending Computer applications fields differ')
    for key in ('source_operation', 'applications_operation', 'staging_operation'):
        value = scope[key]
        if not isinstance(value, str) or len(value) != 36:
            raise ValueError('pending Computer operation is invalid')
        parsed = uuid.UUID(value)
        if parsed.int == 0 or str(parsed) != value:
            raise ValueError('pending Computer operation is not canonical')
    if len({scope[key] for key in ('source_operation', 'applications_operation', 'staging_operation')}) != 3:
        raise ValueError('pending Computer operations overlap')
    digest = scope['staging_plan_sha256']
    person = scope['identity_public_key']
    binding = scope['setup_binding']
    plan = authenticated.get('retained_home')
    if (not isinstance(digest, str) or len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest)
            or not isinstance(person, list) or len(person) != 32
            or any(type(v) is not int or not 0 <= v <= 255 for v in person)
            or person != authenticated.get('identity_public_key')
            or not isinstance(binding, str) or not 1 <= len(binding.encode('utf-8')) <= 256
            or any(unicodedata.category(c) == 'Cc' for c in binding)
            or binding != authenticated.get('setup_binding')
            or not isinstance(plan, dict) or plan.get('home_policy') != 'access-read-write'
            or plan.get('operation_id') != scope['source_operation']
            or not isinstance(plan.get('categories'), dict)
            or plan['categories'].get('home') is not True or plan['categories'].get('flatpak') is not False
            or authenticated.get('completed_applications_only', False) is not False
            or any(authenticated.get(key) is not None for key in
                   ('completed_home', 'completed_applications', 'completed_setup', 'computer_apps_scope'))):
        raise ValueError('pending Computer source or purpose differs')
    return scope


def preflight_authenticated(source, authenticated):
    """Compare transported Home with native password-authenticated declaration.

    Public JSON supplies no disk or directory access: this checks only the
    already root-owned run namespace and its canonical held locator/receipt.
    """
    if (not isinstance(authenticated, dict) or type(authenticated.get('schema')) is not int
            or authenticated.get('schema') != 1):
        raise ValueError('authenticated setup receipt differs')
    if len(json.dumps(authenticated, separators=(',', ':'), ensure_ascii=False).encode()) > 80 * 1024:
        raise ValueError('authenticated setup receipt exceeds bounds')
    expected = authenticated.get('completed_home')
    applications = authenticated.get('completed_applications')
    apps_only = authenticated.get('completed_applications_only', False)
    if not isinstance(apps_only, bool):
        raise ValueError('completed applications purpose is malformed')
    shared_apps = computer_apps_scope(authenticated)
    pending_computer_applications_scope(authenticated)
    reported_btrfs_resize(authenticated)
    if apps_only:
        if expected is not None or applications is None:
            raise ValueError('completed applications purpose differs')
    elif expected is None:
        if applications is not None or authenticated.get('completed_setup') is not None:
            raise ValueError('completed applications lack authenticated generation')
        return
    if expected is not None:
        expected = home_expectation(expected)
    setup = authenticated.get('completed_setup')
    parent = directory(source)
    try:
        facts = os.fstat(parent)
        if facts.st_uid != os.geteuid() or facts.st_mode & 0o022:
            raise ValueError('completed setup run directory is not protected')
        data = read_ciphertext(parent, os.geteuid())
        validate_receipt_digest(setup, len(data), hashlib.sha256(data).hexdigest())
        if read_receipt(parent, os.geteuid()) != setup:
            raise ValueError('completed setup differs from authenticated generation')
    finally:
        os.close(parent)
    if ((authenticated.get('retained_home') is not None and not shared_apps)
            or (expected is not None and expected['setup_binding'] != authenticated.get('setup_binding'))):
        raise ValueError('completed Home does not bind authenticated setup')
    actual = optional_staged_home(source)
    if (expected is None and actual is not None) or (expected is not None and (actual is None or actual['expectation'] != expected)):
        raise ValueError('transported Home differs from authenticated completed setup')
    actual_apps = optional_staged_home(source, 'applications')
    if applications is None:
        if actual_apps is not None:
            raise ValueError('transported applications lack authenticated completed selection')
    else:
        applications = home_expectation(applications, 'applications')
        if (applications['setup_binding'] != authenticated.get('setup_binding')
                or (expected is not None and applications['operation'] == expected['operation'])
                or actual_apps is None or actual_apps['expectation'] != applications):
            raise ValueError('transported applications differ from authenticated completed setup')
    # The locator's prepared_setup equality is checked against actual private
    # receipt.json by staged_home; no live Home or source mount is opened here.


NATIVE_PREPARED = '/usr/libexec/tilo-prepared-setup'
INSTALLER_ROOT = '/run/tilo-installer'


def completed_path(attempt):
    import uuid
    if not isinstance(attempt, str) or str(uuid.UUID(attempt)) != attempt:
        raise ValueError('completed generation attempt is invalid')
    return INSTALLER_ROOT + '/.completed-setup-' + attempt


def selected_generation():
    # Only the native issuer resolves selection; neither env nor public JSON
    # selects a directory. Bound stdout without communicate() buffering it all.
    with subprocess.Popen([NATIVE_PREPARED, '--selected-install-generation'],
                          stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, close_fds=True) as child:
        try:
            data = child.stdout.read(258)
            if len(data) > 257:
                raise ValueError('native generation result exceeds bound')
            if child.wait() != 0:
                raise ValueError('native generation selection failed')
        except BaseException:
            child.kill()
            child.wait()
            raise
    if not data.endswith(b'\n') or b'\n' in data[:-1]:
        raise ValueError('native generation result is malformed')
    path = data[:-1].decode('ascii')
    if path != INSTALLER_ROOT:
        prefix = INSTALLER_ROOT + '/.completed-setup-'
        if not path.startswith(prefix) or completed_path(path[len(prefix):]) != path:
            raise ValueError('native generation result is not a fixed generation')
    return path


def validate_completed_source(source, binding):
    if (not isinstance(binding, dict)
            or set(binding) != {'kind', 'attempt'}
            or binding.get('kind') != 'completed_generation'
            or str(source) != completed_path(binding['attempt'])
            or selected_generation() != str(source)):
        raise ValueError('completed payload belongs to another native generation')


def stream_completed(source, binding, expected, kind, destination=None):
    payload_names(kind)  # Refuse unknown kind before starting any helper.
    validate_completed_source(source, binding)
    digest = hashlib.sha256()
    total = 0
    prefix = bytearray()
    with subprocess.Popen([NATIVE_PREPARED, '--stream-completed-payload', kind],
                          stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, close_fds=True) as child:
        try:
            while True:
                # Read at most one beyond reviewed size, detecting excess promptly.
                chunk = child.stdout.read(min(65536, expected['sealed_bytes'] - total + 1))
                if not chunk:
                    break
                total += len(chunk)
                if total > expected['sealed_bytes']:
                    raise ValueError('completed payload exceeds reviewed length')
                if len(prefix) < 8:
                    prefix.extend(chunk[:8 - len(prefix)])
                digest.update(chunk)
                if destination is not None:
                    pending = memoryview(chunk)
                    while pending:
                        count = os.write(destination, pending)
                        if not count:
                            raise ValueError('completed ciphertext write stopped')
                        pending = pending[count:]
            # EOF is not success: native hash/GPT/custody and mount retirement
            # may fail after its last ciphertext byte has reached the pipe.
            if child.wait() != 0:
                raise ValueError('native completed payload custody failed')
        except BaseException:
            # Closing the reader makes the native writer observe BrokenPipe;
            # retain process custody until its owned mounts have retired.
            child.stdout.close()
            child.wait()
            raise
    if (total != expected['sealed_bytes'] or prefix != b'TILOMIG1'
            or digest.digest() != bytes(expected['ciphertext_sha256'])):
        raise ValueError('completed payload differs from reviewed ciphertext')
    validate_completed_source(source, binding)


@contextmanager
def payload_input(source, value, kind):
    binding, expected = value['binding'], value['expectation']
    if isinstance(binding, dict) and binding.get('kind') == 'completed_generation':
        validate_completed_source(source, binding)
        yield lambda out=None: stream_completed(source, binding, expected, kind, out)
        return
    if str(source).startswith(INSTALLER_ROOT + '/.completed-setup-'):
        raise ValueError('completed generation requires native payload custody')
    with home_root(binding) as root:
        fd, before = home_package(root, expected)
        try:
            if list(snapshot(before)) != value['source_identity']:
                raise ValueError('reviewed ciphertext inode changed')
            yield lambda out=None: stream_home(fd, before, expected, out)
        finally:
            os.close(fd)


def preflight_home(source, kind='home'):
    value = optional_staged_home(source, kind)
    if value is None:
        return None
    with payload_input(source, value, kind) as stream:
        stream()
    return value


def payload_required_bytes(source, kind):
    value = optional_staged_home(source, kind)
    if value is None:
        return 0
    expected = value['expectation']
    # Metadata is bounded by the reviewed sender limit, never an unbounded tar
    # multiplier; retain both ciphertext and authenticated plaintext concurrently.
    plaintext_copies = 2 if kind == 'applications' else 1
    return (expected['sealed_bytes'] + plaintext_copies * expected['plaintext_bytes'] + expected['limits']['metadata_bytes']
            + expected['entries'] * 8192 + HOME_RESERVE)


def home_required_bytes(source):
    return sum(payload_required_bytes(source, kind) for kind in PAYLOAD_KINDS)


def configured_size(value):
    multipliers = {'K': 1024, 'M': 1024 ** 2, 'G': 1024 ** 3, 'T': 1024 ** 4}
    suffix = value[-1:] if isinstance(value, str) else ''
    digits = value[:-1] if suffix in multipliers else value
    if not isinstance(digits, str) or not digits.isascii() or not digits.isdigit():
        raise ValueError('configured partition size is invalid')
    result = int(digits) * multipliers.get(suffix, 1)
    if not 0 < result < (1 << 63):
        raise ValueError('configured partition size exceeds bounds')
    return result


def check_home_capacity(source, root_bytes=None, disk_bytes=None, encrypted=False):
    required = home_required_bytes(source)
    if not required:
        return 0
    if (root_bytes is None) == (disk_bytes is None):
        raise ValueError('Home capacity needs exactly one reviewed root or disk size')
    size = root_bytes if root_bytes is not None else disk_bytes
    if type(size) is not int or not 0 < size < (1 << 63):
        raise ValueError('Home target geometry is invalid')
    if disk_bytes is not None:
        if encrypted:
            reserved = (2 * 1024 ** 2 + configured_size(os.environ.get('TILO_ESP_SIZE', '2G'))
                        + configured_size(os.environ.get('TILO_BOOT_SIZE', '1G')))
            helper = os.environ.get('TILO_STORE_VOLUME', '/usr/libexec/tilo-store-volume')
            output = subprocess.check_output([helper, 'plan', '--disk-bytes', str(size),
                                              '--reserved-bytes', str(reserved)], text=True)
            roots = [line.removeprefix('ROOT_BYTES=') for line in output.splitlines() if line.startswith('ROOT_BYTES=')]
            if len(roots) != 1 or not roots[0].isascii() or not roots[0].isdigit():
                raise ValueError('canonical root/store geometry is unavailable')
            size = int(roots[0])
            if not 0 < size <= disk_bytes - reserved:
                raise ValueError('canonical root/store geometry exceeds the selected disk')
        else:
            # bootc direct installation owns EFI/boot geometry. Reserve four GiB
            # plus GPT/alignment rather than treating raw disk bytes as root free.
            size -= 4 * 1024 ** 3 + 2 * 1024 ** 2
    # ext4 defaults reserve5%; leave another1% for filesystem metadata. Apply
    # this conservative allowance to other supported filesystems too. LUKS and
    # alignment are accounted for even when the selected partition is plaintext.
    usable = max(0, size - 32 * 1024 ** 2) * 94 // 100
    if usable < 32 * 1024 ** 3 + required:
        raise ValueError('reviewed target lacks space for Tilo, encrypted Home, authenticated extraction and filesystem reserves')
    return required


def private_chain(root, components, uid, gid):
    fd = directory(root)
    try:
        facts = os.fstat(fd)
        if facts.st_uid != uid or facts.st_mode & 0o022:
            raise ValueError('Home custody root has unsafe ownership or permissions')
        for index, component in enumerate(components):
            next_fd = private_child(fd, component, uid, gid, index == len(components) - 1)
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def verify_home_chain(root, components, expected_fd, uid):
    fd = directory(root)
    try:
        for component in (None, *components):
            if component is not None:
                child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
                os.close(fd)
                fd = child
            facts = os.fstat(fd)
            if facts.st_uid != uid or facts.st_mode & 0o022:
                raise ValueError('Home custody namespace changed ownership or permissions')
        actual, expected = os.fstat(fd), os.fstat(expected_fd)
        if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino) or actual.st_mode & 0o077:
            raise ValueError('Home custody namespace was replaced')
    finally:
        os.close(fd)


def retain_home(source, target_var, kind='home'):
    _, _, custody_name, _, _ = payload_names(kind)
    value = optional_staged_home(source, kind)
    if value is None:
        return None
    expected = value['expectation']
    destination = private_chain(target_var, ('lib', 'tilo', custody_name), os.geteuid(), os.getegid())
    temporary = None
    try:
        with payload_input(source, value, kind) as stream:
            try:
                existing, facts = home_package(destination, expected, os.geteuid())
            except FileNotFoundError:
                existing = None
            if existing is not None:
                try:
                    stream_home(existing, facts, expected)
                    stream()
                finally:
                    os.close(existing)
            else:
                capacity = os.fstatvfs(destination)
                plaintext_copies = 2 if kind == 'applications' else 1
                required = (expected['sealed_bytes'] + plaintext_copies * expected['plaintext_bytes'] + expected['limits']['metadata_bytes']
                            + expected['entries'] * 8192 + HOME_RESERVE)
                if capacity.f_bavail * capacity.f_frsize < required:
                    raise ValueError('installed filesystem lacks space for encrypted Home and authenticated extraction')
                temporary = '.home-ciphertext-' + secrets.token_hex(16)
                out = os.open(temporary, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=destination)
                try:
                    stream(out)
                    os.fsync(out)
                    stream_home(out, os.fstat(out), expected)
                    os.link(temporary, expected['filename'], src_dir_fd=destination, dst_dir_fd=destination, follow_symlinks=False)
                    os.fsync(destination)
                finally:
                    os.close(out)
                os.unlink(temporary, dir_fd=destination)
                temporary = None
            verify_home_chain(target_var, ('lib', 'tilo', custody_name), destination, os.geteuid())
            publish_home_json(destination, HOME_EXPECTATION, expected, os.geteuid(), os.getegid())

    finally:
        if temporary is not None:
            os.unlink(temporary, dir_fd=destination)
        os.close(destination)
    return expected


def deliver_home_to(source, home, uid, gid, kind='home'):
    _, _, custody_name, _, _ = payload_names(kind)
    source_fd = directory(source)
    facts = os.fstat(source_fd)
    if facts.st_uid != os.geteuid() or facts.st_mode & 0o077:
        os.close(source_fd)
        raise ValueError('retained Home root is not private to the firstboot service')
    try:
        destination = private_chain(home, ('.local', 'state', 'tilo', custody_name), uid, gid)
    except BaseException:
        os.close(source_fd)
        raise
    try:
        expected = home_expectation(json_file(source_fd, HOME_EXPECTATION, 16384, os.geteuid(), True), kind)
        if os.fstat(source_fd).st_dev != os.fstat(destination).st_dev:
            raise ValueError('Home custody handoff requires one filesystem; ciphertext was retained')
        try:
            existing, facts = home_package(destination, expected)
        except FileNotFoundError:
            existing = None
        if existing is not None:
            try:
                if facts.st_mode & 0o077 or facts.st_uid not in (os.geteuid(), uid):
                    raise ValueError('existing Home receiver has unsafe ownership or permissions')
                stream_home(existing, facts, expected)
                if facts.st_uid != uid:
                    original = os.stat(expected['filename'], dir_fd=source_fd, follow_symlinks=False)
                    if (original.st_dev, original.st_ino) != (facts.st_dev, facts.st_ino):
                        raise ValueError('unfinished Home ownership handoff lacks its original inode')
                    os.fchown(existing, uid, gid)
                    os.fchmod(existing, 0o600)
                    os.fsync(existing)
            finally:
                os.close(existing)
        else:
            fd, before = home_package(source_fd, expected)
            try:
                if before.st_uid not in (os.geteuid(), uid) or before.st_mode & 0o077:
                    raise ValueError('retained Home ciphertext has unsafe custody')
                stream_home(fd, before, expected)
                # Link gives atomic no-replacement custody publication without a
                # second archive allocation. The root-private source is retained
                # until receiver bytes, owner and receipt are independently checked.
                os.link(expected['filename'], expected['filename'], src_dir_fd=source_fd, dst_dir_fd=destination, follow_symlinks=False)
                os.fchown(fd, uid, gid)
                os.fchmod(fd, 0o600)
                os.fsync(fd)
            finally:
                os.close(fd)
        verify_home_chain(home, ('.local', 'state', 'tilo', custody_name), destination, uid)
        publish_home_json(destination, HOME_EXPECTATION, expected, uid, gid)
        actual, facts = home_package(destination, expected, uid)
        try:
            stream_home(actual, facts, expected)
        finally:
            os.close(actual)
        os.fsync(destination)
        verify_home_chain(home, ('.local', 'state', 'tilo', custody_name), destination, uid)
        renewed_source = directory(source)
        try:
            old, new = os.fstat(source_fd), os.fstat(renewed_source)
            if (old.st_dev, old.st_ino) != (new.st_dev, new.st_ino):
                raise ValueError('Home source custody namespace was replaced')
        finally:
            os.close(renewed_source)
        # Receiver independently verified: remove only the same ciphertext inode
        # from root custody; original installation media is never modified.
        try:
            original = os.stat(expected['filename'], dir_fd=source_fd, follow_symlinks=False)
        except FileNotFoundError:
            original = None
        received = os.stat(expected['filename'], dir_fd=destination, follow_symlinks=False)
        if original is not None:
            if (original.st_dev, original.st_ino) != (received.st_dev, received.st_ino):
                raise ValueError('Home source custody conflicts with verified receiver')
            os.unlink(expected['filename'], dir_fd=source_fd)
            os.fsync(source_fd)
        return expected
    finally:
        os.close(source_fd)
        os.close(destination)


def deliver_home_user(username):
    import pwd
    if os.geteuid() != 0:
        raise ValueError('Home custody delivery requires the firstboot root service')
    account = pwd.getpwnam(username)
    delivered = {}
    for kind in PAYLOAD_KINDS:
        _, _, custody_name, _, _ = payload_names(kind)
        source = '/var/lib/tilo/' + custody_name
        if not os.path.lexists(source):
            continue
        delivered[kind] = deliver_home_to(source, account.pw_dir, account.pw_uid, account.pw_gid, kind)
    return delivered


def main():
    import sys
    if sys.argv[1:] == ['selected-generation']:
        try:
            print(selected_generation())
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            raise SystemExit('Installer generation was not selected: ' + str(error))
        return
    if len(sys.argv) > 1 and sys.argv[1] in ('retain-authenticated-binding', 'retain', 'deliver-user', 'preflight', 'preflight-profile', 'preflight-home', 'retain-home', 'deliver-home-user', 'home-required-bytes', 'check-home-capacity', 'preflight-authenticated'):
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('operation', choices=('retain-authenticated-binding', 'retain', 'deliver-user', 'preflight', 'preflight-profile', 'preflight-home', 'retain-home', 'deliver-home-user', 'home-required-bytes', 'check-home-capacity', 'preflight-authenticated'))
        parser.add_argument('--source-dir')
        parser.add_argument('--target-var')
        parser.add_argument('--username')
        parser.add_argument('--receipt')
        parser.add_argument('--root-bytes', type=int)
        parser.add_argument('--disk-bytes', type=int)
        parser.add_argument('--encrypted', action='store_true')
        args = parser.parse_args()
        try:
            if args.operation == 'retain-authenticated-binding':
                if (not args.source_dir or not args.target_var or args.receipt or args.username
                        or args.root_bytes is not None or args.disk_bytes is not None or args.encrypted):
                    parser.error('authenticated binding requires source-dir and target-var only')
                data = sys.stdin.buffer.read(80 * 1024 + 1)
                if len(data) > 80 * 1024:
                    raise ValueError('authenticated receipt exceeds bounds')
                retain_authenticated_binding(args.source_dir, args.target_var, json.loads(data))
            elif args.operation == 'preflight-authenticated':
                if (not args.source_dir or args.receipt or args.target_var or args.username
                        or args.root_bytes is not None or args.disk_bytes is not None or args.encrypted):
                    parser.error('authenticated preflight requires source-dir only')
                data = sys.stdin.buffer.read(80 * 1024 + 1)
                if len(data) > 80 * 1024:
                    raise ValueError('authenticated receipt exceeds bounds')
                value = json.loads(data)
                preflight_authenticated(args.source_dir, value)
                sys.stdout.buffer.write(data)
            elif args.operation in ('home-required-bytes', 'check-home-capacity'):
                if not args.source_dir or args.receipt or args.target_var or args.username:
                    parser.error('Home capacity operation requires source-dir and target geometry only')
                if args.operation == 'home-required-bytes':
                    if args.root_bytes is not None or args.disk_bytes is not None or args.encrypted:
                        parser.error('home-required-bytes accepts no target geometry')
                    print(home_required_bytes(args.source_dir))
                else:
                    print(check_home_capacity(args.source_dir, args.root_bytes, args.disk_bytes, args.encrypted))
            elif args.root_bytes is not None or args.disk_bytes is not None or args.encrypted:
                parser.error('target geometry applies only to check-home-capacity')
            elif args.operation == 'preflight-home':
                if not args.source_dir or args.receipt or args.target_var or args.username:
                    parser.error('preflight-home requires source-dir only')
                for kind in PAYLOAD_KINDS:
                    preflight_home(args.source_dir, kind)
            elif args.operation == 'retain-home':
                if not args.source_dir or not args.target_var or args.receipt or args.username:
                    parser.error('retain-home requires source-dir and target-var only')
                for kind in PAYLOAD_KINDS:
                    retain_home(args.source_dir, args.target_var, kind)
            elif args.operation == 'deliver-home-user':
                if not args.username or args.source_dir or args.target_var or args.receipt:
                    parser.error('deliver-home-user requires username only')
                deliver_home_user(args.username)
            elif args.operation == 'preflight-profile':
                if not args.source_dir or args.receipt or args.target_var or args.username:
                    parser.error('preflight-profile requires source-dir only')
                preflight_profile(args.source_dir)
            elif args.operation == 'preflight':
                if not args.source_dir or not args.receipt or args.target_var or args.username:
                    parser.error('preflight requires source-dir and receipt only')
                preflight(args.source_dir, args.receipt)
            elif args.operation == 'retain':
                if not args.source_dir or not args.target_var or args.username:
                    parser.error('retain requires source-dir and target-var only')
                retain(args.source_dir, args.target_var)
            else:
                if not args.username or args.source_dir or args.target_var:
                    parser.error('deliver-user requires username only')
                deliver_user(args.username)
        except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
            parser.exit(1, f'Encrypted prepared setup was not retained: {error}\n')
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--device', action='append', default=[])
    parser.add_argument('--source-root', action='append', default=[])
    parser.add_argument('--out-dir', required=True)
    args = parser.parse_args()
    if args.device and args.source_root:
        parser.error('use canonical device discovery or explicit mounted filesystem grants')
    try:
        if args.device:
            with media_roots(args.device) as roots:
                delivered = extract(roots, args.out_dir)
                if delivered is not None:
                    receipt = {'schema': 1, **delivered}
                    for kind in PAYLOAD_KINDS:
                        stage_home(roots, args.out_dir, receipt, args.device, kind)
        else:
            delivered = extract(args.source_root, args.out_dir)
            if delivered is not None:
                for kind in PAYLOAD_KINDS:
                    stage_home(args.source_root, args.out_dir, {'schema': 1, **delivered}, kind=kind)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Encrypted prepared setup was not delivered: {error}\n')


if __name__ == '__main__':
    main()
