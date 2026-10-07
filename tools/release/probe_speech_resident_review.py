#!/usr/bin/env python3
"""Two completed public sessions in one bounded, isolated native review worker.

No microphone, daemon, installed model, or recognition cleanup. Uses the shared
speech pipe and the continuous source clock from probe_speech_stream.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import time
import wave

from probe_speech_stream import PacedAudio, exact, receive, send, verified


def preparation(pipe):
    line = bytearray()
    deadline = time.monotonic() + 10
    while not line.endswith(b'\n') and len(line) <= 4096:
        line.extend(exact(pipe, 1, deadline))
    if len(line) > 4096 or not line.startswith(b'NATIVE_PREPARATION '):
        raise RuntimeError('bounded native counters missing')
    result = json.loads(line.split(b' ', 1)[1])
    if set(result) != {'verify_ms', 'native_ms'} or any(
            not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0
            for value in result.values()):
        raise RuntimeError('invalid native counters')
    return result


def fixture(manifest, root, name):
    if name == 'silence':
        return bytes(3 * 32000), '', 'en', []
    case = next((item for item in manifest['cases'] if item['id'] == name), None)
    if case is None:
        raise RuntimeError('pinned public fixture unavailable')
    with wave.open(str(verified(root, case))) as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
            raise RuntimeError('requires mono16kHz PCM16')
        pcm = audio.readframes(audio.getnframes())
    if not 0 < len(pcm) <= 120 * 32000:
        raise RuntimeError('public PCM outside session bound')
    # Previously exposed public vocabulary only. Second-session silence uses
    # empty vocabulary and English, checking retirement of German context.
    words = ['Hongkong', 'Kowloon', 'TiLo'] if name == 'de_de-0' else []
    return pcm, case['reference'], case['language'], words


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--worker', type=Path, required=True)
    parser.add_argument('--model', type=Path, required=True)
    parser.add_argument('--case', choices=('en_us-0', 'de_de-0'), required=True)
    parser.add_argument('--second-case', choices=('en_us-0', 'de_de-0', 'silence'), required=True)
    args = parser.parse_args()
    if (os.geteuid() == 0 or os.environ.get('TILO_TEST_PRIVATE_NAMESPACE') != '1'
            or args.manifest.stat().st_size > 1048576 or args.model.stat().st_size != 742090464):
        parser.error('requires bounded unprivileged native review namespace')
    manifest = json.loads(args.manifest.read_text())
    if (manifest.get('dataset') != 'google/fleurs'
            or manifest.get('revision') != '70bb2e84b976b7e960aa89f1c648e09c59f894dd'):
        parser.error('unexpected public corpus')
    fixtures = [fixture(manifest, args.manifest.parent, name)
                for name in (args.case, args.second_case)]
    child = subprocess.Popen([str(args.worker), '--model', str(args.model),
                              '--phase-review', '--review-sessions', '2'],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    source = None
    rows = []
    try:
        for index, (pcm, reference, language, words) in enumerate(fixtures):
            started = time.monotonic()
            send(child.stdin, 0, 1, json.dumps({'language': language, 'vocabulary': words}).encode())
            ready = receive(child.stdout)
            if ready.get('kind') != 'ready' or ready.get('sequence') != 1:
                raise RuntimeError('resident session did not become ready')
            ready_ms = (time.monotonic() - started) * 1000
            phases = preparation(child.stderr)
            if index == 1 and phases['verify_ms'] != 0:
                raise RuntimeError('resident session unexpectedly reverified')
            capture = time.monotonic()
            source = PacedAudio(child.stdin, pcm, capture)
            source.thread.start()
            drafts, sent = [], 0
            next_draft = capture + 2
            while sent < len(pcm):
                time.sleep(max(0, next_draft - time.monotonic()))
                inference = time.monotonic()
                sequence, sent, arrived = source.control(2)
                next_draft = inference + 1
                reply = receive(child.stdout)
                if reply.get('kind') != 'draft' or reply.get('sequence') != sequence:
                    raise RuntimeError('resident draft failed')
                drafts.append({'samples': sent // 2, 'wall_ms': (time.monotonic() - capture) * 1000,
                               'inference_ms': (time.monotonic() - inference) * 1000,
                               'last_audio_frame_finished_ms': arrived, 'text': reply['text']})
            source.thread.join(timeout=2)
            if source.thread.is_alive():
                raise RuntimeError('public PCM source did not finish')
            final_started = time.monotonic()
            sequence, sent, _ = source.control(3)
            final = receive(child.stdout)
            if final.get('kind') != 'done' or final.get('sequence') != sequence:
                raise RuntimeError('resident final failed')
            if (args.case, args.second_case)[index] == 'silence' and (
                    final['text'] or any(row['text'] for row in drafts)):
                raise RuntimeError('prior transcript or vocabulary leaked into silence')
            rows.append({'session': index + 1, 'case': (args.case, args.second_case)[index],
                         'worker_pid': child.pid, 'resident_model': index == 1,
                         'language': language, 'public_vocabulary': words,
                         'input_pcm_sha256': hashlib.sha256(pcm).hexdigest(),
                         'audio_seconds': len(pcm) / 32000, 'ready_ms': ready_ms,
                         'preparation_phases': phases, 'drafts': drafts,
                         'first_audio_frame_finished_ms': source.first_finished_ms,
                         'finalization_ms': (time.monotonic() - final_started) * 1000,
                         'reference': reference, 'final_text': final['text']})
            source = None
        if child.wait(timeout=10) != 0 or child.stderr.read(1):
            raise RuntimeError('resident worker exit or unexpected diagnostic')
        if args.case == args.second_case and rows[0]['final_text'] != rows[1]['final_text']:
            raise RuntimeError('resident recognition changed with identical public input')
        print('NATIVE_RESIDENT_REVIEW ' + json.dumps({'sessions': rows, 'worker_exit': 0,
              'audio_transport': 'continuous20ms', 'first_draft_seconds': 2,
              'memory_limit_bytes': 1073741824, 'cleanup': 'none',
              'scope': 'review-only completed-session reuse; not a persistent service'}, ensure_ascii=False), flush=True)
    finally:
        if source is not None:
            source.stop.set()
        if child.poll() is None:
            child.kill()
        child.wait(timeout=10)
        if source is not None:
            source.thread.join(timeout=2)
            if source.thread.is_alive():
                raise RuntimeError('owned PCM cleanup failed')
        child.stdin.close()
        child.stdout.close()
        child.stderr.close()


if __name__ == '__main__':
    main()
