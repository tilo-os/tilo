#!/usr/bin/env python3
"""Serial, public-only local-engine experiment in a hardware-free namespace.

Uses explicit receipt-checked weights, no model catalogue or automatic download.
Hard address-space cap stays 1 GiB; terminate near 960 MiB to retain headroom.
This is a comparison adapter, not model release or physical-device acceptance.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import resource
import selectors
import signal
import subprocess
import time
import wave

from summarize_speech_benchmark import distance, units

ROOT = Path('/tmp/tilo-dictation/target')
MODELS = {
    'base': (ROOT/'speech-quality-public/base.bin', 147951465,
             '60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe'),
    'base-q8': (ROOT/'asr-comparison-weights-v1/base-q8_0.bin', 81768585,
                'c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9'),
    'small-q5': (ROOT/'asr-comparison-weights-v1/small-q5_1.bin', 190085487,
                 'ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb'),
    'small-q8': (ROOT/'asr-comparison-weights-v1/small-q8_0.bin', 264464607,
                 '49c8fb02b65e6049d5fa6c04f81f53b867b5ec9540406812c643f177317f779f'),
    'nemotron-q8': (ROOT/'asr-comparison-weights-v1/nemotron-q8.gguf', 742090464,
                    '3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1'),
}
LANGUAGES = {'en': 'en-US', 'de': 'de-DE', 'fr': 'fr-FR', 'es': 'es-US', 'ar': 'ar-AR', 'zh': 'zh-CN'}


def receipt(path, size, sha):
    if path.stat().st_size != size:
        raise ValueError('public experiment size receipt mismatch')
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        while chunk := stream.read(65536):
            digest.update(chunk)
    if digest.hexdigest() != sha:
        raise ValueError('public experiment hash receipt mismatch')


def subjects(pid, executable_identity=None):
    found, pending, visited = [], [pid], set()
    while pending and len(visited) < 32:
        current = pending.pop()
        if current in visited:
            continue
        visited.add(current)
        try:
            if executable_identity is None:
                name = Path(f'/proc/{current}/comm').read_text().strip()
                matched = name in ('nemo-speech', 'asr-whisper-com', 'asr-nemo-compar', 'tilo-speech-wor')
            else:
                # Thread names are mutable, including in upstream runtimes.
                # The caller pins one executable inode before its owned launch;
                # only that binary inside this descendant tree is observable.
                executable = Path(f'/proc/{current}/exe').stat()
                matched = (executable.st_dev, executable.st_ino) == executable_identity
            if matched:
                found.append(current)
            pending.extend(map(int, Path(f'/proc/{current}/task/{current}/children').read_text().split()))
        except (FileNotFoundError, ProcessLookupError):
            continue
    return found


def measured_cpu(stderr, successful):
    receipts = [json.loads(line.split(' ', 1)[1]) for line in stderr.splitlines()
                if line.startswith('ASR_RESOURCE ')]
    if len(receipts) != 1:
        if successful or receipts:
            raise RuntimeError('cannot observe unique native decoder CPU accounting')
        return None
    receipt = receipts[0]
    if (set(receipt) != {'user', 'system'} or any(
            not isinstance(value, (int, float)) or isinstance(value, bool)
            or not math.isfinite(value) or value < 0 for value in receipt.values())):
        raise ValueError('invalid native decoder CPU receipt')
    return receipt['user'] + receipt['system']


def observe(command, cpus, executable_identity=None, max_threads=None, capture_lines=False, on_line=None):
    if on_line is not None and (not capture_lines or not callable(on_line)):
        raise ValueError('live observation requires captured lines and a callable consumer')
    if max_threads is not None and (type(max_threads) is not int or max_threads < 1):
        raise ValueError('positive integer native thread limit required')
    def limits():
        resource.setrlimit(resource.RLIMIT_AS, (1073741824, 1073741824))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        os.sched_setaffinity(0, cpus)
    start = time.monotonic()
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             preexec_fn=limits, start_new_session=True)
    buffers = {'stdout': bytearray(), 'stderr': bytearray()}
    stdout_events, line_tail = [], bytearray()
    peaks, guarded, sampled_cpu = {}, None, 0
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ, 'stdout')
            selector.register(child.stderr, selectors.EVENT_READ, 'stderr')
            while selector.get_map():
                if time.monotonic() - start > 180:
                    guarded = 'deadline'
                for pid in subjects(child.pid, executable_identity):
                    try:
                        status = Path(f'/proc/{pid}/status').read_text()
                    except (FileNotFoundError, ProcessLookupError):
                        continue
                    try:
                        fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
                        sampled_cpu = max(sampled_cpu, (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'))
                    except (FileNotFoundError, ProcessLookupError):
                        pass
                    for line in status.splitlines():
                        key, _, value = line.partition(':')
                        if key in ('VmPeak', 'VmSize', 'VmHWM', 'VmRSS', 'Threads'):
                            peaks[key] = max(peaks.get(key, 0), int(value.split()[0]))
                    if peaks.get('VmSize', 0) >= 960 * 1024:
                        guarded = 'address_space_headroom'
                    if max_threads is not None and peaks.get('Threads', 0) > max_threads:
                        guarded = 'native_thread_limit'
                if guarded and child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                for key, _ in selector.select(.02):
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                    else:
                        buffers[key.data].extend(data)
                        if capture_lines and key.data == 'stdout':
                            line_tail.extend(data)
                            while b'\n' in line_tail:
                                if len(stdout_events) >= 4096:
                                    guarded = 'bounded_output'
                                    line_tail.clear()
                                    break
                                line, _, remaining = line_tail.partition(b'\n')
                                line_tail = bytearray(remaining)
                                event = {'wall_seconds': time.monotonic()-start,
                                         'line': line.decode()}
                                stdout_events.append(event)
                                if on_line is not None:
                                    on_line(event)
                        if len(buffers[key.data]) > 1024 * 1024:
                            guarded = 'bounded_output'
                if child.poll() is not None and not selector.get_map():
                    break
        code = child.wait(timeout=5)
        if not all(key in peaks for key in ('VmPeak', 'VmSize', 'VmHWM', 'VmRSS', 'Threads')):
            raise RuntimeError('cannot observe actual decoder process memory')
        cpu = measured_cpu(buffers['stderr'].decode(), code == 0)
        result = dict(exit=code, guarded=guarded, memory_peak=peaks,
                    cpu_seconds=cpu, sampled_cpu_lower_bound_seconds=sampled_cpu,
                    wall_seconds=time.monotonic()-start,
                    stdout=buffers['stdout'].decode(), stderr=buffers['stderr'].decode())
        if capture_lines:
            result['stdout_events'] = stdout_events
        return result
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
        child.stdout.close()
        child.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model', choices=MODELS, required=True)
    parser.add_argument('--case', required=True)
    parser.add_argument('--mode', choices=('offline', 'paced'), default='offline')
    parser.add_argument('--nemo-api', choices=('cli', 'native'), default='cli')
    parser.add_argument('--allocator', choices=('default', 'arena-1'), default='default')
    parser.add_argument('--cpus', type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    suite_path = ROOT/'speech-evaluation-suite-v1/suite.json'
    if suite_path.stat().st_size > 1024 * 1024:
        parser.error('bounded public suite required')
    suite = json.loads(suite_path.read_text())
    content = dict(suite)
    stated = content.pop('suite_sha256')
    observed = hashlib.sha256(json.dumps(content, sort_keys=True, ensure_ascii=False,
                                        separators=(',', ':')).encode()).hexdigest()
    if observed != stated or stated != '9995db50a190bcd818373886e06c404c9d9009cbb85a76cf166dd65daf8ef8fd':
        parser.error('exact frozen public suite receipt required')
    matches = [case for case in suite['cases'] if case['id'] == args.case and case['split'] == 'development']
    if len(matches) != 1:
        parser.error('explicit development case required; holdout stays reserved')
    case = matches[0]
    if Path(case['file']).name != case['file']:
        parser.error('plain public fixture filename required')
    audio = suite_path.parent/case['file']
    receipt(audio, case['size'], case['sha256'])
    with wave.open(str(audio)) as source:
        if (source.getnchannels(), source.getsampwidth(), source.getframerate()) != (1, 2, 16000):
            parser.error('canonical public WAV required')
    model, size, sha = MODELS[args.model]
    receipt(model, size, sha)
    cpus = sorted(os.sched_getaffinity(0))[:args.cpus]
    if case['vocabulary']:
        parser.error('this adapter does not yet implement vocabulary; never silently ignore hints')
    if len(cpus) != args.cpus:
        parser.error('CPU affinity unavailable')
    nemo = args.model == 'nemotron-q8'
    if nemo and args.nemo_api == 'cli' and args.mode == 'paced':
        parser.error('CLI file streaming is unpaced; native callback adapter required')
    binary = ((ROOT/'nemo-asr-review-v1/source/build/cpu-asr/bin/nemo-speech' if args.nemo_api == 'cli'
               else ROOT/'asr-nemo-compare-v2/asr-nemo-compare') if nemo else
              ROOT/'asr-whisper-compare-v1/asr-whisper-compare')
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review',
               '--ro-bind', str(audio), '/review/audio.wav', '--ro-bind', str(model), '/review/model.bin',
               '--clearenv', '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'OMP_NUM_THREADS', str(args.cpus),
               '--uid', '65534', '--gid', '65534', '--chdir', '/']
    if args.allocator == 'arena-1':
        command += ['--setenv', 'MALLOC_ARENA_MAX', '1']
    if nemo and args.nemo_api == 'cli':
        executable = '/review/bin/nemo-speech'
        command += ['--ro-bind', str(binary.parent), '/review/bin',
                    '--setenv', 'LD_LIBRARY_PATH', '/review/bin', '/review/bin/nemo-speech',
                    'transcribe', '/review/audio.wav', '--model', '/review/model.bin', '--device', 'cpu',
                    '--asr.backend.threads', str(args.cpus), '--language', LANGUAGES.get(case['language'], ''),
                    '--no-warmup', '--no-batching', '--verbatim', '--endpointing=false', '--format', 'json']
    elif nemo:
        executable = '/review/asr-nemo-compare'
        command += ['--ro-bind', str(ROOT/'nemo-asr-review-v1/source/build/cpu-asr/bin'), '/review/bin',
                    '--setenv', 'LD_LIBRARY_PATH', '/review/bin',
                    '--ro-bind', str(binary), executable, executable,
                    '/review/model.bin', '/review/audio.wav', LANGUAGES.get(case['language'], 'auto'),
                    str(args.cpus), args.mode]
    else:
        executable = '/review/asr-whisper-compare'
        command += ['--ro-bind', str(binary), '/review/asr-whisper-compare', '/review/asr-whisper-compare',
                    '/review/model.bin', '/review/audio.wav', case['language'], str(args.cpus), args.mode]
    # Measure the native child inside its PID namespace, not bwrap's wrapper.
    position = len(command) - 1 - command[::-1].index(executable)
    command[position:position] = ['/usr/bin/time', '-f',
        'ASR_RESOURCE {"user":%U,"system":%S}']
    result = observe(command, cpus)
    result.update(nemo_api=args.nemo_api if nemo else None, allocator=args.allocator, cpu_accounting='GNU time waits for the native child inside the private PID namespace; guard kills report null plus sampled lower bound',
                  model=args.model, model_sha256=sha, case=case['id'], mode=args.mode,
                  suite_sha256=suite['suite_sha256'], input_pcm_sha256=case['input_pcm_sha256'],
                  memory_limit_bytes=1073741824, headroom_guard_bytes=960*1024*1024,
                  cpu_affinity_count=len(cpus), profile='remote builder affinity; not physical older hardware',
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
    if result['exit'] == 0 and not result['guarded']:
        if nemo and args.nemo_api == 'cli':
            text = json.loads(result['stdout'])['text']
        else:
            final = [json.loads(line.split(' ',1)[1]) for line in result['stdout'].splitlines()
                     if line.startswith('ASR_FINAL ')]
            if len(final) != 1:
                raise RuntimeError('missing decoder final')
            text = final[0]['text']
        characters = case['language'] == 'zh'
        reference = units(case['reference'], characters)
        result.update(final_text=text, error_metric='CER' if characters else 'WER',
                      error_fraction=distance(reference, units(text, characters))/len(reference))
    print('ASR_CANDIDATE_PROBE '+json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
