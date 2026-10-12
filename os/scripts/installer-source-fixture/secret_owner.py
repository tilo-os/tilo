"""Owned PAM Secret Service retirement, witnessed through its actual private bus.

The bus name supplies no credential or source authority. Kernel parentage,
UID, executable, namespace and pidfd bind this infrastructure process to the
new source login before any signal; unknown ownership retains the namespace.
"""
import ast
import os
from pathlib import Path
import re
import select
import signal
import stat
import time


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def process(pid):
    raw = (Path('/proc') / str(pid) / 'stat').read_bytes()
    tail = raw.rsplit(b') ', 1)[1].split()
    return int(tail[1]), int(tail[19])


def namespace(pid):
    info = (Path('/proc') / str(pid) / 'ns/mnt').stat()
    return info.st_dev, info.st_ino


class PamSecretOwner:
    def __init__(self, pid):
        self.pid = pid
        self.pidfd = None
        self.program = None
        try:
            self.pidfd = os.pidfd_open(pid, 0)
            self.program = os.open('/usr/bin/gnome-keyring-daemon', os.O_PATH | os.O_CLOEXEC)
            public = Path('/usr/bin/gnome-keyring-daemon').resolve(strict=True)
            for item in (public, *public.parents):
                info = item.lstat()
                require(info.st_uid == 0 and not info.st_mode & 0o022
                        and not stat.S_ISLNK(info.st_mode), 'PAM daemon protected program refused')
            self.start = process(pid)[1]
            self.mount = namespace(os.getpid())
            self.check()
            current = pid
            for _ in range(8):
                parent, before = process(current)
                require(namespace(current) == self.mount, 'PAM daemon ancestry namespace differs')
                require(process(current) == (parent, before), 'PAM daemon ancestry changed')
                if parent == os.getpid():
                    break
                require(parent > 1 and parent != current, 'PAM daemon is not an owned descendant')
                current = parent
            else:
                raise RuntimeError('PAM daemon ancestry bound refused')
            self.check()
        except BaseException:
            self.close_descriptors()
            raise

    def exited(self):
        return bool(select.select([self.pidfd], [], [], 0)[0])

    def check(self):
        require(not self.exited() and process(self.pid)[1] == self.start,
                'PAM daemon actual process changed or stopped')
        require(namespace(self.pid) == self.mount, 'PAM daemon private namespace differs')
        status = (Path('/proc') / str(self.pid) / 'status').read_text()
        uid = next(line for line in status.splitlines() if line.startswith('Uid:')).split()[1:]
        require(uid == ['1000'] * 4, 'PAM daemon actual UID differs')
        executable = (Path('/proc') / str(self.pid) / 'exe').stat()
        held = os.fstat(self.program)
        require(stat.S_ISREG(held.st_mode)
                and (executable.st_dev, executable.st_ino) == (held.st_dev, held.st_ino),
                'PAM daemon actual executable differs')
        require(process(self.pid)[1] == self.start and not self.exited(),
                'PAM daemon changed during kernel checks')

    def retire_after_login(self, deadline):
        if not self.exited():
            self.check()
            # Login was already observed/reaped. The subreaper must own this
            # exact adopted child before sending through its held kernel pidfd.
            require(process(self.pid)[0] == os.getpid(), 'PAM daemon adoption not observed')
            signal.pidfd_send_signal(self.pidfd, signal.SIGTERM)
        while not self.exited():
            require(time.monotonic() < deadline, 'PAM daemon retirement unknown')
            select.select([self.pidfd], [], [], min(.05, deadline - time.monotonic()))
        observed = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        require(observed is not None and observed.si_pid == self.pid,
                'PAM daemon owned exit receipt absent')
        reaped, _ = os.waitpid(self.pid, 0)
        require(reaped == self.pid, 'PAM daemon actual reap differs')
        self.close_descriptors()

    def close_descriptors(self):
        for name in ('pidfd', 'program'):
            descriptor = getattr(self, name, None)
            if descriptor is not None:
                os.close(descriptor)
                setattr(self, name, None)


def discover(runtime, username, fixed, deadline):
    def call(method, argument):
        return fixed(['/usr/sbin/runuser', '-u', username, '--', '/usr/bin/gdbus', 'call',
            '--address', 'unix:path=' + str(runtime / 'bus'), '--dest', 'org.freedesktop.DBus',
            '--object-path', '/org/freedesktop/DBus', '--method',
            'org.freedesktop.DBus.' + method, argument], deadline=deadline).strip()
    present = call('NameHasOwner', 'org.freedesktop.secrets')
    require(present in (b'(true,)', b'(false,)'), 'actual Secrets bus presence reply refused')
    if present == b'(false,)':
        return None
    name = call('GetNameOwner', 'org.freedesktop.secrets')
    decoded = ast.literal_eval(name.decode('ascii'))
    require(isinstance(decoded, tuple) and len(decoded) == 1
            and isinstance(decoded[0], str) and re.fullmatch(r':[0-9]+\.[0-9]+', decoded[0]),
            'actual Secrets unique bus owner refused')
    pid_reply = call('GetConnectionUnixProcessID', decoded[0])
    matched = re.fullmatch(rb'\(uint32 ([1-9][0-9]*),\)', pid_reply)
    require(matched is not None, 'actual Secrets kernel PID reply refused')
    owner = PamSecretOwner(int(matched[1]))
    try:
        require(call('GetNameOwner', 'org.freedesktop.secrets') == name
                and call('GetConnectionUnixProcessID', decoded[0]) == pid_reply,
                'actual Secrets bus owner changed')
        owner.check()
        return owner
    except BaseException:
        owner.close_descriptors()
        raise
