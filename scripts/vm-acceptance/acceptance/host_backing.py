"""Bounded executable-only proof for an authenticated host-window kernel peer."""
import os
from pathlib import Path
from .runtime_backing import hash_backing, start_time


def probe(uid, pid, start, installed_executable, proc=Path('/proc')):
    if type(uid) is not int or uid <= 0 or type(pid) is not int or pid <= 0:
        raise RuntimeError('invalid authenticated host peer')
    if not isinstance(start, str) or not start.isdecimal():
        raise RuntimeError('invalid authenticated host lifetime')
    process = proc / str(pid)
    if process.stat().st_uid != uid or start_time(process) != start:
        raise RuntimeError('host peer owner or lifetime changed')
    expected = hash_backing(Path(installed_executable))
    actual = hash_backing(process / 'exe')
    if start_time(process) != start or process.stat().st_uid != uid:
        raise RuntimeError('host peer changed during executable proof')
    if actual['sha256'] != expected['sha256']:
        raise RuntimeError('host executable differs from installed Flatpak deployment')
    return {'method': 'proc-exe-fd', 'pid': pid, 'start_time_ticks': start,
            'uid': uid, 'reader_uid': os.getuid(), 'executable': actual,
            'installed_executable': expected, 'native_sdk_claim': False}
