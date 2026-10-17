#!/usr/bin/env python3
"""Create only absent firstboot defaults and attest exactly those generated files."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import pwd
import stat
import sys

PROOFS = Path('/var/lib/tilo/firstboot-home-seeds')
LIMIT = 4 << 20

# Shared no-replace publication keeps interrupted writes out of final paths.
_atomic_path = Path(__file__).with_name('tilo-firstboot-files.py')
_spec = importlib.util.spec_from_file_location('firstboot_files', _atomic_path)
atomic = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(atomic)


def publish_bytes(directory, name, data, mode, uid, gid):
    fd = os.open(directory, atomic.DIR)
    try:
        atomic.publish(fd, name, lambda temporary: atomic.write_file(
            fd, temporary, io.BytesIO(data), mode, uid, gid))
    finally:
        os.close(fd)


def verify_receipt(receipt, home, uid):
    trusted(receipt)
    info = receipt.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > LIMIT:
        raise ValueError('unsafe firstboot seed proof')
    fd = os.open(receipt, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        body = json.loads(stream.read(LIMIT + 1))
        if stamp(os.fstat(stream.fileno())) != stamp(info):
            raise ValueError('firstboot seed proof changed')
    if (not isinstance(body, dict) or body.get('schema') != 1
            or body.get('uid') != uid or body.get('home_inode') != home.stat().st_ino
            or body.get('filesystem') != os.statvfs(home).f_fsid
            or not isinstance(body.get('files'), list) or len(body['files']) > 4096):
        raise ValueError('firstboot seed proof does not match home')
    seen = set()
    for entry in body['files']:
        if not isinstance(entry, dict) or not isinstance(entry.get('path'), str):
            raise ValueError('malformed firstboot seed proof')
        relative = Path(entry['path'])
        if (relative.is_absolute() or not relative.parts or '..' in relative.parts
                or str(relative) != entry['path'] or entry['path'] in seen
                or any(relative == Path(custody) or Path(custody) in relative.parents
                       for custody in ('.config/tilo', '.local/state/tilo'))):
            raise ValueError('unsafe firstboot seed proof path')
        seen.add(entry['path'])
        directory = os.open(home, atomic.DIR)
        try:
            for part in relative.parts[:-1]:
                child = os.open(part, atomic.DIR, dir_fd=directory)
                os.close(directory)
                directory = child
            fd = os.open(relative.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                         dir_fd=directory)
            with os.fdopen(fd, 'rb') as stream:
                before = os.fstat(stream.fileno())
                if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
                        or before.st_uid != uid or before.st_size > LIMIT
                        or stamp(before) != entry.get('stamp')):
                    raise ValueError('attested firstboot default changed')
                data = stream.read(LIMIT + 1)
                if (stamp(os.fstat(stream.fileno())) != stamp(before)
                        or hashlib.sha256(data).hexdigest() != entry.get('sha256')):
                    raise ValueError('attested firstboot default changed')
        finally:
            os.close(directory)
    return body


def trusted(path):
    info = path.lstat()
    if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
        raise ValueError('firstboot seed authority is not root controlled')


def stamp(info):
    return {'inode': info.st_ino, 'uid': info.st_uid, 'mode': stat.S_IMODE(info.st_mode),
            'bytes': info.st_size, 'mtime_ns': info.st_mtime_ns, 'ctime_ns': info.st_ctime_ns}


def seed(home, skeleton, uid, gid, proofs=PROOFS):
    if os.geteuid() != 0:
        raise PermissionError('firstboot seed creation requires root')
    for parent in (proofs.parent.parent, proofs.parent):
        trusted(parent)
    proofs.mkdir(mode=0o755, exist_ok=True)
    trusted(proofs)
    receipt = proofs / (str(uid) + '.json')
    home.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = home.lstat()
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise ValueError('firstboot home is redirected')
    if os.path.lexists(receipt):
        return verify_receipt(receipt, home, uid)
    if (info.st_uid, info.st_gid) != (uid, gid):
        os.chown(home, uid, gid)
    files = []
    if skeleton.exists():
        trusted(skeleton)
        for directory, folders, names in os.walk(skeleton, followlinks=False):
            folders.sort()
            names.sort()
            source_dir = Path(directory)
            trusted(source_dir)
            relative = source_dir.relative_to(skeleton)
            destination = home / relative
            if os.path.lexists(destination):
                target = destination.lstat()
                if not stat.S_ISDIR(target.st_mode) or stat.S_ISLNK(target.st_mode):
                    folders.clear()
                    continue
            else:
                destination.mkdir(mode=stat.S_IMODE(source_dir.stat().st_mode))
                os.chmod(destination, stat.S_IMODE(source_dir.stat().st_mode))
                os.chown(destination, uid, gid)
            # Literal skel links keep normal account setup semantics. They are
            # not automatically replaceable: only regular files are attested.
            names.extend(name for name in folders if (source_dir / name).is_symlink())
            folders[:] = [name for name in folders if not (source_dir / name).is_symlink()]
            for name in names:
                source, target = source_dir / name, destination / name
                metadata = source.lstat()
                if stat.S_ISLNK(metadata.st_mode):
                    if metadata.st_uid != 0:
                        raise ValueError('firstboot skeleton link has foreign ownership')
                    if not os.path.lexists(target):
                        literal = os.readlink(source)
                        if len(os.fsencode(literal)) > 4096:
                            raise ValueError('firstboot skeleton link exceeds bounds')
                        directory_fd = os.open(destination, atomic.DIR)
                        try:
                            def link(temporary):
                                os.symlink(literal, temporary, dir_fd=directory_fd)
                                os.chown(temporary, uid, gid, dir_fd=directory_fd,
                                         follow_symlinks=False)
                            atomic.publish(directory_fd, name, link)
                        finally:
                            os.close(directory_fd)
                    continue
                trusted(source)
                if not stat.S_ISREG(metadata.st_mode):
                    raise ValueError('unsupported special file in firstboot skeleton')
                if os.path.lexists(target):
                    continue
                if metadata.st_size > LIMIT or len(files) >= 4096:
                    raise ValueError('firstboot skeleton exceeds bounds')
                source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
                try:
                    with os.fdopen(source_fd, 'rb') as input_stream:
                        data = input_stream.read(LIMIT + 1)
                        if len(data) != metadata.st_size or stamp(os.fstat(input_stream.fileno())) != stamp(metadata):
                            raise ValueError('firstboot skeleton changed')
                    publish_bytes(destination, name, data,
                                  stat.S_IMODE(metadata.st_mode) & 0o777, uid, gid)
                    witness = stamp(target.lstat())
                    relative_file = target.relative_to(home)
                    if not any(relative_file == Path(custody) or Path(custody) in relative_file.parents
                               for custody in ('.config/tilo', '.local/state/tilo')):
                        files.append({'path': str(relative_file), 'stamp': witness,
                                      'sha256': hashlib.sha256(data).hexdigest()})
                except BaseException:
                    # Never remove a path after a possible same-name substitution.
                    raise
            directory_fd = os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    home_info = home.stat()
    body = {'schema': 1, 'home_inode': home_info.st_ino,
            'filesystem': os.statvfs(home).f_fsid, 'uid': uid, 'files': files}
    encoded = (json.dumps(body, separators=(',', ':')) + '\n').encode()
    if len(encoded) > LIMIT:
        raise ValueError('firstboot seed proof exceeds bounds')
    # Files published before an interrupted receipt are conservatively treated
    # as existing user data on retry. Never reconstruct their provenance merely
    # because their contents match skel: migrated files can match it too.
    publish_bytes(proofs, receipt.name, encoded, 0o644, 0, 0)
    for directory in (home, proofs):
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    return body


def main():
    if sys.argv[1:] == ['--capabilities']:
        print('{"schema":1,"seed_protocol":1}')
        return
    if len(sys.argv) != 2:
        raise ValueError('expected one account name')
    account = pwd.getpwnam(sys.argv[1])
    seed(Path(account.pw_dir), Path('/etc/skel'), account.pw_uid, account.pw_gid)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit('firstboot home defaults: ' + str(error)) from error
