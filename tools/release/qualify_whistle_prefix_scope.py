#!/usr/bin/env python3
"""Source-paced, prerecorded public prefixes through the pinned official CLI.

Only audio already elapsed on the source clock reaches each process. The public
file CLI reloads the model and resets its stream for every prefix: these are
preview drafts, never a qualified resident C API, microphone or insertion path.
Use the unchanged speech transient-service controls. No downloader or wrapper.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
import unicodedata
import wave

import probe_asr_candidate as probe
import qualify_whistle_cli_scope as scope
from summarize_speech_benchmark import distance, units


def words(text):
    return any(unicodedata.category(char)[0] in 'LN' for char in text)


def command_for(root, prefix, language, options):
    mode, name_cues, threads = options
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review']
    for name in ['needle', 'whistle.cact', 'model-LICENSE', 'runtime-LICENSE']:
        command.extend(['--ro-bind', str(root / name), '/review/' + name])
    if name_cues:
        command += ['--ro-bind', str(prefix.parent / 'keywords.txt'), '/keywords.txt']
    command += ['--ro-bind', str(prefix), '/audio.wav', '--clearenv', '--setenv',
                      'LANG', 'C.UTF-8', '--setenv', 'NEEDLE_TELEMETRY', '0', '--setenv',
                      'DO_NOT_TRACK', '1', '--chdir', '/', '/usr/bin/time', '-f',
                      'ASR_RESOURCE {"user":%U,"system":%S}', '/review/needle', '--model',
                      '/review/whistle.cact', '--audio', '/audio.wav', '--audio-language',
                      language, '--threads', str(threads)]
    if mode == 'replay':
        command += ['--audio-stream', '--audio-chunk', '2']
    if name_cues:
        command += ['--audio-keywords', '/keywords.txt']
    return command


def run_prefixes(root, fixture, language, directory, options):
    mode, name_cues, _ = options
    if name_cues:
        (directory / 'keywords.txt').write_text('Hongkong\nKowloon\n')
    with wave.open(str(fixture), 'rb') as source:
        pcm = source.readframes(source.getnframes())
    frames = len(pcm) // 2
    executable = (root / 'needle').stat()
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    start = time.monotonic()
    offset, planned, first_words, records = 0, 32000, None, []
    while offset < frames:
        planned = min(planned, frames)
        time.sleep(max(0, start + planned / 16000 - time.monotonic()))
        end = min(frames, max(planned, int((time.monotonic() - start) * 16000)))
        prefix = directory / f'prefix-{end}.wav'
        with wave.open(str(prefix), 'wb') as audio:
            audio.setparams((1, 2, 16000, end, 'NONE', 'not compressed'))
            audio.writeframes(pcm[:end * 2])
        launch = time.monotonic() - start
        result = probe.observe(command_for(root, prefix, language, options), cpus,
                               (executable.st_dev, executable.st_ino), max_threads=2,
                               capture_lines=True)
        prefix.unlink()
        result['source_audio_seconds'] = end / 16000
        result['launch_source_clock_seconds'] = launch
        if result['exit'] or result['guarded']:
            raise RuntimeError('prefix decoder failed the unchanged envelope')
        final = []
        for event in result['stdout_events']:
            reply = json.loads(event['line'])
            if not isinstance(reply.get('text'), str):
                raise RuntimeError('unexpected official CLI reply')
            final.append(reply['text'])
            if first_words is None and words(reply['text']):
                first_words = launch + event['wall_seconds']
        result['draft'] = ' '.join(text for text in final if text)
        records.append(result)
        offset, planned = end, end + 16000
    completed = time.monotonic() - start
    return {'first_actual_words_seconds': first_words,
            'completion_source_clock_seconds': completed,
            'completion_after_audio_end_seconds': completed - frames / 16000,
            'audio_seconds': frames / 16000,
            'final_text': records[-1]['draft'], 'prefixes': records}


def correct_prefix_time(records, reference):
    """Post-hoc exposed-fixture scoring only; never decoder or display policy."""
    expected = units(reference)[:2]
    for row in records:
        accumulated = []
        for event in row['stdout_events']:
            accumulated += units(json.loads(event['line'])['text'])
            if accumulated[:2] == expected:
                return row['launch_source_clock_seconds'] + event['wall_seconds']
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--case', choices=scope.PUBLIC_CASES, required=True)
    parser.add_argument('--mode', choices=['replay', 'single-pass'], required=True)
    parser.add_argument('--public-name-cues', action='store_true')
    parser.add_argument('--decoder-threads', type=int, choices=[1, 2], default=2)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged public fixture review only')
    if args.public_name_cues and args.case != 'de_de-0':
        parser.error('only the two already exposed German fixture names are admitted')
    cg = scope.admitted_scope()
    root = args.runtime_dir.resolve(strict=True)
    ledger_path = scope.PROJECT / 'docs/architecture/dictation-whistle-review.json'
    receipts = scope.verified_runtime(root, json.loads(ledger_path.read_text()))
    fixture, language, sha, pcm_sha, seconds = scope.verified_fixture(args.manifest, args.case)
    manifest = json.loads(args.manifest.read_text())
    reference = next(row['reference'] for row in manifest['cases'] if row['id'] == args.case)
    args.out.mkdir(exist_ok=False)
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    result = run_prefixes(root, fixture, language, args.out,
                          (args.mode, args.public_name_cues, args.decoder_threads))
    after = events()
    result.update(case=args.case, language=language, mode=args.mode,
                  decoder_threads=args.decoder_threads, native_thread_limit=2,
                  public_name_cues=args.public_name_cues, fixture_sha256=sha,
                  input_pcm_sha256=pcm_sha, materialization=receipts,
                  reference_words=len(units(reference)),
                  final_raw_word_errors=distance(units(reference), units(result['final_text'])),
                  first_correct_two_token_prefix_seconds=correct_prefix_time(result['prefixes'], reference),
                  scope_events_before=before, scope_events_after=after,
                  scope_peak_bytes=int((cg / 'memory.peak').read_text()),
                  scope_script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  observer_sha256=hashlib.sha256(Path(probe.__file__).read_bytes()).hexdigest(),
                  interpretation='Source-paced public prefix CLI drafts; model reload and stream reset per prefix; not resident/live C API or microphone qualification')
    if abs(result['audio_seconds'] - seconds) > 1 / 16000:
        raise RuntimeError('fixture duration changed')
    if args.case == 'de_de-0':
        actual = units(result['final_text'])
        result['literal_name_accuracy'] = {name: name.casefold() in actual
                                           for name in ['Hongkong', 'Kowloon']}
    (args.out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    summary = {key: value for key, value in result.items() if key not in ['final_text', 'prefixes']}
    summary['native_memory_peak'] = {key: max(row['memory_peak'][key] for row in result['prefixes'])
                                      for key in ['VmPeak', 'VmSize', 'VmHWM', 'VmRSS', 'Threads']}
    summary['cpu_seconds'] = sum(row['cpu_seconds'] for row in result['prefixes'])
    summary['prefix_calls'] = len(result['prefixes'])
    print('WHISTLE_PREFIX_SCOPE_RESULT ' + json.dumps(summary), flush=True)
    if any(before[key] != after[key] for key in ['high', 'max', 'oom', 'oom_kill']):
        raise RuntimeError('scope memory event changed')


if __name__ == '__main__':
    main()
