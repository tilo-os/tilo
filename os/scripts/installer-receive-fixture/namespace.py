"""Receiving mounts over a real owned target partition; source records stay on source."""
import ctypes
import os
from pathlib import Path
import shutil
import stat

_CLONE_NEWNS = 0x00020000
_MS_BIND = 4096
_MS_REC = 16384
_MS_PRIVATE = 1 << 18
_libc = ctypes.CDLL(None, use_errno=True)
_libc.mount.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
                       ctypes.c_ulong, ctypes.c_char_p)
_libc.mount.restype = ctypes.c_int


def mount(source, destination, filesystem=None, flags=0, options=None):
    def encoded(value):
        return None if value is None else os.fsencode(value)
    if _libc.mount(encoded(source), encoded(destination), encoded(filesystem),
                   flags, encoded(options)) != 0:
        raise OSError(ctypes.get_errno(), 'owned receiving mount refused')


def copy_etc(source_fd, destination):
    """Clone actual baseline configuration, not a fabricated passwd/PAM fixture."""
    info = os.fstat(source_fd)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise RuntimeError('baseline etc descriptor is not root controlled')
    total = 0
    count = 0
    def copy_file(source, target):
        nonlocal total, count
        metadata = os.lstat(source)
        count += 1
        total += metadata.st_size
        if count > 32768 or total > 256 * 1024 * 1024 or not stat.S_ISREG(metadata.st_mode):
            raise RuntimeError('baseline etc exceeds actual regular-file clone bounds')
        fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        try:
            before = os.fstat(fd)
            if (before.st_dev, before.st_ino) != (metadata.st_dev, metadata.st_ino):
                raise RuntimeError('baseline configuration changed before clone')
            output = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             stat.S_IMODE(metadata.st_mode))
            try:
                remaining = before.st_size
                while remaining:
                    data = os.read(fd, min(remaining, 65536))
                    if not data:
                        raise RuntimeError('baseline configuration shortened during clone')
                    offset = 0
                    while offset < len(data):
                        offset += os.write(output, data[offset:])
                    remaining -= len(data)
                after = os.fstat(fd)
                if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                        after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                    raise RuntimeError('baseline configuration changed during clone')
                os.fchown(output, before.st_uid, before.st_gid)
                os.fchmod(output, stat.S_IMODE(before.st_mode))
            finally:
                os.close(output)
        finally:
            os.close(fd)
        return target
    # Canonical distribution symlinks (authselect, mtab, certificates) remain
    # symlinks. They are never followed while copying source configuration.
    shutil.copytree('/proc/self/fd/' + str(source_fd), destination, symlinks=True,
                    copy_function=copy_file)
    after = os.fstat(source_fd)
    if (info.st_dev, info.st_ino) != (after.st_dev, after.st_ino):
        raise RuntimeError('baseline etc identity changed')


def enter(target, baseline_etc_fd, backing):
    """Called only by the actual owned root receiving child, never a source actor."""
    if os.geteuid() != 0 or os.getuid() != 0:
        raise RuntimeError('receiving mount owner is not root')
    target.verify()
    backing = Path(backing)
    info = backing.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError('receiving backing is not root private')
    if _libc.unshare(_CLONE_NEWNS) != 0:
        raise OSError(ctypes.get_errno(), 'receiving mount namespace refused')
    mount(None, '/', flags=_MS_REC | _MS_PRIVATE)
    etc = backing / 'etc'
    copy_etc(baseline_etc_fd, etc)
    os.close(baseline_etc_fd)
    mount(etc, '/etc', flags=_MS_BIND)
    # Preserve an actual bind of the held target root outside /var before
    # replacing /var; the original owner path is deliberately under /var/tmp.
    mount('tmpfs', '/run', 'tmpfs', options='mode=0755,nosuid,nodev')
    original = os.open(target.mount, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        before = os.fstat(original)
        volume = Path('/run/tilo-receiving-volume')
        volume.mkdir(mode=0o755)
        mount(target.mount, volume, flags=_MS_BIND)
        from semantic import _native
        def verify_volume():
            for column, expected in (('SOURCE', str(target.partition)), ('FSTYPE', target.filesystem)):
                status, output, diagnostics = _native.bounded(
                    ['/usr/bin/findmnt', '-n', '-o', column, '--target', str(volume)],
                    timeout=10, limit=81920)
                if status != 0 or output.decode().strip() != expected:
                    raise RuntimeError('receiving bind is not the actual owned target partition')
        verify_volume()
        alias = volume.stat(follow_symlinks=False)
        if (before.st_dev, before.st_ino) != (alias.st_dev, alias.st_ino):
            raise RuntimeError('actual target volume bind differs from held original root')
        target.verify()
        # Original source protected records are reopened from the authenticated
        # relative source namespace. The target journal lives on this distinct
        # actual Btrfs target partition, not a copied source Store.
        installed_var = volume / 'var'
        installed_var.mkdir(mode=0o755)
        for relative in ('lib', 'lib/tilo', 'lib/tilo-imager', 'home', 'cache', 'tmp'):
            (installed_var / relative).mkdir(mode=0o755)
        mount(installed_var, '/var', flags=_MS_BIND)
        after = os.fstat(original)
        named = volume.stat(follow_symlinks=False)
        if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino) or (
                after.st_dev, after.st_ino) != (named.st_dev, named.st_ino):
            raise RuntimeError('actual receiving volume changed during /var activation')
        verify_volume()
    finally:
        os.close(original)
    (Path('/etc') / 'tilo').mkdir(mode=0o755, exist_ok=True)
    return Path('/var')


class Services:
    """Exact foreground service children in this receiving namespace."""
    def __init__(self, log):
        self.log = log
        self.children = []
        self.retired = False

    def spawn(self, argv, *, environment=None, stdin=None):
        import subprocess
        child = subprocess.Popen(argv, stdin=subprocess.DEVNULL if stdin is None else stdin, stdout=self.log,
                                 stderr=self.log, env=environment,
                                 start_new_session=True)
        # Retain immediately, including all readiness and subsequent failures.
        self.children.append(child)
        return child

    def start_system(self, devices, deadline):
        import time
        from semantic import bounded_command
        Path('/run/dbus').mkdir(mode=0o755)
        self.spawn(['/usr/bin/dbus-daemon', '--system', '--nofork', '--nopidfile'])
        while not Path('/run/dbus/system_bus_socket').exists():
            self.check_live()
            if time.monotonic() >= deadline:
                raise RuntimeError('actual receiving system bus did not publish')
            time.sleep(0.05)
        self.spawn(['/usr/lib/polkit-1/polkitd', '--no-debug'])
        self.spawn(['/usr/lib/systemd/systemd-udevd', '--children-max=1'])
        for device in devices:
            device = Path(device)
            info = device.stat(follow_symlinks=False)
            if not stat.S_ISBLK(info.st_mode):
                raise RuntimeError('owned receiving discovery subject is not a block device')
            sys_path = Path('/sys/dev/block') / f'{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}'
            bounded_command(['/usr/bin/udevadm', 'trigger', '--action=change', str(sys_path.resolve())],
                            deadline, self.log)
        bounded_command(['/usr/bin/udevadm', 'settle', '--timeout=10'], deadline, self.log)
        self.spawn(['/usr/libexec/udisks2/udisksd', '--disable-modules'])
        self.check_live()

    def check_live(self):
        for child in self.children:
            if os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                raise RuntimeError('owned receiving service exited before retirement')

    def stop(self):
        import signal
        import subprocess
        errors = []
        for child in reversed(self.children):
            if child.returncode is None:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    # The exact still-owned leader has not been reaped, so its
                    # process group cannot have become an unrelated new group.
                    os.killpg(child.pid, signal.SIGKILL)
                    try:
                        child.wait(timeout=5)
                    except (OSError, subprocess.TimeoutExpired) as error:
                        errors.append(error)
                except OSError as error:
                    errors.append(error)
            if child.returncode is None:
                errors.append(RuntimeError('actual receiving child reap is unknown'))
        if errors:
            # Keep this owner and every actual child reachable. Outer owned
            # PID-namespace retirement remains required on a fixture failure.
            _UNKNOWN_SERVICES.append(self)
            raise RuntimeError('receiving services retain unknown child custody') from errors[0]
        groups = {child.pid for child in self.children}
        for process in Path('/proc').iterdir():
            if process.name.isdecimal():
                try:
                    if os.getpgid(int(process.name)) in groups:
                        raise RuntimeError('receiving service worker still exists after leader reap')
                except ProcessLookupError:
                    pass
        self.retired = True


_UNKNOWN_SERVICES = []
