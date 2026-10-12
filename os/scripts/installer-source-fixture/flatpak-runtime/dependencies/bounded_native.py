#!/usr/bin/env python3
"""Root-only disposable clean/dirty fixtures; never accepts existing device paths.
Run ONLY inside the selected rootful immutable Fedora44 no-network namespace.
Does not build anything. Root supplies compiled exact Rust native qualifier.
"""
import os
import selectors
import signal
import subprocess
import time

BOUND = 1024 * 1024
FICLONE = 0x40049409
TOOLS = ('e2fsck', 'xfs_repair', 'mkfs.ext4', 'mkfs.xfs', 'losetup',
         'mount', 'umount', 'blkid', 'findmnt', 'mountpoint', 'setfacl')



class BoundedFailure(RuntimeError):
    def __init__(self, message, output, limit):
        super().__init__(message)
        # Even an output-limit failure cannot publish more than its admitted
        # aggregate diagnostic budget. Timeout snapshots retain actual bytes.
        self.stdout = bytes(output[0][:limit])
        self.stderr = bytes(output[1][:max(0, limit-len(self.stdout))])

def bounded(argv, *, timeout=1800, pass_fds=(), limit=BOUND, test_manifest=None, stdin=None, env=None, preexec_fn=None):
    environment = {'PATH': '/usr/bin:/usr/sbin', 'LC_ALL': 'C', 'LANG': 'C'} if env is None else env
    if test_manifest is not None:
        environment['TILO_CLEAN_NATIVE_FIXTURE'] = test_manifest
    process = subprocess.Popen(argv, env=environment,
                               stdin=subprocess.DEVNULL if stdin is None else stdin, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True, pass_fds=pass_fds, preexec_fn=preexec_fn)
    output = [bytearray(), bytearray()]
    selector = selectors.DefaultSelector()
    for index, pipe in enumerate((process.stdout, process.stderr)):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ, index)
    deadline = time.monotonic() + timeout
    observed = None
    try:
        while selector.get_map() or observed is None:
            if time.monotonic() >= deadline:
                raise BoundedFailure('native qualification command deadline exceeded', output, limit)
            for key, _ in selector.select(0.05):
                part = os.read(key.fileobj.fileno(), 8192)
                if not part:
                    selector.unregister(key.fileobj)
                else:
                    output[key.data].extend(part)
                    if sum(map(len, output)) > limit:
                        raise BoundedFailure('native qualification command output exceeded bound', output, limit)
            if observed is None:
                observed = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        status = observed.si_status if observed.si_code == os.CLD_EXITED else -observed.si_status
        return status, bytes(output[0]), bytes(output[1])
    finally:
        # WNOWAIT keeps the leader PID reserved until the exact group is killed.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        selector.close()
        process.stdout.close()
        process.stderr.close()

