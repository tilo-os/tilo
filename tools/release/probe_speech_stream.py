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

# Review-only pinned MIT weight; never a production catalogue/default change.
REVIEW_MODELS = dict(MODELS, **{
    'base-q8_0': 'c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9',
})


def has_lexical_text(text):
    """Punctuation-only replies are visible but contain no recognized words."""
    return any(character.isalnum() for character in text)


def preparation_diagnostics(data, whisper=False):
    prefix = b'SPEECH_PREPARATION ' if whisper else b'NATIVE_PREPARATION '
    if len(data) > 4096 or not data.startswith(prefix):
        raise RuntimeError('bounded preparation counters absent')
    result = json.loads(data[len(prefix):])
    expected = {'verify_ms', 'native_ms'}
    if whisper:
        expected.add('flash_attention')
    if not isinstance(result, dict) or set(result) != expected:
        raise RuntimeError('invalid preparation counters')
    for name in ('verify_ms', 'native_ms'):
        value = result[name]
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise RuntimeError('invalid preparation counters')
    if whisper and type(result['flash_attention']) is not bool:
        raise RuntimeError('invalid native attention observation')
    return result


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



class PacedAudio:
    """Public PCM delivered every20ms while the actual decoder is busy.

    One lock preserves production frame ordering. Pipe backpressure bounds queued
    bytes; the original <=120s PCM is retained rather than dropped/accelerated.
    """
    def __init__(self, pipe, pcm, capture):
        self.pipe, self.pcm, self.capture = pipe, pcm, capture
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.sequence, self.sent = 1, 0
        self.first_started_ms = self.first_finished_ms = None
        self.last_finished_ms = None
        self.error = None
        self.thread = threading.Thread(target=self.feed, daemon=True)

    def feed(self):
        try:
            while self.sent < len(self.pcm) and not self.stop.is_set():
                due = self.capture + min(len(self.pcm), self.sent + 640) / 32000
                if self.stop.wait(max(0, due - time.monotonic())):
                    return
                with self.lock:
                    end = min(len(self.pcm), self.sent + 640)
                    self.sequence += 1
                    started = (time.monotonic() - self.capture) * 1000
                    send(self.pipe, 1, self.sequence, self.pcm[self.sent:end])
                    finished = (time.monotonic() - self.capture) * 1000
                    if self.first_started_ms is None:
                        self.first_started_ms, self.first_finished_ms = started, finished
                    self.last_finished_ms = finished
                    self.sent = end
        except Exception as error:
            self.error = type(error).__name__
            self.stop.set()

    def control(self, kind):
        with self.lock:
            if self.error is not None:
                raise RuntimeError('owned public PCM transport failed')
            self.sequence += 1
            send(self.pipe, kind, self.sequence)
            return self.sequence, self.sent, self.last_finished_ms


def worker_failure(reply):
    # Never copy an arbitrary peer message/transcript into an exception.
    errors = {'protocol', 'model_unavailable', 'model_receipt', 'unsupported_language',
              'audio_too_long', 'text_too_long', 'recognition', 'worker_stopped'}
    error = reply.get('error')
    return error if reply.get('kind') == 'error' and isinstance(error, str) and error in errors else 'unexpected_reply'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--qualifier', type=Path)
    parser.add_argument('--native-review-model', type=Path,
                        help='explicit isolated Nemotron worker fixture; never installed catalogue authority')
    parser.add_argument('--worker', required=True, type=Path)
    parser.add_argument('--model', choices=tuple(REVIEW_MODELS), default='base')
    parser.add_argument('--case', required=True)
    parser.add_argument('--append-case', action='append', default=[])
    parser.add_argument('--language', choices=('auto', 'en', 'de', 'fr', 'es', 'ar', 'zh'))
    parser.add_argument('--vocabulary', action='append', default=[])
    parser.add_argument('--noise-snr-db', type=int, choices=(10, 20))
    parser.add_argument('--continuous-audio', action='store_true',
                        help='deliver public PCM every20ms while inference runs; native phase review only')
    parser.add_argument('--worker-cadence', action='store_true')
    parser.add_argument('--worker-diagnostics', action='store_true')
    parser.add_argument('--expect-attention', choices=('flash', 'standard'))
    parser.add_argument('--native-right-context', type=int, choices=(0, 1, 3), default=1)
    parser.add_argument('--first-draft-ms', type=int, choices=(500, 1000, 2000))
    parser.add_argument('--draft-interval-ms', type=int, choices=(250, 500, 1000), default=1000)
    parser.add_argument('--cpus', type=int, choices=(1, 2), default=2)
    parser.add_argument('--first-draft-seconds', type=int, choices=(1, 2, 3), default=1)
    args = parser.parse_args()
    if (os.geteuid() == 0 or os.environ.get('TILO_TEST_PRIVATE_NAMESPACE') != '1'
            or args.manifest.stat().st_size > 1048576):
        parser.error('requires an unprivileged, owned hardware-free namespace')
    if args.continuous_audio and args.native_review_model is None:
        parser.error('continuous public PCM requires explicit native review')
    if args.worker_diagnostics and args.native_review_model is not None:
        parser.error('Whisper diagnostics exclude the native review backend')
    if args.expect_attention and not args.worker_diagnostics:
        parser.error('attention expectation requires actual worker diagnostics')
    if (args.worker_cadence or args.first_draft_ms is not None or args.draft_interval_ms != 1000 or args.native_right_context != 1) and (not args.continuous_audio or args.native_review_model is None):
        parser.error('alternate cadence/context requires explicit continuous native review')
    first_draft_seconds = args.first_draft_ms / 1000 if args.first_draft_ms is not None else args.first_draft_seconds
    draft_interval_seconds = args.draft_interval_ms / 1000
    manifest = json.loads(args.manifest.read_text())
    if (manifest.get('dataset') != 'google/fleurs'
            or manifest.get('revision') != '70bb2e84b976b7e960aa89f1c648e09c59f894dd'):
        parser.error('unexpected public corpus')
    if args.native_review_model is not None:
        model = {'sha256': '3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1'}
        if args.qualifier is not None or args.model != 'base':
            parser.error('native review fixture excludes Whisper model/witness options')
        if args.native_review_model.stat().st_size != 742090464:
            parser.error('exact native review object size required')
    else:
        if args.qualifier is None:
            parser.error('production Whisper probe requires an ephemeral qualifier')
        model = manifest['models'][args.model]
        if model['sha256'] != REVIEW_MODELS[args.model]:
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
    model_path = args.native_review_model or verified(args.manifest.parent, model)
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
    holder = (None if args.native_review_model is not None else
              subprocess.Popen([str(args.qualifier), '--hold-fixture', str(model_path)],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL))
    worker = None
    peak, memory_errors = {}, []
    finished = threading.Event()
    sampler = None
    source = None
    try:
        if holder is not None:
            with selectors.DefaultSelector() as selector:
                selector.register(holder.stdout, selectors.EVENT_READ)
                if not selector.select(90):
                    raise TimeoutError('ephemeral witness deadline')
            witness = json.loads(holder.stdout.readline(4096))
            if witness.get('ephemeral') is not True or witness['model_sha256'] != model['sha256']:
                raise ValueError('unexpected isolated witness')
            worker_command = [str(args.worker), '--root', witness['root'],
                              '--catalog', witness['catalog'], '--keys', witness['keys']]
            if args.worker_diagnostics:
                worker_command.append('--diagnostics')
        else:
            worker_command = [str(args.worker), '--model', str(model_path)]
            if args.continuous_audio:
                worker_command.extend(['--phase-review', '--review-right-context', str(args.native_right_context)])
        usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
        started = time.monotonic()
        worker = subprocess.Popen(worker_command, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE if args.continuous_audio or args.worker_diagnostics else subprocess.DEVNULL)
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
            raise RuntimeError('worker did not become ready: ' + worker_failure(ready))
        policy = ready.get('draft_policy', 'full_context')
        if policy not in ('full_context', 'incremental'):
            raise RuntimeError('unknown worker draft policy')
        if args.worker_cadence:
            if policy != 'incremental':
                raise RuntimeError('native worker did not declare incremental policy')
            if args.first_draft_ms is not None or args.draft_interval_ms != 1000:
                raise RuntimeError('worker cadence excludes explicit timing overrides')
            first_draft_seconds, draft_interval_seconds = ((.5, .5) if policy == 'incremental' else (2, 1))
        cold_ms = (time.monotonic() - started) * 1000
        capture = time.monotonic()
        sent, first_partial, first_lexical_partial, revisions, drafts = 0, None, None, 0, []
        previous = ''
        next_draft = capture + first_draft_seconds
        if args.continuous_audio:
            source = PacedAudio(worker.stdin, pcm, capture)
            source.thread.start()
        while sent < len(pcm):
            # Model backpressure advances the source clock; samples are retained
            # and never accelerated to make latency look better.
            arrived = None
            if source is not None:
                time.sleep(max(0, next_draft - time.monotonic()))
                inference = time.monotonic()
                sequence, sent, arrived = source.control(2)
                next_draft = inference + draft_interval_seconds
            else:
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
            if first_lexical_partial is None and has_lexical_text(text):
                first_lexical_partial = (time.monotonic() - capture) * 1000
            if previous and not text.startswith(previous):
                revisions += 1
            drafts.append({'samples': sent // 2, 'wall_ms': (time.monotonic()-capture)*1000,
                           'inference_ms': (time.monotonic()-inference)*1000,
                           'last_audio_frame_finished_ms': arrived, 'text': text})
            previous = text
        final_started = time.monotonic()
        if source is not None:
            source.thread.join(timeout=2)
            if source.thread.is_alive():
                raise RuntimeError('public PCM source did not finish')
            sequence, sent, _ = source.control(3)
        else:
            sequence += 1
            send(worker.stdin, 3, sequence)
        final = receive(worker.stdout)
        final_ms = (time.monotonic() - final_started) * 1000
        if final.get('kind') != 'done' or final.get('sequence') != sequence:
            raise RuntimeError('production final failed')
        if worker.wait(timeout=10) != 0 or first_partial is None or first_lexical_partial is None:
            raise RuntimeError('worker exit or lexical partial missing')
        preparation = None
        if args.continuous_audio or args.worker_diagnostics:
            diagnostic = worker.stderr.read(4097)
            worker.stderr.close()
            preparation = preparation_diagnostics(diagnostic, args.worker_diagnostics)
            if args.expect_attention and preparation['flash_attention'] != (args.expect_attention == 'flash'):
                raise RuntimeError('actual native attention strategy differs from expectation')
        finished.set()
        sampler.join(timeout=2)
        if memory_errors or not all(name in peak for name in ('VmPeak', 'VmSize', 'VmRSS', 'VmHWM', 'Threads')):
            raise RuntimeError('could not observe production worker memory')
        usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        print('SPEECH_STREAM_PROBE ' + json.dumps({
            'case': case['id'], 'language': language,
            'backend': 'nemotron-review' if args.native_review_model is not None else 'whisper',
            'public_sources': [{'id': case['id'], 'sha256': case['sha256']} for case in cases],
            'input_pcm_sha256': hashlib.sha256(pcm).hexdigest(),
            'original_pcm_sha256': original_pcm_sha256, 'synthetic_noise': noise,
            'assembled_language_transition': len({case['language'] for case in cases}) > 1,
            'public_vocabulary': args.vocabulary, 'model_sha256': model['sha256'], 'cpu_affinity_count': len(cpus),
            'profile': 'CPU-affinity constraint; hardware identity requires an external receipt',
            'audio_seconds': len(pcm)/32000, 'cold_ms': cold_ms,
            'first_draft_seconds': first_draft_seconds,
            'draft_interval_ms': draft_interval_seconds * 1000,
            'worker_draft_policy': policy,
            'worker_cadence_used': args.worker_cadence,
            'native_right_context': args.native_right_context if args.native_review_model is not None else None,
            'preparation_phases': preparation,
            'audio_transport': 'continuous20ms' if source is not None else 'source-paced draft batches',
            'first_audio_frame_started_ms': source.first_started_ms if source is not None else None,
            'first_audio_frame_finished_ms': source.first_finished_ms if source is not None else None,
            'first_partial_from_source_start_ms': first_partial,
            'first_lexical_partial_from_source_start_ms': first_lexical_partial,
            'non_prefix_partial_revisions': revisions, 'drafts': drafts,
            'finalization_ms': final_ms, 'final_text': final['text'],
            'cpu_user_seconds': usage_after.ru_utime-usage_before.ru_utime,
            'cpu_system_seconds': usage_after.ru_stime-usage_before.ru_stime,
            'wall_seconds': time.monotonic()-started, 'worker_exit': 0,
            'cpu_accounting': 'serial worker RUSAGE_CHILDREN delta; includes verification and preparation',
            'memory_limit_bytes': 1073741824, 'memory_peak': peak,
            'memory_unit': 'KiB except Threads', 'memory_sample_interval_ms': 20,
            'reference': case['reference'],
        }, ensure_ascii=False), flush=True)
    finally:
        if source is not None:
            source.stop.set()
        if worker:
            if worker.poll() is None:
                worker.kill()
            worker.wait(timeout=10)
            worker.stdin.close()
            worker.stdout.close()
            if worker.stderr is not None:
                worker.stderr.close()
        if source is not None:
            source.thread.join(timeout=2)
            if source.thread.is_alive():
                raise RuntimeError('owned PCM source cleanup failed')
        finished.set()
        if sampler:
            sampler.join(timeout=2)
        if holder is not None:
            holder.stdin.close()
            if holder.wait(timeout=10) != 0:
                raise RuntimeError('ephemeral witness cleanup failed')
            holder.stdout.close()



if __name__ == '__main__':
    main()
