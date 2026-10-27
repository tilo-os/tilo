#!/usr/bin/env python3
"""Source-only AOT qualification in the existing admitted scope.

Factory compilation is measured separately from real worker initialization and
recognition. Receipts belong to this private review directory, not the production
catalogue. Never feed an externally supplied compiled artifact to this harness.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import wave

import probe_asr_candidate as probe
import qualify_whistle_cli_scope as scope

PROJECT = Path(__file__).resolve().parents[2]
SOURCE_COMMIT = '3c8a3e79aa3188a1a12b3aebbaa097abccceac92'
MODULE_SHA = 'c43f48e11f302087250d1e406024956781cd2f02595a5e343b3d7ddd5ef707fa'
MODEL_SHA = 'b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb'

PEER = r'''
import json, os, select, signal, struct, subprocess, sys, time
from pathlib import Path
raw = open('/review/audio.s16', 'rb').read()
cancelling = sys.argv[4] in ('cancel-draft', 'cancel-finish')
command = ['/review/aot-worker', *(['--observe-calls'] if cancelling else []),
    '/review/needle.aot', sys.argv[1],
    '/review/whistle.cact', '/review/aot-bridge.so', sys.argv[2]]
if sys.argv[5] == 'trace':
    command = ['strace', '-f', '-qq', '-e', 'trace=mmap,mprotect,pkey_mprotect',
        '-o', '/trace/native.log', *command]
child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    stderr=subprocess.PIPE if cancelling else None, bufsize=0)
sequence = 0
beginning = time.monotonic()
def request(kind, payload=b''):
    global sequence
    sequence += 1
    body = bytes([kind]) + sequence.to_bytes(8, 'little') + payload
    child.stdin.write(len(body).to_bytes(4, 'little') + body)
    child.stdin.flush()
def reply():
    header = child.stdout.read(4)
    if len(header) != 4: raise RuntimeError('incomplete speech frame')
    length, = struct.unpack('<I', header)
    if not 0 < length <= 65545: raise RuntimeError('bounded speech frame')
    payload = child.stdout.read(length)
    if len(payload) != length: raise RuntimeError('incomplete speech reply')
    value = json.loads(payload)
    value['wall_seconds'] = time.monotonic() - beginning
    print('WHISTLE_AOT_WORKER_REPLY ' + json.dumps(value), flush=True)
def marker(expected):
    if not select.select([child.stderr], [], [], 5)[0]:
        raise RuntimeError('native call marker was not observed')
    if child.stderr.readline(128).decode().strip() != expected:
        raise RuntimeError('unexpected native call marker')
def cpu_ticks():
    fields = Path(f'/proc/{child.pid}/stat').read_text().rsplit(')', 1)[1].split()
    return int(fields[11]) + int(fields[12])
def cancel_native(operation):
    marker(f'WHISTLE_AOT_NATIVE_BEGIN {operation}')
    initial_ticks = cpu_ticks()
    deadline = time.monotonic() + 2
    while True:
        if child.poll() is not None or select.select([child.stdout, child.stderr], [], [], 0)[0]:
            raise RuntimeError('native call ended before observed cancellation')
        ticks = cpu_ticks() - initial_ticks
        if ticks >= 2:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError('native computation was not observed')
        time.sleep(.005)
    # Pin the exact owned process, never a name or reused PID. This review
    # measures native termination; compositor owner/reaping has separate tests.
    pidfd = os.pidfd_open(child.pid)
    try:
        started = time.monotonic()
        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
        exit_code = child.wait(timeout=1)
        elapsed = time.monotonic() - started
    finally:
        os.close(pidfd)
    child.stdin.close()
    trailing = child.stdout.read(65546)
    child.stdout.close()
    ended = child.stderr.read(1024)
    child.stderr.close()
    if exit_code != -signal.SIGKILL or trailing or ended or elapsed >= 1:
        raise RuntimeError('cancelled native call published output or did not reap')
    print('WHISTLE_AOT_CANCEL ' + json.dumps({'operation': operation,
        'native_cpu_ticks_observed': ticks, 'clock_ticks_per_second': os.sysconf('SC_CLK_TCK'),
        'kill_to_reaped_seconds': elapsed, 'worker_exit': exit_code,
        'trailing_reply_bytes': len(trailing), 'native_end_observed': False}), flush=True)
request(0, json.dumps({'language': sys.argv[3]}).encode())
reply()
beginning = time.monotonic()
if cancelling:
    # A real authenticated complete recording makes inference long enough to
    # observe. Each Audio frame remains inside the production framing bound.
    for offset in range(0, len(raw), 64000):
        request(1, raw[offset:offset + 64000])
    request(2)
    if sys.argv[4] == 'cancel-finish':
        marker('WHISTLE_AOT_NATIVE_BEGIN 0')
        marker('WHISTLE_AOT_NATIVE_END 0')
        reply()
        request(3)
        cancel_native(1)
    else:
        cancel_native(0)
    sys.exit(0)
offset, planned = 0, min(64000, len(raw))
while offset < len(raw):
    time.sleep(max(0, planned / 32000 - (time.monotonic() - beginning)))
    end = min(len(raw), max(planned, int((time.monotonic() - beginning) * 32000) // 2 * 2))
    request(1, raw[offset:end])
    request(2)
    reply()
    offset, planned = end, min(len(raw), end + 32000)
if sys.argv[4] != 'eof':
    request(3)
    reply()
child.stdin.close()
if child.stdout.read(1): raise RuntimeError('unexpected trailing speech reply')
child.stdout.close()
sys.exit(child.wait(timeout=5))
'''


def factory_identity(receipt):
    return {key: receipt[key] for key in ('source_commit', 'version', 'source_lock_sha256',
            'runtime_build', 'signals_and_spectre_defaults', 'target')} | {
        'files': {name: receipt['files'][name] for name in (
            'libwasmtime.so', 'aot-factory', 'whistle_aot_factory.cpp', 'whistle_aot_config.hpp')}}


def digest(path):
    if path.is_symlink() or not path.is_file():
        raise RuntimeError('owned regular review input required')
    metadata = path.stat()
    if metadata.st_uid != os.getuid() or metadata.st_mode & 0o022:
        raise RuntimeError('private review custody required')
    return {'bytes': metadata.st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--phase', choices=('compile', 'recognize', 'worker'), required=True)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--case', choices=scope.PUBLIC_CASES, default='fr_fr-0')
    parser.add_argument('--mode', choices=('complete', 'silence', 'eof',
                                         'cancel-draft', 'cancel-finish'), default='complete')
    parser.add_argument('--trace', action='store_true')
    args = parser.parse_args()
    cancelling = args.mode in ('cancel-draft', 'cancel-finish')
    if cancelling and (args.phase != 'worker' or args.trace):
        parser.error('native cancellation needs the direct, untraced worker')
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged public review required')
    cg = scope.admitted_scope()
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    root = args.runtime_dir.resolve(strict=True)
    if root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o022:
        raise RuntimeError('private qualification directory required')
    receipt = json.loads((root / 'source-build-receipt.json').read_text())
    if (receipt['source_commit'] != SOURCE_COMMIT or receipt['version'] != '49.0.2'
            or receipt['target'] != 'x86-64-v3' or receipt['installed']
            or not receipt['signals_and_spectre_defaults']):
        raise RuntimeError('pinned source-built engine configuration required')
    for name, entry in receipt['files'].items():
        if Path(name).name != name or digest(root / name) != entry:
            raise RuntimeError('source-built runtime receipt differs')
    for name in ('whistle_aot_config.hpp', 'whistle_aot_factory.cpp', 'whistle_aot_review.cpp'):
        if digest(PROJECT / 'tools/release/fixtures' / name) != receipt['files'][name]:
            raise RuntimeError('checkout differs from measured bridge source')
    for name, entry in receipt['rust_sources'].items():
        if hashlib.sha256((PROJECT / name).read_bytes()).hexdigest() != entry:
            raise RuntimeError('checkout differs from measured worker source')
    probe.receipt(root / 'needle.wasm', 923348, MODULE_SHA)
    probe.receipt(root / 'whistle.cact', 16919407, MODEL_SHA)
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review']
    inputs = ['libwasmtime.so', 'aot-factory', 'needle.wasm']
    if args.phase != 'compile':
        inputs += ['aot-bridge.so', 'aot-review', 'whistle.cact']
    for name in inputs:
        command += ['--ro-bind', str(root / name), '/review/' + name]
    tail = ['--clearenv', '--setenv', 'LANG', 'C.UTF-8', '--setenv',
            'LD_LIBRARY_PATH', '/review', '--chdir', '/', '/usr/bin/time', '-f',
            'ASR_RESOURCE {"user":%U,"system":%S}']
    if args.trace:
        if args.phase != 'worker':
            parser.error('trace the actual worker only; compilation timings stay separate')
        trace_dir = root / ('trace-' + args.case + '-' + args.mode)
        trace_dir.mkdir(mode=0o700, exist_ok=False)
        command += ['--bind', str(trace_dir), '/trace']
    if args.phase == 'compile':
        output = root / 'factory-output'
        output.mkdir(mode=0o700, exist_ok=False)
        command += ['--bind', str(output), '/output', *tail,
                    '/review/aot-factory', '/review/needle.wasm', '/output/needle.aot']
        executable = root / 'aot-factory'
    else:
        artifact = root / 'factory-output/needle.aot'
        custody = json.loads((root / 'aot-receipt.json').read_text())
        if (factory_identity(custody['source_build']) != factory_identity(receipt)
                or digest(artifact) != custody['artifact']):
            raise RuntimeError('AOT artifact lost factory custody')
        if not args.manifest:
            parser.error('recognition needs the pinned public manifest')
        fixture, language, sha, pcm_sha, seconds = scope.verified_fixture(args.manifest, args.case)
        with wave.open(str(fixture), 'rb') as audio:
            raw = audio.readframes(audio.getnframes())
        command += ['--ro-bind', str(artifact), '/review/needle.aot',
                    '--ro-bind', str(root / 'aot-worker'), '/review/aot-worker']
        if args.phase == 'worker':
            if args.mode == 'silence':
                raw = bytes(3 * 32000)
            elif args.mode == 'eof':
                raw = raw[:3 * 32000]
            pcm = root / (args.case + '-' + args.mode + '.s16')
            pcm.write_bytes(raw)
            peer = root / 'worker-peer.py'
            peer.write_text(PEER)
            executable = root / 'aot-worker'
            command += ['--ro-bind', str(pcm), '/review/audio.s16',
                        '--ro-bind', str(peer), '/review/peer.py', *tail,
                        'python3', '/review/peer.py', custody['artifact']['sha256'],
                        receipt['files']['aot-bridge.so']['sha256'], language, args.mode,
                        'trace' if args.trace else 'plain']
        else:
            samples = struct.unpack('<' + 'h' * (len(raw) // 2), raw)
            pcm = root / (args.case + '.f32')
            pcm.write_bytes(struct.pack('<' + 'f' * len(samples), *(x / 32768 for x in samples)))
            executable = root / 'aot-review'
            command += ['--ro-bind', str(pcm), '/review/audio.f32', *tail,
                        '/review/aot-review', '/review/needle.aot', custody['artifact']['sha256'],
                        '/review/whistle.cact', '/review/aot-bridge.so',
                        receipt['files']['aot-bridge.so']['sha256'], '/review/audio.f32',
                        digest(pcm)['sha256'], language]
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    identity = executable.stat()
    result = probe.observe(command, cpus, (identity.st_dev, identity.st_ino),
                           max_threads=2, capture_lines=True)
    after = events()
    result.update(phase=args.phase, case=args.case, mode=args.mode, trace=args.trace,
                  source_build=receipt, scope_before=before,
                  scope_after=after, scope_peak_bytes=int((cg / 'memory.peak').read_text()))
    if args.phase != 'compile':
        result.update(fixture_sha256=sha, fixture_pcm_sha256=pcm_sha,
                      submitted_pcm_sha256=hashlib.sha256(raw).hexdigest(),
                      original_seconds=seconds, submitted_seconds=len(raw) / 32000,
                      language=language, live_microphone=False)
    suffix = '-trace' if args.trace else ''
    (root / (args.phase + '-' + args.case + '-' + args.mode + suffix + '-result.json')).write_text(json.dumps(result, indent=2) + '\n')
    print('WHISTLE_AOT_SCOPE_RESULT ' + json.dumps(result), flush=True)
    expected_exit = 1 if args.phase == 'worker' and args.mode == 'eof' else 0
    if (result['exit'] != expected_exit or result['guarded'] or result['memory_peak'].get('Threads', 0) < 1
            or any(before[key] != after[key] for key in ('high', 'max', 'oom', 'oom_kill'))):
        raise RuntimeError('AOT qualification failed actual resource or completion observation')
    prefix = ('WHISTLE_AOT_WORKER_REPLY ' if args.phase == 'worker' else
              'WHISTLE_AOT_COMPILE ' if args.phase == 'compile' else 'WHISTLE_AOT_RECOGNITION ')
    rows = [json.loads(line.split(' ', 1)[1]) for line in result['stdout'].splitlines()
            if line.startswith(prefix)]
    if args.phase == 'worker':
        if cancelling:
            cancellations = [json.loads(line.split(' ', 1)[1])
                             for line in result['stdout'].splitlines()
                             if line.startswith('WHISTLE_AOT_CANCEL ')]
            operation = 0 if args.mode == 'cancel-draft' else 1
            if (len(cancellations) != 1 or cancellations[0]['operation'] != operation
                    or cancellations[0]['native_cpu_ticks_observed'] < 2
                    or cancellations[0]['worker_exit'] != -9
                    or cancellations[0]['trailing_reply_bytes']
                    or cancellations[0]['native_end_observed']
                    or cancellations[0]['kill_to_reaped_seconds'] >= 1):
                raise RuntimeError('actual native cancellation must be observed and reaped')
        if args.trace:
            calls = (trace_dir / 'native.log').read_text().splitlines()
            if any('PROT_WRITE' in line and 'PROT_EXEC' in line for line in calls):
                raise RuntimeError('runtime requested writable executable memory')
            if not any('mprotect(' in line and 'PROT_READ|PROT_EXEC' in line
                       and line.endswith('= 0') for line in calls):
                raise RuntimeError('must observe actual executable code publication')
        if not rows or rows[0]['kind'] != 'ready' or rows[0]['draft_policy'] != 'incremental':
            raise RuntimeError('actual prepared incremental speech worker required')
        if cancelling:
            if ([row['kind'] for row in rows] !=
                    (['ready'] if operation == 0 else ['ready', 'draft'])
                    or rows[0]['sequence'] != 1
                    or (operation == 1 and rows[-1]['sequence'] != 3 + (len(raw) - 1) // 64000)):
                raise RuntimeError('cancelled session emitted unexpected replies')
        elif [row['sequence'] for row in rows] != [1] + list(range(3, len(rows) * 2, 2)):
            if (args.mode == 'eof' or [row['sequence'] for row in rows[:-1]] !=
                    [1] + list(range(3, (len(rows) - 1) * 2, 2))
                    or rows[-1]['sequence'] != rows[-2]['sequence'] + 1):
                raise RuntimeError('ordered exact speech sequences required')
        done = [row for row in rows if row['kind'] == 'done']
        if args.mode == 'eof' or cancelling:
            if done:
                raise RuntimeError('unfinished session committed text')
        elif len(done) != 1 or (args.mode == 'silence' and done[0]['text']):
            raise RuntimeError('one final endpoint and empty silence required')
        return
    if len(rows) != 1:
        raise RuntimeError('exact observed result required')
    if args.phase == 'compile':
        artifact = digest(root / 'factory-output/needle.aot')
        if artifact['bytes'] > 32 * 1024 * 1024 or rows[0]['bytes'] != artifact['bytes']:
            raise RuntimeError('bounded compiled artifact required')
        (root / 'aot-receipt.json').write_text(json.dumps({
            'source_build': receipt, 'artifact': artifact, 'compile': rows[0],
            'review_only_not_catalogue_authority': True}, indent=2) + '\n')
    elif (not rows[0]['production_confinement']
          or not rows[0]['raw_result'].get('text')):
        raise RuntimeError('actual confined lexical recognition required')


if __name__ == '__main__':
    main()
