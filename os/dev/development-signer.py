#!/usr/bin/python3
"""Rootful development-only EFI/PCR signer, installed outside the checkout.

Provision with setup-development-signer.py. This is not a privileged service:
it requires an already-root caller and never changes firmware or enrolls keys.
"""
import os
from contextlib import contextmanager
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

PURPOSE = b'tilo-development-signer-v1\n'


def trusted(path, *, private=False, directory=False):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError('signer paths must be absolute')
    for current in (path, *path.parents):
        info = current.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('signer paths must be root-owned, non-writable and not redirected')
        if current == path:
            if directory and not stat.S_ISDIR(info.st_mode):
                raise ValueError('expected signer directory')
            if not directory and (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1):
                raise ValueError('expected singly linked signer file')
            if private and info.st_mode & 0o077:
                raise ValueError('private signing material must be owner-only')
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError('signer parent is not a directory')
    return path


def run(argv, *, pass_fds=()):
    # Never expose private-key contents or potentially sensitive tool diagnostics.
    try:
        result = subprocess.run(argv, pass_fds=pass_fds, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=120,
                                env={'PATH': '/usr/bin:/bin', 'LANG': 'C'})
    except subprocess.TimeoutExpired:
        raise ValueError('trusted signing tool timed out') from None
    if result.returncode:
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout


@contextmanager
def exclusive_output(destination):
    destination = Path(destination).absolute()
    if '..' in destination.parts:
        raise ValueError('output path must be canonical')
    parent = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        # Hold directory descriptors throughout traversal; never follow links.
        for part in destination.parent.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
            os.close(parent)
            parent = child
        info = os.fstat(parent)
        if info.st_uid != 0 or info.st_mode & 0o077:
            raise ValueError('output directory must be root-owned and owner-only')
        descriptor = os.open(destination.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o644, dir_fd=parent)
        created = os.fstat(descriptor)
        try:
            with os.fdopen(descriptor, 'wb') as output:
                yield output
        except BaseException:
            # Closing a buffered writer can be the first place ENOSPC/EIO is
            # reported. It must have the same cleanup as a failed operation.
            try:
                current = os.stat(destination.name, dir_fd=parent, follow_symlinks=False)
                if (current.st_dev, current.st_ino) == (created.st_dev, created.st_ino):
                    os.unlink(destination.name, dir_fd=parent)
            except FileNotFoundError:
                pass
            raise
    finally:
        os.close(parent)


def sign(home, operation, source, destination):
    if operation not in ('export-efi-cert', 'sign-efi', 'sign-sha256-rsa-pkcs1'):
        raise ValueError('unsupported signing operation')
    if os.getuid() != 0:
        raise ValueError('development signer requires an already-root caller')
    trusted(home, private=True, directory=True)
    marker = trusted(home / 'purpose')
    if marker.read_bytes() != PURPOSE:
        raise ValueError('missing development-only signer purpose')
    certificate = trusted(home / 'efi-signing.crt')
    crypto = trusted(Path('/usr/local/bin/tilo-crypto'))
    # Exclusive output prevents accidental overwrite, including key files.
    with exclusive_output(destination) as output:
        if operation == 'export-efi-cert':
            output.write(certificate.read_bytes())
            return
        descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= 1024 * 1024 * 1024:
                raise ValueError('signing input must be a bounded nonempty regular file')
            # Private keys can stay on a read-only mount. The already-root caller
            # supplies the trusted system temporary parent; no key is copied here.
            scratch_parent = trusted(Path('/var/lib'), directory=True)
            with tempfile.TemporaryDirectory(prefix='tilo-signing-', dir=scratch_parent) as temporary:
                # The native crypto reader deliberately uses O_NOFOLLOW, so
                # /proc/self/fd/N is not a valid input path. Snapshot the held
                # descriptor into this private directory, never reopen source.
                copied = Path(temporary) / 'input'
                with copied.open('xb') as snapshot:
                    remaining = info.st_size
                    while remaining:
                        block = os.read(descriptor, min(1024 * 1024, remaining))
                        if not block:
                            raise ValueError('signing input truncated during snapshot')
                        snapshot.write(block)
                        remaining -= len(block)
                    if os.read(descriptor, 1):
                        raise ValueError('signing input grew during snapshot')
                after = os.fstat(descriptor)
                if (info.st_size, info.st_mtime_ns, info.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                    raise ValueError('signing input changed during snapshot')
                signed = Path(temporary) / 'signed'
                if operation == 'sign-efi':
                    key = trusted(home / 'efi-signing.key', private=True)
                    argv = [str(crypto), 'sign-efi', str(key), str(certificate),
                            str(copied), str(signed)]
                else:
                    key = trusted(home / 'pcr-signing.key', private=True)
                    argv = [str(crypto), 'sign-rsa-sha256', str(key), str(copied),
                            str(signed)]
                run(argv)
                with signed.open('rb') as result:
                    shutil.copyfileobj(result, output)
        finally:
            os.close(descriptor)


def main():
    args = sys.argv[1:]
    if not args or (args[0] == 'export-efi-cert' and len(args) != 2) or (
            args[0] in ('sign-efi', 'sign-sha256-rsa-pkcs1') and len(args) != 3):
        raise ValueError('expected export-efi-cert OUTPUT or sign-efi/sign-sha256-rsa-pkcs1 INPUT OUTPUT')
    executable = trusted(Path(__file__).absolute())
    sign(executable.parent, args[0], args[1] if len(args) == 3 else None, args[-1])


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError) as error:
        sys.exit(f'development signer: {error}')
