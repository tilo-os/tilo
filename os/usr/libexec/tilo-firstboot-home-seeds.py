#!/usr/bin/env python3
"""Create only absent firstboot defaults and attest exactly those generated files."""
import hashlib
import json
import os
from pathlib import Path
import pwd
import stat
import sys

PROOFS = Path('/var/lib/tilo/firstboot-home-seeds')
LIMIT = 4 << 20


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
    if os.path.lexists(receipt):
        raise ValueError('firstboot seed proof already exists')
    home.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = home.lstat()
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise ValueError('firstboot home is redirected')
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
                        os.symlink(literal, target)
                        os.chown(target, uid, gid, follow_symlinks=False)
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
                    target_fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                        stat.S_IMODE(metadata.st_mode) & 0o777)
                    with os.fdopen(target_fd, 'wb') as output:
                        output.write(data)
                        output.flush()
                        os.fchmod(output.fileno(), stat.S_IMODE(metadata.st_mode) & 0o777)
                        os.fchown(output.fileno(), uid, gid)
                        os.fsync(output.fileno())
                        witness = stamp(os.fstat(output.fileno()))
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
    fd = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, 'wb') as output:
        output.write(encoded)
        output.flush()
        os.fsync(output.fileno())
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
