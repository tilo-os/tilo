#!/usr/bin/env python3
"""Public English-only streaming comparison; no microphone or network."""
import json
from pathlib import Path
import sys
import time
import wave

import numpy as np
import sherpa_onnx

model = Path(sys.argv[1])
threads = int(sys.argv[3])
if threads not in (1, 2):
    raise ValueError('bounded CPU count required')
with wave.open(sys.argv[2]) as audio:
    if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
        raise ValueError('canonical public WAV required')
    samples = np.frombuffer(audio.readframes(audio.getnframes()), dtype='<i2').astype(np.float32) / 32768
if not 0 < len(samples) <= 120 * 16000:
    raise ValueError('bounded audio required')
load = time.monotonic()
recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(
    tokens=str(model/'tokens.txt'), encoder=str(model/'encoder-epoch-99-avg-1.int8.onnx'),
    decoder=str(model/'decoder-epoch-99-avg-1.onnx'),
    joiner=str(model/'joiner-epoch-99-avg-1.int8.onnx'), num_threads=threads,
    provider='cpu', decoding_method='greedy_search', enable_endpoint_detection=False)
print('ASR_LOAD '+json.dumps({'cold_ms': (time.monotonic()-load)*1000, 'threads': threads}), flush=True)
stream = recognizer.create_stream()
capture = time.monotonic()
for start in range(0, len(samples), 2560):
    end = min(len(samples), start+2560)
    time.sleep(max(0, capture+end/16000-time.monotonic()))
    inference = time.monotonic()
    stream.accept_waveform(16000, samples[start:end])
    while recognizer.is_ready(stream):
        recognizer.decode_stream(stream)
    print('ASR_DRAFT '+json.dumps({'samples': end, 'wall_ms': (time.monotonic()-capture)*1000,
        'inference_ms': (time.monotonic()-inference)*1000,
        'text': recognizer.get_result(stream)}), flush=True)
stop = time.monotonic()
# The pinned runtime's documented finish recipe supplies 0.5 s of zero padding.
# Report it explicitly; these samples are not source audio or capture duration.
stream.accept_waveform(16000, np.zeros(8000, dtype=np.float32))
stream.input_finished()
while recognizer.is_ready(stream):
    recognizer.decode_stream(stream)
print('ASR_FINAL '+json.dumps({'samples': len(samples), 'tail_padding_samples': 8000,
    'finalization_ms': (time.monotonic()-stop)*1000,
    'source_final_ms': (time.monotonic()-capture)*1000,
    'text': recognizer.get_result(stream)}), flush=True)
