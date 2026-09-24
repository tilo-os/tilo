#!/usr/bin/env python3
"""Opt-in Linux fixture coordinator. Never creates/mounts/changes a device itself.

An explicitly supplied external action performs detach/replace while ptrace holds
an actual production broker at its native reader fork. No production test hook,
caller-supplied claim, or bypass of inventory/privilege/postflight checks exists.
"""
import argparse
import ctypes
import json
import os
import pathlib
import signal
import stat
import subprocess
import sys
import tempfile
import time


def require(condition, reason):
    if not condition:
        raise RuntimeError(reason)


def inventory(probe):
    result = subprocess.run([probe, 'inventory'], capture_output=True, timeout=22, check=True)
    value = json.loads(result.stdout)
    require(value['diagnostic_only'] is True, 'inventory schema/authority mismatch')
    require(not value['unavailable'], 'coordinator must resolve all physical candidate ambiguity')
    return value['inventory']


def select(probe, device):
    info = os.stat(device, follow_symlinks=False)
    require(stat.S_ISBLK(info.st_mode), 'fixture must be a direct physical block partition')
    matches = [c for c in inventory(probe)
               if (c['major'], c['minor']) == (os.major(info.st_rdev), os.minor(info.st_rdev))]
    require(len(matches) == 1, 'production inventory must discover exactly one selected partition')
    selected = matches[0]
    require((pathlib.Path(selected['sysfs']) / 'partition').is_file(), 'physical partition required')
    require(selected['hardware'] and selected['start'] > 0, 'native hardware/partition identity required')
    return selected, info.st_rdev


def arguments(probe, selected):
    return [probe, 'inspect-selected', '--inventory-id', selected['inventory_id'], '--root-mft', '5']


def positive(probe, selected):
    result = subprocess.run(arguments(probe, selected), capture_output=True, timeout=22, check=True)
    value = json.loads(result.stdout)
    require(value['inventory'] == selected, 'selected inventory changed')
    require(value['diagnostic_only'] is True and value['report']['status'] == 0,
            'production reader must succeed diagnostically')
    require(value['report']['memory_limit_enforced'] is True, 'Linux allocation limit not enforced')
    require(value['report']['metadata_complete'] is False, 'unexpected capability promotion')
    require(value['report']['stats']['writes'] == 0, 'production attempted writes')
    require(value['native_identity']['root_record'] == 5 and value['persistent_root'],
            'native root identity missing')
    return {'inventory_id': selected['inventory_id'], 'persistent_root': value['persistent_root'],
            'bytes': value['report']['stats']['bytes'], 'writes': value['report']['stats']['writes']}


LIBC = ctypes.CDLL(None, use_errno=True)
LIBC.ptrace.argtypes = [ctypes.c_uint, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
LIBC.ptrace.restype = ctypes.c_long


def trace(request, pid, data=0):
    result = LIBC.ptrace(request, pid, None, data)
    if result == -1:
        raise OSError(ctypes.get_errno(), 'ptrace')
    return result


def waited(pid, deadline):
    while time.monotonic() < deadline:
        found, status = os.waitpid(pid, os.WNOHANG | 0x40000000)  # __WALL, traced fork child
        if found:
            return status
        time.sleep(0.005)
    raise TimeoutError('coordinator exceeded bounded trace deadline')


def held_readonly(pid, expected_rdev):
    matches = []
    for path in pathlib.Path(f'/proc/{pid}/fd').iterdir():
        info = path.stat()
        if stat.S_ISBLK(info.st_mode) and info.st_rdev == expected_rdev:
            fields = dict(line.split(':', 1) for line in
                          pathlib.Path(f'/proc/{pid}/fdinfo/{path.name}').read_text().splitlines())
            require(int(fields['flags'], 8) & os.O_ACCMODE == os.O_RDONLY, 'claim is not readonly')
            matches.append(int(path.name))
    require(matches, 'actual production broker has no selected retained block descriptor')
    return matches


def ended(pid):
    path = pathlib.Path(f'/proc/{pid}/stat')
    try:
        # A reparented zombie is exited and has no file descriptors; its init
        # reaping schedule is outside the broker contract.
        state = path.read_text().rsplit(')', 1)[1].split()[0]
        return state == 'Z' and not list(path.parent.joinpath('fd').iterdir())
    except FileNotFoundError:
        return True


def adversarial(probe, device, selected, rdev, action, kind):
    require(pathlib.Path(action).is_absolute(), 'external coordinator action must be an absolute executable path')
    broker = reader = None
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            broker = os.fork()
            if broker == 0:
                try:
                    os.dup2(stdout.fileno(), 1)
                    os.dup2(stderr.fileno(), 2)
                    trace(0, 0)  # PTRACE_TRACEME, exec then traps before broker instructions
                    os.execv(probe, arguments(probe, selected))
                except BaseException:
                    os._exit(125)
            deadline = time.monotonic() + 15
            status = waited(broker, deadline)
            require(os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGTRAP,
                    'production executable did not reach exec trace stop')
            trace(0x4200, broker, 2 | 0x00100000)  # TRACEFORK | EXITKILL
            trace(7, broker)  # CONT
            status = waited(broker, deadline)
            require(os.WIFSTOPPED(status) and status >> 16 == 1,
                    'production must acquire claims and fork a real reader before action')
            child = ctypes.c_ulong()
            trace(0x4201, broker, ctypes.addressof(child))  # GETEVENTMSG
            reader = child.value
            require(reader > 0, 'missing native reader child')
            child_status = waited(reader, deadline)
            require(os.WIFSTOPPED(child_status), 'reader did not stop at fork')
            descriptors = held_readonly(broker, rdev)
            held_readonly(reader, rdev)
            request = {'schema': 1, 'action': kind, 'phase': 'production-claimed-reader-fork',
                       'device': device, 'inventory': selected, 'broker_pid': broker,
                       'reader_pid': reader, 'claim_fds': descriptors}
            # Only this explicit external coordinator changes the disposable
            # fixture. It must finish within five seconds; no shell expansion.
            result = subprocess.run([action, kind], input=json.dumps(request).encode(),
                                    capture_output=True, timeout=5, check=True)
            require(len(result.stdout) <= 65536, 'oversized action receipt')
            receipt = json.loads(result.stdout)
            require(receipt == {'schema': 1, 'completed': kind}, 'action receipt mismatch')
            # Independently observe actual kernel identity invalidation, not the
            # external coordinator's assertion. A no-op action cannot pass.
            old_sysfs = pathlib.Path(selected['sysfs'])
            try:
                invalidated = (old_sysfs.stat().st_ino != selected['sysfs_inode'] or
                               int((old_sysfs.parent / 'diskseq').read_text()) != selected['disk_sequence'])
            except FileNotFoundError:
                invalidated = True
            require(invalidated, 'coordinator action did not detach or replace the kernel object')
            if kind == 'replace':
                replacements = [item for item in inventory(probe)
                                if item['name'] == selected['name']]
                require(len(replacements) == 1 and
                        replacements[0]['disk_sequence'] != selected['disk_sequence'],
                        'replacement requires a present replacement partition with new kernel disk sequence')
            else:
                require(not old_sysfs.exists(), 'detach requires the old kernel partition to be absent')
            # Detach the observer before credential/seccomp setup: ptrace must
            # not suppress the production privilege transition or syscall rules.
            trace(17, reader)  # DETACH and resume
            trace(17, broker)
            status = waited(broker, time.monotonic() + 22)
            require(os.WIFEXITED(status) or os.WIFSIGNALED(status), 'broker did not terminate')
            broker_status = status
            broker_pid = broker
            broker = None  # already reaped; never send a signal to a reused PID
            stdout.seek(0)
            require(not stdout.read(1), 'invalidated inspection published evidence')
            require(not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0,
                    'invalidated production inspection succeeded')
            until = time.monotonic() + 2
            while not ended(reader) and time.monotonic() < until:
                time.sleep(0.01)
            require(ended(reader) and ended(broker_pid), 'reader or retained claims survived teardown')
            reader = None
            return {'action': kind, 'synchronized_phase': request['phase'],
                    'kernel_identity_invalidated': True, 'evidence_bytes': 0,
                    'reader_and_claims_torn_down': True, 'broker_wait_status': broker_status}
        finally:
            for pid in (reader, broker):
                if pid is not None and not ended(pid):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            if broker is not None:
                try:
                    os.waitpid(broker, 0)
                except ChildProcessError:
                    pass


def main():
    require(sys.platform == 'linux' and os.geteuid() == 0 and
            os.environ.get('QOS_NTFS_NATIVE_AUTHORIZATION') == 'DISPOSABLE_FIXTURE_ONLY',
            'explicit disposable Linux root opt-in required')
    parser = argparse.ArgumentParser()
    parser.add_argument('--probe', required=True)
    parser.add_argument('--mode', required=True, choices=['positive', 'detach', 'replace'])
    args = parser.parse_args()
    probe = str(pathlib.Path(args.probe).resolve(strict=True))
    device = os.environ['QOS_NTFS_NATIVE_DEVICE']
    selected, rdev = select(probe, device)
    result = {'positive': positive(probe, selected)}
    if args.mode != 'positive':
        result['adversarial'] = adversarial(probe, device, selected, rdev,
                                           os.environ['QOS_NTFS_NATIVE_ACTION'], args.mode)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
