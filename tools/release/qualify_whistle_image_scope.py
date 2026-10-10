#!/usr/bin/env python3
"""Qualify the actual default-off image handler with ephemeral signed weights.

Mapped namespace root owns only the private synthetic image; host root, real
model stores, microphone, user home, credentials and network are absent.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import wave

import probe_asr_candidate as probe
import qualify_whistle_aot_scope as aot
import qualify_whistle_cli_scope as scope

PEER = aot.PEER.replace(
    "command = ['/review/aot-worker', *(['--observe-calls'] if cancelling else []),\n"
    "    '/review/needle.aot', sys.argv[1],\n"
    "    '/review/whistle.cact', '/review/aot-bridge.so', sys.argv[2]]",
    "command = ['/review/image-worker']",
).replace(
    '    request(1, raw[offset:end])',
    '    for frame in range(offset, end, 64000):\n'
    '        request(1, raw[frame:min(frame + 64000, end)])',
).replace(
    '    value = json.loads(payload)',
    "    value = json.loads(payload)\n"
    "    if value['sequence'] != sequence: raise RuntimeError('wrong speech sequence')",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--review-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--case', choices=scope.PUBLIC_CASES, default='fr_fr-0')
    parser.add_argument('--mode', choices=('complete', 'silence', 'eof', 'reject-image'), default='complete')
    parser.add_argument('--repeat-audio', type=int, choices=(1, 4, 11), default=1)
    parser.add_argument('--pad-to-session-limit', action='store_true',
                        help='Append silence to a complete public recording to exercise exactly 120 seconds')
    parser.add_argument('--stream-replies', action='store_true',
                        help='Forward actual bounded worker replies for concurrent synthetic app qualification')
    parser.add_argument('--automatic-language', action='store_true',
                        help='Exercise the default automatic language setting rather than the fixture language')
    args = parser.parse_args()
    if os.getuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged host preparation required')
    cg = scope.admitted_scope()
    root = args.review_dir.resolve(strict=True)
    receipt = json.loads((root / 'receipt.json').read_text())
    for name, pin in receipt['files'].items():
        path = root / name
        if (path.is_symlink() or not path.resolve().is_relative_to(root)
                or aot.digest(path) != pin):
            raise RuntimeError('private source/executable receipt differs')
    for name, digest in receipt['sources'].items():
        if hashlib.sha256((root / 'source' / name).read_bytes()).hexdigest() != digest:
            raise RuntimeError('review source differs from compiled witness')
    fixture, language, sha, pcm_sha, seconds = scope.verified_fixture(args.manifest, args.case)
    requested_language = 'auto' if args.automatic_language else language
    with wave.open(str(fixture), 'rb') as audio:
        raw = audio.readframes(audio.getnframes()) * args.repeat_audio
    if args.mode == 'silence':
        raw = bytes(3 * 32000)
    elif args.mode == 'eof':
        raw = raw[:3 * 32000]
    unpadded_bytes = len(raw)
    if args.pad_to_session_limit:
        if args.mode != 'complete' or not 0 < len(raw) < 120 * 32000:
            raise RuntimeError('padding requires a complete recording shorter than the session cap')
        raw += bytes(120 * 32000 - len(raw))
    if not 0 < len(raw) <= 120 * 32000:
        raise RuntimeError('production session bound required')
    stem = args.case + '-' + args.mode + '-' + str(args.repeat_audio)
    if args.pad_to_session_limit:
        stem += '-120s'
    if args.automatic_language:
        stem += '-auto'
    result_path = root / (stem + '-result.json')
    if result_path.exists():
        raise RuntimeError('fresh qualification output required')
    pcm = root / (stem + '.s16')
    pcm.write_bytes(raw)
    peer = root / 'peer.py'
    peer.write_text(PEER)
    command = ['bwrap', '--unshare-all', '--uid', '0', '--gid', '0', '--die-with-parent',
               '--dir', '/base', '--ro-bind', '/usr', '/base/usr',
               '--ro-bind', str(root / 'usr'), '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--symlink', 'usr/bin', '/bin', '--proc', '/proc', '--dev', '/dev',
               '--tmpfs', '/tmp', '--dir', '/review',
               '--ro-bind', str(root / 'image-worker'), '/review/image-worker',
               '--ro-bind', str(root / 'whistle.cact'), '/review/whistle.cact',
               '--ro-bind', str(pcm), '/review/audio.s16',
               '--ro-bind', str(peer), '/review/peer.py', '--clearenv',
               '--setenv', 'LANG', 'C.UTF-8', '--chdir', '/',
               '/usr/bin/time', '-f', 'ASR_RESOURCE {"user":%U,"system":%S}',
               '/usr/bin/python3', '/review/peer.py', 'unused', 'unused',
               requested_language, args.mode, 'plain']
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    identity = (root / 'image-worker').stat()
    def forward(event):
        if args.stream_replies and event['line'].startswith('WHISTLE_AOT_WORKER_REPLY '):
            print(event['line'], flush=True)
    result = probe.observe(command, cpus, (identity.st_dev, identity.st_ino),
                           max_threads=2, capture_lines=True, on_line=forward)
    result.update(case=args.case, mode=args.mode, repeat_audio=args.repeat_audio,
                  fixture_language=language, requested_language=requested_language,
                  appended_silence_seconds=(len(raw) - unpadded_bytes) / 32000,
                  fixture_sha256=sha, fixture_pcm_sha256=pcm_sha,
                  submitted_pcm_sha256=hashlib.sha256(raw).hexdigest(),
                  submitted_seconds=len(raw) / 32000, original_seconds=seconds,
                  source_receipt=receipt, scope_before=before, scope_after=events(),
                  live_microphone=False, installed=False)
    result_path.write_text(json.dumps(result, indent=2) + '\n')
    print('WHISTLE_IMAGE_RESULT ' + json.dumps(result), flush=True)
    rejecting = args.mode == 'reject-image'
    if (result['exit'] != (1 if args.mode == 'eof' or rejecting else 0) or result['guarded']
            or result['memory_peak'].get('Threads', 0) < 1
            or any(before[k] != result['scope_after'][k] for k in ('high', 'max', 'oom', 'oom_kill'))):
        raise RuntimeError('actual image handler failed qualification')
    rows = [json.loads(line.split(' ', 1)[1]) for line in result['stdout'].splitlines()
            if line.startswith('WHISTLE_AOT_WORKER_REPLY ')]
    if rejecting:
        if rows:
            raise RuntimeError('unadmitted image runtime became ready or emitted text')
        return
    if not rows or rows[0]['kind'] != 'ready' or rows[0].get('draft_policy', 'full_context') != 'full_context':
        raise RuntimeError('prepared full-context production handler required')
    done = [row for row in rows if row['kind'] == 'done']
    if (args.mode == 'eof' and done) or (args.mode != 'eof' and len(done) != 1):
        raise RuntimeError('exact final lifecycle required')
    if args.mode == 'silence' and any(row.get('text') for row in rows):
        raise RuntimeError('silence produced text')


if __name__ == '__main__':
    main()
