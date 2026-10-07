#!/usr/bin/env python3
"""Public, paced production-worker probe. Run in a private hardware-free namespace.

Uses the existing ephemeral qualification witness, never an installed trust key.
Only pinned public FLEURS fixture transcripts may appear in its output.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import math
import random
import selectors
import struct
import subprocess
import time
import threading
import wave

from probe_speech_memory import MODELS


def exact(pipe, count, deadline):
    output = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(pipe, selectors.EVENT_READ)
        while len(output) < count:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise TimeoutError("owned worker reply deadline")
            chunk = os.read(pipe.fileno(), count - len(output))
            if not chunk:
                raise EOFError("owned worker stopped")
            output.extend(chunk)
    return bytes(output)


def receive(pipe):
    deadline = time.monotonic() + 90
    size, = struct.unpack('<I', exact(pipe, 4, deadline))
    if not 0 < size <= 32768:
        raise ValueError("worker reply exceeds bound")
    return json.loads(exact(pipe, size, deadline))


def send(pipe, kind, sequence, payload=b''):
    body = bytes([kind]) + struct.pack('<Q', sequence) + payload
    if len(body) > 65545:
        raise ValueError("worker request exceeds bound")
    pipe.write(struct.pack('<I', len(body)) + body)
    pipe.flush()


def verified(root, receipt):
    name = receipt['file']
    if Path(name).name != name:
        raise ValueError("public fixture must be a plain filename")
    path = root / name
    if (path.stat().st_size != receipt['size']
            or hashlib.sha256(path.read_bytes()).hexdigest() != receipt['sha256']):
        raise ValueError("public fixture receipt mismatch")
    return path


def synthetic_noise(pcm, snr_db):
    """Deterministic white-noise stress, not a recording of a noisy environment."""
    samples = [sample[0] for sample in struct.iter_unpack('<h', pcm)]
    if not samples or snr_db not in (10, 20):
        raise ValueError('bounded nonempty PCM and reviewed SNR required')
    signal_rms = math.sqrt(sum(sample * sample for sample in samples) / len(samples))
    if signal_rms == 0:
        raise ValueError('cannot assign an SNR to silent input')
    generator = random.Random(0)
    noise = [generator.gauss(0, 1) for _ in samples]
    noise_rms = math.sqrt(sum(value * value for value in noise) / len(noise))
    scale = signal_rms / (10 ** (snr_db / 20) * noise_rms)
    noisy, clipped = [], 0
    actual_noise_power = 0
    for sample, value in zip(samples, noise):
        raw = round(sample + value * scale)
        changed = max(-32768, min(32767, raw))
        clipped += changed != raw
        actual_noise_power += (changed - sample) ** 2
        noisy.append(changed)
    actual_noise_rms = math.sqrt(actual_noise_power / len(samples))
    if actual_noise_rms == 0:
        raise ValueError('quantization made injected noise unobservable')
    return struct.pack('<' + 'h' * len(noisy), *noisy), {
        'kind': 'synthetic seeded Gaussian white noise; not real noisy speech',
        'seed': 0, 'requested_snr_db': snr_db,
        'achieved_snr_db': 20 * math.log10(signal_rms / actual_noise_rms),
        'clipped_samples': clipped,
    }


def worker_failure(reply):
    # Never copy an arbitrary peer message/transcript into an exception.
    errors = {'protocol', 'model_unavailable', 'model_receipt', 'unsupported_language',
              'audio_too_long', 'text_too_long', 'recognition', 'worker_stopped'}
    error = reply.get('error')
    return error if reply.get('kind') == 'error' and isinstance(error, str) and error in errors else 'unexpected_reply'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--qualifier', required=True, type=Path)
    parser.add_argument('--worker', required=True, type=Path)
    parser.add_argument('--model', choices=('tiny', 'base'), default='base')
    parser.add_argument('--case', required=True)
    parser.add_argument('--append-case', action='append', default=[])
    parser.add_argument('--language', choices=('auto', 'en', 'de', 'fr', 'es', 'ar', 'zh'))
    parser.add_argument('--vocabulary', action='append', default=[])
    parser.add_argument('--noise-snr-db', type=int, choices=(10, 20))
    parser.add_argument('--cpus', type=int, choices=(1, 2), default=2)
    parser.add_argument('--first-draft-seconds', type=int, choices=(1, 2, 3), default=1)
    args = parser.parse_args()
    if (os.geteuid() == 0 or os.environ.get('TILO_TEST_PRIVATE_NAMESPACE') != '1'
            or args.manifest.stat().st_size > 1048576):
        parser.error('requires an unprivileged, owned hardware-free namespace')
    manifest = json.loads(args.manifest.read_text())
    if (manifest.get('dataset') != 'google/fleurs'
            or manifest.get('revision') != '70bb2e84b976b7e960aa89f1c648e09c59f894dd'):
        parser.error('unexpected public corpus')
    model = manifest['models'][args.model]
    if model['sha256'] != MODELS[args.model]:
        parser.error('unexpected reviewed model')
    long_language = args.case.removeprefix('long-') if args.case.startswith('long-') else None
    requested = [args.case, *args.append_case]
    if len(requested) > 6 or len(set(requested)) != len(requested) or (long_language and args.append_case):
        parser.error('bounded unique public cases required; long fixtures cannot be appended')
    cases = ([case for case in manifest['cases'] if case['language'] == long_language]
             if long_language else [next((case for case in manifest['cases']
                                          if case['id'] == name), None) for name in requested])
    if any(case is None for case in cases):
        parser.error('pinned public case unavailable')
    if len(cases) != (8 if long_language else len(requested)):
        parser.error('pinned public case collection unavailable')
    model_path = verified(args.manifest.parent, model)
    chunks, references = [], []
    for case in cases:
        audio_path = verified(args.manifest.parent, case)
        with wave.open(str(audio_path)) as audio:
            if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
                parser.error('requires mono 16 kHz PCM16')
            chunk = audio.readframes(audio.getnframes())
        if long_language and sum(map(len, chunks)) + len(chunk) > 100 * 32000:
            break
        chunks.append(chunk)
        references.append(case['reference'])
    pcm = b''.join(chunks)
    if not 0 < len(pcm) <= 120 * 32000:
        parser.error('assembled public audio exceeds production session bound')
    original_pcm_sha256 = hashlib.sha256(pcm).hexdigest()
    noise = None
    if args.noise_snr_db is not None:
        pcm, noise = synthetic_noise(pcm, args.noise_snr_db)
    case = dict(cases[0], id='+'.join(requested), reference=' '.join(references))
    language = args.language or case['language']
    if len({case['language'] for case in cases}) > 1 and args.language != 'auto':
        parser.error('assembled language transitions require explicit auto recognition')
    public_words = set(case['reference'].split()) | {'TiLo'}
    if (len(args.vocabulary) > 256 or any(word not in public_words for word in args.vocabulary)
            or any(not word or len(word) > 128 or any(c.isspace() for c in word) for word in args.vocabulary)):
        parser.error('vocabulary probe accepts bounded public reference words or unspoken TiLo only')
    if not 0 < len(pcm) <= 120 * 32000 or (long_language and len(pcm) < 60 * 32000):
        parser.error('public audio exceeds production session bound or long fixture is too short')
    cpus = sorted(os.sched_getaffinity(0))[:args.cpus]
    if len(cpus) != args.cpus:
        parser.error('requested CPU affinity unavailable')
    os.sched_setaffinity(0, cpus)
    holder = subprocess.Popen([str(args.qualifier), '--hold-fixture', str(model_path)],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL)
    worker = None
    peak, memory_errors = {}, []
    finished = threading.Event()
    sampler = None
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(holder.stdout, selectors.EVENT_READ)
            if not selector.select(90):
                raise TimeoutError('ephemeral witness deadline')
        witness = json.loads(holder.stdout.readline(4096))
        if witness.get('ephemeral') is not True or witness['model_sha256'] != model['sha256']:
            raise ValueError('unexpected isolated witness')
        usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
        started = time.monotonic()
        worker = subprocess.Popen([str(args.worker), '--root', witness['root'],
                                   '--catalog', witness['catalog'], '--keys', witness['keys']],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL)
        def sample_memory():
            while not finished.is_set():
                try:
                    status = Path(f'/proc/{worker.pid}/status').read_text()
                    for line in status.splitlines():
                        name, _, value = line.partition(':')
                        if name in ('VmPeak', 'VmSize', 'VmRSS', 'VmHWM', 'Threads'):
                            peak[name] = max(peak.get(name, 0), int(value.split()[0]))
                except FileNotFoundError:
                    break
                except Exception as error:
                    memory_errors.append(type(error).__name__)
                    break
                finished.wait(.02)
        sampler = threading.Thread(target=sample_memory)
        sampler.start()
        sequence = 1
        send(worker.stdin, 0, sequence, json.dumps({'language': language, 'vocabulary': args.vocabulary}).encode())
        ready = receive(worker.stdout)
        if ready.get('kind') != 'ready' or ready.get('sequence') != sequence:
            raise RuntimeError('production worker did not become ready')
        cold_ms = (time.monotonic() - started) * 1000
        capture = time.monotonic()
        sent, first_partial, revisions, drafts = 0, None, 0, []
        previous = ''
        while sent < len(pcm):
            # Model backpressure advances the source clock; samples are retained
            # and never accelerated to make latency look better.
            interval = args.first_draft_seconds if sent == 0 else 1
            if time.monotonic() - capture < sent / 32000 + interval:
                time.sleep(max(0, sent / 32000 + interval - (time.monotonic() - capture)))
            available = min(len(pcm), int((time.monotonic() - capture) * 16000) * 2)
            while sent < available:
                end = min(available, sent + 64000)
                sequence += 1
                send(worker.stdin, 1, sequence, pcm[sent:end])
                sent = end
            sequence += 1
            inference = time.monotonic()
            send(worker.stdin, 2, sequence)
            draft = receive(worker.stdout)
            if draft.get('kind') != 'draft' or draft.get('sequence') != sequence:
                failure = worker_failure(draft)
                print('SPEECH_STREAM_FAILURE ' + json.dumps({
                    'case': case['id'], 'language': language, 'stage': 'draft',
                    'samples': sent // 2, 'worker_error': failure,
                    'public_vocabulary': args.vocabulary, 'synthetic_noise': noise,
                    'memory_peak': peak, 'model_sha256': model['sha256'],
                }), flush=True)
                raise RuntimeError('production draft failed: ' + failure)
            text = draft['text']
            if text and first_partial is None:
                first_partial = (time.monotonic() - capture) * 1000
            if previous and not text.startswith(previous):
                revisions += 1
            drafts.append({'samples': sent // 2, 'wall_ms': (time.monotonic()-capture)*1000,
                           'inference_ms': (time.monotonic()-inference)*1000, 'text': text})
            previous = text
        sequence += 1
        final_started = time.monotonic()
        send(worker.stdin, 3, sequence)
        final = receive(worker.stdout)
        final_ms = (time.monotonic() - final_started) * 1000
        if final.get('kind') != 'done' or final.get('sequence') != sequence:
            raise RuntimeError('production final failed')
        if worker.wait(timeout=10) != 0 or first_partial is None:
            raise RuntimeError('worker exit or visible partial missing')
        finished.set()
        sampler.join(timeout=2)
        if memory_errors or not all(name in peak for name in ('VmPeak', 'VmSize', 'VmRSS', 'VmHWM', 'Threads')):
            raise RuntimeError('could not observe production worker memory')
        usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        print('SPEECH_STREAM_PROBE ' + json.dumps({
            'case': case['id'], 'language': language,
            'public_sources': [{'id': case['id'], 'sha256': case['sha256']} for case in cases],
            'input_pcm_sha256': hashlib.sha256(pcm).hexdigest(),
            'original_pcm_sha256': original_pcm_sha256, 'synthetic_noise': noise,
            'assembled_language_transition': len({case['language'] for case in cases}) > 1,
            'public_vocabulary': args.vocabulary, 'model_sha256': model['sha256'], 'cpu_affinity_count': len(cpus),
            'profile': 'builder CPU-affinity constraint; not physical older hardware',
            'audio_seconds': len(pcm)/32000, 'cold_ms': cold_ms,
            'first_draft_seconds': args.first_draft_seconds,
            'first_partial_from_source_start_ms': first_partial,
            'non_prefix_partial_revisions': revisions, 'drafts': drafts,
            'finalization_ms': final_ms, 'final_text': final['text'],
            'cpu_user_seconds': usage_after.ru_utime-usage_before.ru_utime,
            'cpu_system_seconds': usage_after.ru_stime-usage_before.ru_stime,
            'wall_seconds': time.monotonic()-started, 'worker_exit': 0,
            'cpu_accounting': 'serial worker RUSAGE_CHILDREN delta; witness still running',
            'memory_limit_bytes': 1073741824, 'memory_peak': peak,
            'memory_unit': 'KiB except Threads', 'memory_sample_interval_ms': 20,
            'reference': case['reference'],
        }, ensure_ascii=False), flush=True)
    finally:
        if worker:
            if worker.poll() is None:
                worker.kill()
            worker.wait(timeout=10)
            worker.stdin.close()
            worker.stdout.close()
        finished.set()
        if sampler:
            sampler.join(timeout=2)
        holder.stdin.close()
        if holder.wait(timeout=10) != 0:
            raise RuntimeError('ephemeral witness cleanup failed')
        holder.stdout.close()


if __name__ == '__main__':
    main()
