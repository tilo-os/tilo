#!/usr/bin/env python3
"""Executable native Copy acceptance, UNRUN until separately authorized on Linux.

Uses the actual Imager administrative command and fresh broker selections.
No installation or credentials policy changes; existing selected UID required.
"""
import argparse
import ctypes
import hashlib
import json
import os
import pathlib
import pwd
import signal
import struct
import subprocess
import sys
import tempfile
import time
from native_coordinator import require, select as select_device, held_readonly, ended, trace, waited, inventory
from native_session_coordinator import Session, full_hash, reacquire, aliases


def frames(path):
    data = path.read_bytes()
    require(data[:8] == b'QOSJNL3\n', 'native V3 magic')
    offset, sequence, records = 8, 0, []
    while offset < len(data):
        require(len(data) - offset >= 44, 'torn production receipt')
        length, actual = struct.unpack_from('<IQ', data, offset)
        require(0 < length <= 128 * 1024 and actual == sequence, 'bounded sequenced frame')
        payload = data[offset + 44:offset + 44 + length]
        require(len(payload) == length and hashlib.sha256(data[offset:offset + 12] + payload).digest() == data[offset + 12:offset + 44], 'receipt checksum')
        records.append(json.loads(payload)); sequence += 1; offset += 44 + length
    require(any(r['record'] == 'Seal' for r in records), 'sealed reviewed inventory missing')
    return records


def ordinary_credentials(uid):
    def establish():
        account = pwd.getpwuid(uid)
        os.setgroups([]); os.setgid(account.pw_gid); os.setuid(uid)
        require(os.getuid() == uid and uid != 0, 'ordinary selected client required')
    return establish


def private_directory(path, uid):
    path.mkdir(mode=0o700)
    os.chown(path, uid, pwd.getpwuid(uid).pw_gid)


def command(session, imager, workspace, uid, resume=False):
    descriptor = workspace / 'session.json'
    descriptor.write_text(json.dumps(session.locator))
    os.chmod(descriptor, 0o600); os.chown(descriptor, uid, pwd.getpwuid(uid).pw_gid)
    args = [imager, '--native-resume-selected' if resume else '--native-copy-selected', str(descriptor), json.dumps(session.locator['identity'])]
    if not resume:
        args.append(str(workspace / 'output'))
    args.append(str(workspace / 'journal' / 'copy.v3'))
    return args


def result(output):
    lines = [line for line in output.splitlines() if line.startswith(b'{')]
    require(len(lines) == 1, 'missing single native Copy result')
    report = json.loads(lines[0])
    require(report['acceptance'] == 'unverified-administrative-only', 'public capability promotion')
    return report


def run_copy(probe, imager, selected, uid, device, workspace, resume=False, root=5, test_binary=None, boundary=None, conflict=False):
    session = Session(probe, selected, uid, root=root)
    try:
        held_readonly(session.process.pid, os.stat(device).st_rdev)
        args = command(session, imager, workspace, uid, resume)
        env = dict(os.environ)
        if boundary:
            require(test_binary, 'crash mode requires exact-source native test executable')
            env.update(QOS_NATIVE_COPY_COMMAND_ARGS=json.dumps(args), QOS_JOURNAL_BOUNDARY=boundary, QOS_JOURNAL_BOUNDARY_COUNT='1')
            args = [test_binary, 'native_selected_copy_command_worker', '--ignored', '--nocapture']
        process = subprocess.run(args, env=env, preexec_fn=ordinary_credentials(uid), capture_output=True, timeout=1800)
        if boundary:
            require(process.returncode == 73, 'requested publication/receipt crash did not occur')
            report = None
        elif conflict:
            require(process.returncode != 0 and b'preserved conflict' in process.stderr, 'unreceipted output did not require review')
            report = None
        else:
            require(process.returncode in (0, 2), 'native command failed: ' + repr(process.stderr))
            report = result(process.stdout)
        session.closed(); reacquire(device)
        return report
    finally:
        session.close()


def inspect(probe, selected, uid, device, root=5):
    session = Session(probe, selected, uid, root=root)
    try:
        entries, _ = session.begin()
        session.finish(); session.receive(session.request('Close')); session.closed(); reacquire(device)
        return entries
    finally:
        session.close()


def digest_without_atime(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME)
    with os.fdopen(fd, 'rb') as source:
        digest = hashlib.sha256()
        while part := source.read(1024 * 1024):
            digest.update(part)
        return digest.hexdigest()


def verify_output(workspace, entries, report):
    require(report['scan_completed'] and not report['failed'] and not report['cancelled'], 'native Copy did not finish ordinary scope')
    # Root selection intentionally retains NTFS system-file pending scopes.
    require(all(path.startswith('$') for path, _ in report['skipped']), 'unexpected ordinary scope pending')
    expected = hashlib.sha256(b''.join(bytes((j + block) % 256 for j in range(256)) * 4096 for block in range(12))).hexdigest()
    require('large.bin' in report['verified_files'], '12 MiB content not durably receipted')
    for entry in entries:
        path = '/'.join(bytes(struct.pack('<' + 'H' * len(component), *component)).decode('utf-16-le') for component in entry['path'])
        if path not in report['verified_files']:
            continue
        file = workspace / 'output' / path; metadata = file.stat(); times = entry['copy_times']
        require(metadata.st_mtime_ns == (times['last_data_change_100ns'] - 116444736000000000) * 100, 'modification timestamp mismatch')
        require(metadata.st_atime_ns == (times['last_access_100ns'] - 116444736000000000) * 100, 'access timestamp mismatch')
    require(digest_without_atime(workspace / 'output' / 'large.bin') == expected, 'independent destination digest')
    require((workspace / 'output' / 'empty.bin').stat().st_size == 0, 'empty file missing')
    records = frames(workspace / 'journal' / 'copy.v3')
    receipts = [r['receipt'] for r in records if r['record'] == 'Receipt']
    require(any(r['source']['size'] == 12 * 1024 * 1024 and r['digest'] and r['completed'] for r in receipts), 'native file receipt missing')
    require(any(r['source']['directory'] and r['completed'] for r in receipts), 'completed native directory receipt missing')
    require(records[-1]['record'] == 'CommitOutcome', 'durable outcome not committed')
    return {'verified_bytes': report['verified_bytes'], 'large_sha256': expected, 'native_receipts': len(receipts)}


def held_copy(args, selected, uid, device, workspace, root):
    session = Session(args.probe, selected, uid, root=root)
    child, runner = None, None
    try:
        trace(0x4206, session.process.pid, 2 | 0x00100000)  # real worker fork holds
        runner = subprocess.Popen(command(session, args.imager, workspace, uid), preexec_fn=ordinary_credentials(uid), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        # Two metadata observations = four workers; file Begin = fifth; each
        # independent one-chunk pass = health/range/health. Finish = twelfth.
        target = 12 if args.mode == 'cancel-postflight' else 10
        seen, deadline = 0, time.monotonic() + 120
        while seen < target:
            status = waited(session.process.pid, deadline)
            require(os.WIFSTOPPED(status), 'broker ended before native Copy target')
            if status >> 16 != 1:
                require(os.WSTOPSIG(status) == signal.SIGCHLD, 'unexpected broker stop')
                trace(7, session.process.pid); continue
            child_id = ctypes.c_ulong(); trace(0x4201, session.process.pid, ctypes.addressof(child_id)); child = child_id.value
            require(os.WIFSTOPPED(waited(child, deadline)), 'missing native child fork stop')
            seen += 1
            if seen != target:
                trace(17, child); trace(7, session.process.pid); child = None; continue
            held_readonly(child, os.stat(device).st_rdev)
            if args.mode.startswith('cancel-'):
                runner.send_signal(signal.SIGINT)
            elif args.mode == 'client-death':
                runner.kill()
            elif args.mode == 'broker-death':
                os.kill(session.process.pid, signal.SIGKILL)
            elif args.mode == 'parser-death':
                trace(17, child, signal.SIGKILL); child = None
            elif args.mode == 'aliases':
                with tempfile.TemporaryDirectory(prefix='qos-native-copy-mount-refusal-') as mountpoint:
                    for prefix in ([], ['unshare', '--mount', '--propagation', 'private']):
                        attempt = subprocess.run(prefix + ['mount', '-t', 'ntfs3', '-o', 'ro', device, mountpoint], capture_output=True, timeout=10, env=dict(os.environ, LC_ALL='C'))
                        require(attempt.returncode != 0 and b'busy' in attempt.stderr.lower(), 'live Copy claim did not refuse concurrent mount')
                runner.send_signal(signal.SIGINT)
            elif args.mode in ('detach', 'replace'):
                action = os.environ['QOS_NTFS_NATIVE_ACTION']; require(pathlib.Path(action).is_absolute(), 'absolute authorized external action required')
                message = {'schema': 1, 'action': args.mode, 'phase': 'native-copy-second-pass-range', 'device': device, 'inventory': selected, 'broker_pid': session.process.pid, 'reader_pid': child}
                receipt = subprocess.run([action, args.mode], input=json.dumps(message).encode(), capture_output=True, timeout=5, check=True)
                require(json.loads(receipt.stdout) == {'schema': 1, 'completed': args.mode}, 'external action receipt')
                old = pathlib.Path(selected['sysfs'])
                changed = not old.exists() or old.stat().st_ino != selected['sysfs_inode'] or int((old.parent / 'diskseq').read_text()) != selected['disk_sequence']
                require(changed, 'no-op detach/replacement')
                if args.mode == 'replace':
                    require(any(c['name'] == selected['name'] and c['disk_sequence'] != selected['disk_sequence'] for c in inventory(args.probe)), 'replacement absent')
            if child:
                trace(17, child); child = None
            if args.mode != 'broker-death':
                trace(17, session.process.pid)
        output, errors = runner.communicate(timeout=45)
        require(not (workspace / 'output' / 'last.bin').exists(), 'unattested/cancelled native file published')
        if args.mode.startswith('cancel-') or args.mode == 'aliases':
            require(runner.returncode == 2 and result(output)['cancelled'], 'late cancellation not acknowledged: ' + repr(errors))
        else:
            require(runner.returncode != 0, 'terminal native failure became success')
        if args.mode != 'broker-death':
            session.closed()
        if args.mode not in ('detach', 'replace'):
            reacquire(device)
    finally:
        if child and not ended(child):
            os.kill(child, signal.SIGKILL)
        if runner and runner.poll() is None:
            runner.kill(); runner.wait(timeout=3)
        session.close()


def main():
    require(sys.platform == 'linux' and os.geteuid() == 0 and os.environ.get('QOS_NTFS_NATIVE_AUTHORIZATION') == 'DISPOSABLE_FIXTURE_ONLY', 'explicit disposable Linux authorization required; fixture remains UNRUN otherwise')
    parser = argparse.ArgumentParser()
    parser.add_argument('--probe', required=True); parser.add_argument('--imager', required=True)
    parser.add_argument('--output', required=True); parser.add_argument('--native-test')
    parser.add_argument('--baseline-root', type=int, default=5); parser.add_argument('--budget-root', type=int)
    parser.add_argument('--mode', choices=['positive', 'resume', 'cancel-final-range', 'cancel-postflight', 'client-death', 'broker-death', 'parser-death', 'detach', 'replace', 'aliases', 'crash-publication', 'crash-before-receipt', 'crash-after-receipt', 'budget'], required=True)
    args = parser.parse_args(); device = os.environ['QOS_NTFS_NATIVE_DEVICE']; uid = int(os.environ['QOS_NTFS_NATIVE_CLIENT_UID'])
    require(uid != 0, 'existing ordinary selected client required')
    workspace = pathlib.Path(args.output).resolve(); private_directory(workspace, uid); private_directory(workspace / 'journal', uid)
    selected, _ = select_device(args.probe, device); before = full_hash(device)
    entries = inspect(args.probe, selected, uid, device, root=args.baseline_root)
    baseline = run_copy(args.probe, args.imager, selected, uid, device, workspace, root=args.baseline_root)
    proof = verify_output(workspace, entries, baseline)
    if args.mode == 'resume':
        resumed = run_copy(args.probe, args.imager, selected, uid, device, workspace, resume=True, root=args.baseline_root)
        require(resumed['verified_bytes'] == baseline['verified_bytes'], 'fresh-selected resume lost verified prefix')
        verify_output(workspace, entries, resumed)
    elif args.mode != 'positive':
        if args.mode == 'aliases':
            aliases(args.probe, selected, uid, device)
        subtree = next(e for e in entries if e['path'] == [[ord(c) for c in 'cancel']])
        negative = workspace / 'negative'; private_directory(negative, uid); private_directory(negative / 'journal', uid)
        if args.mode == 'budget':
            require(args.budget_root is not None, 'budget mode requires separately prepared, explicitly selected bounded inventory with oversized or request-exhausting content')
            pending = run_copy(args.probe, args.imager, selected, uid, device, negative, root=args.budget_root)
            require(not pending['complete'] and any('budget' in message for _, message in pending['failed']), 'native session budget was not enforced')
        elif args.mode.startswith('crash-'):
            boundary = {'crash-publication': 'file_published', 'crash-before-receipt': 'native_before_file_receipt', 'crash-after-receipt': 'native_after_file_receipt'}[args.mode]
            run_copy(args.probe, args.imager, selected, uid, device, negative, root=subtree['record'], test_binary=args.native_test, boundary=boundary)
            require(digest_without_atime(negative / 'output' / 'last.bin') == hashlib.sha256(b'final chunk').hexdigest(), 'published crash prefix missing')
            resumed = run_copy(args.probe, args.imager, selected, uid, device, negative, resume=True, root=subtree['record'], conflict=args.mode != 'crash-after-receipt')
            require(resumed['complete'] if args.mode == 'crash-after-receipt' else resumed is None, 'receipt boundary recovery disagreement')
            require((negative / 'output' / 'last.bin').read_bytes() == b'final chunk', 'recovery overwrote conflict')
        else:
            held_copy(args, selected, uid, device, negative, subtree['record'])
    if args.mode not in ('detach', 'replace'):
        require(full_hash(device) == before, 'whole native source changed')
    print(json.dumps({'mode': args.mode, 'ordinary_copy': proof, 'source_sha256': before, 'accepted': True}))


if __name__ == '__main__':
    main()
