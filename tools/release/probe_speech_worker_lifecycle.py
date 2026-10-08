#!/usr/bin/env python3
"""Actual native worker cancellation/repeat/silence/language journey.

Uses the existing private protocol helpers and exposed public audio only.
No microphone, account, installed model, transcript logging or fake decoder.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import wave

from probe_speech_stream import receive, send, verified, worker_failure


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', required=True, type=Path)
    parser.add_argument('--model', required=True, type=Path)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--native-right-context', type=int, choices=(1, 3), default=1)
    parser.add_argument('--identity-review', action='store_true')
    args = parser.parse_args()
    if os.geteuid() == 0 or os.environ.get('TILO_TEST_PRIVATE_NAMESPACE') != '1':
        parser.error('owned unprivileged hardware-free namespace required')
    if args.manifest.stat().st_size > 1048576:
        parser.error('bounded public manifest required')
    manifest = json.loads(args.manifest.read_text())
    if manifest['dataset'] != 'google/fleurs' or manifest['revision'] != '70bb2e84b976b7e960aa89f1c648e09c59f894dd':
        parser.error('exact public corpus required')
    case = next(row for row in manifest['cases'] if row['id'] == 'en_us-0')
    path = verified(args.manifest.parent, case)
    with wave.open(str(path)) as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
            parser.error('mono16k PCM16 required')
        pcm = audio.readframes(32000)
    if len(pcm) != 64000:
        parser.error('two exposed seconds required')

    def start(language, vocabulary, model=args.model):
        child = subprocess.Popen([str(args.worker), '--model', str(model),
                                  '--review-right-context', str(args.native_right_context)],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.DEVNULL)
        try:
            send(child.stdin, 0, 1, json.dumps({'language': language, 'vocabulary': vocabulary}).encode())
            first = receive(child.stdout)
            return child, first
        except BaseException:
            close(child)
            raise

    def close(child):
        if child.poll() is None:
            child.kill()  # only this directly owned worker
        child.wait(timeout=10)
        child.stdin.close()
        child.stdout.close()

    identities = {}
    if args.identity_review:
        for name, error in [('mutable-same-size.gguf', 'model_receipt'),
                            ('verity-same-size.gguf', 'model_receipt'),
                            ('model-symlink.gguf', 'model_unavailable')]:
            child, first = start('en', [], args.model.parent / name)
            try:
                if worker_failure(first) != error or child.wait(timeout=10) != 1:
                    raise RuntimeError('untrusted model was not rejected before Ready')
                identities[name] = error
            finally:
                close(child)
        rejected = subprocess.run([str(args.worker), '--model', str(args.model),
                                   '--review-right-context', '2'], input=b'',
                                  capture_output=True, timeout=10)
        if rejected.returncode != 2 or rejected.stdout or b'trained context' not in rejected.stderr:
            raise RuntimeError('untrained context accepted by actual worker CLI')
        identities['untrained_context'] = 'CLI refusal before model access'

    cancelled, ready = start('en', ['TiLo'])
    try:
        if ready.get('kind') != 'ready' or ready.get('sequence') != 1:
            raise RuntimeError('cancel worker failed readiness: ' + worker_failure(ready))
        send(cancelled.stdin, 1, 2, pcm)
        send(cancelled.stdin, 2, 3)
        cancelled.kill()
        if cancelled.wait(timeout=10) != -9:
            raise RuntimeError('owned cancellation did not terminate worker')
    finally:
        close(cancelled)

    repeated, ready = start('en', [])
    try:
        if ready.get('kind') != 'ready' or ready.get('sequence') != 1:
            raise RuntimeError('repeat worker failed readiness: ' + worker_failure(ready))
        send(repeated.stdin, 1, 2, b'\0\0' * 16000)
        send(repeated.stdin, 2, 3)
        draft = receive(repeated.stdout)
        if draft.get('kind') != 'draft' or draft.get('sequence') != 3 or draft.get('text') != '':
            raise RuntimeError('silence draft was not empty')
        send(repeated.stdin, 3, 4)
        final = receive(repeated.stdout)
        if final.get('kind') != 'done' or final.get('sequence') != 4 or final.get('text') != '':
            raise RuntimeError('silence final was not empty')
        if repeated.wait(timeout=10) != 0:
            raise RuntimeError('repeat session did not exit cleanly')
    finally:
        close(repeated)

    unsupported, first = start('zz', [])
    try:
        if worker_failure(first) != 'unsupported_language' or unsupported.wait(timeout=10) != 1:
            raise RuntimeError('unsupported selected language was not rejected before readiness')
    finally:
        close(unsupported)
    print('SPEECH_WORKER_LIFECYCLE ' + json.dumps({
        'native_right_context': args.native_right_context,
        'model_identity': identities,
        'cancel_after_ready_and_draft_request': 'owned worker terminated',
        'repeat': 'fresh configured worker, empty vocabulary',
        'silence_draft': 'empty', 'silence_final': 'empty',
        'unsupported_language': 'typed failure before Ready',
        'microphone_or_installation': False, 'transcript_logged': False,
    }), flush=True)


if __name__ == '__main__':
    main()
