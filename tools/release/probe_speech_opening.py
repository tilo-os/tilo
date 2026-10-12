"""Review paced opening previews and short input with pinned public FLEURS PCM.

Run only in the existing unprivileged, hardware-free qualification capsule.
Capture and FIFO writing are independent; model backpressure retains all PCM.
The loop follows FullContext actor timing and Finish interruption. This is a
worker control, not a running compositor or physical microphone acceptance.
Short-auto/fixed use the noise-only first three seconds of the German fixture.
Speech profiles remove its four-second leading noise in the review input only;
three-second excerpts lack word-time alignment and are not complete-phrase WER.
Google/FLEURS revision70bb2e84b976b7e960aa89f1c648e09c59f894dd,
CC-BY-4.0, Google authors. Model and source receipts remain the runner's gate.
"""

import argparse
import hashlib
import json
import os
import pathlib
import queue
import signal
import subprocess
import threading
import time
import wave
from probe_speech_stream import send, receive, verified, preparation_diagnostics

assert os.geteuid() != 0 and os.environ.get("TILO_TEST_PRIVATE_NAMESPACE") == "1"
assert not pathlib.Path("/dev/snd").exists() and not pathlib.Path("/dev/dri").exists()
p = argparse.ArgumentParser()
for name in ("manifest", "qualifier", "worker", "case"):
    p.add_argument("--" + name, required=True)
p.add_argument("--append-case")
p.add_argument("--activity-model")
profiles = {
    "delay2": (2, "auto", None),
    "delay3": (3, "auto", None),
    "delay4": (4, "auto", None),
    "delay5": (5, "auto", None),
    "short-auto": (2, "auto", 3),
    "short-fixed": (2, "de", 3),
    "speech-auto": (2, "auto", 3),
    "speech-fixed": (2, "de", 3),
    "speech-quiet": (2, "de", 3),
    "speech-quiet100": (2, "de", 3),
    "speech-quiet100-loud-tail": (2, "de", 3),
}
p.add_argument("--scenario", choices=tuple(profiles), required=True)
a = p.parse_args()
delay, language, cutoff = profiles[a.scenario]
if language != "auto" or cutoff is not None:
    assert a.case == "de_de-0"
manifest_path = pathlib.Path(a.manifest)
assert manifest_path.stat().st_size <= 1048576
manifest_bytes = manifest_path.read_bytes()
assert (
    hashlib.sha256(manifest_bytes).hexdigest()
    == "7cb63d4b6b67aa47df22dcab3ed424100312865b01275b4ffbcc2fc378d33469"
)
m = json.loads(manifest_bytes)
assert m["revision"] == "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
model = m["models"]["small-q5_1"]
assert (
    model["sha256"]
    == "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb"
)
model_path = verified(manifest_path.parent, model)
c = next(c for c in m["cases"] if c["id"] == a.case)
with wave.open(str(verified(manifest_path.parent, c))) as f:
    assert (f.getnchannels(), f.getsampwidth(), f.getframerate()) == (1, 2, 16000)
    pcm = f.readframes(f.getnframes())
leading_noise = pcm[: 3 * 32000]
references = [c["reference"]]
if a.append_case:
    assert cutoff is None and language == "auto" and a.append_case != a.case
    extra = next(item for item in m["cases"] if item["id"] == a.append_case)
    with wave.open(str(verified(manifest_path.parent, extra))) as f:
        assert (f.getnchannels(), f.getsampwidth(), f.getframerate()) == (1, 2, 16000)
        pcm += bytes(2 * 32000) + f.readframes(f.getnframes())
    references.append(extra["reference"])
if a.scenario.startswith("speech-"):
    assert a.case == "de_de-0"
    pcm = pcm[4 * 32000 :]
if cutoff:
    pcm = pcm[: cutoff * 32000]
if a.scenario in ("speech-quiet", "speech-quiet100", "speech-quiet100-loud-tail"):
    import struct

    pcm = b"".join(
        struct.pack(
            "<h",
            round(x[0] / (100 if a.scenario.startswith("speech-quiet100") else 10)),
        )
        for x in struct.iter_unpack("<h", pcm)
    )
if a.scenario == "speech-quiet100-loud-tail":
    import struct

    noise_values = [value[0] for value in struct.iter_unpack("<h", leading_noise)]
    peak = max(abs(value) for value in noise_values)
    assert peak > 0
    tail = b"".join(
        struct.pack("<h", round(value * 16384 / peak)) for value in noise_values
    )
    pcm += tail
assert 0 < len(pcm) <= 120 * 32000
cpus = sorted(os.sched_getaffinity(0))[:2]
assert len(cpus) == 2
os.sched_setaffinity(0, cpus)
outgoing = queue.Queue(maxsize=8192)
incoming = queue.Queue(maxsize=8)
errors = queue.Queue(maxsize=8)
stop = threading.Event()
holder = worker = None
threads = []
drafts = []
captured = sequence = 0
ready = None
result = None
try:
    holder = subprocess.Popen(
        [a.qualifier, "--hold-fixture", str(model_path)]
        + ([a.activity_model] if a.activity_model else []),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    witness = json.loads(holder.stdout.readline(4096))
    assert witness["ephemeral"] and witness["model_sha256"] == model["sha256"]
    worker = subprocess.Popen(
        [
            a.worker,
            "--root",
            witness["root"],
            "--catalog",
            witness["catalog"],
            "--keys",
            witness["keys"],
            "--diagnostics",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    send(
        worker.stdin,
        0,
        1,
        json.dumps({"language": language, "vocabulary": []}).encode(),
    )
    ready = receive(worker.stdout)
    assert ready["kind"] == "ready" and ready["sequence"] == 1
    assert ready.get("draft_policy", "full_context") == "full_context"
    assert ready.get("finish_interrupts_draft") is True
    assert (
        next(
            l
            for l in pathlib.Path("/proc", str(worker.pid), "limits")
            .read_text()
            .splitlines()
            if l.startswith("Max address space")
        ).split()[3:5]
        == ["1073741824"] * 2
    )
    capture_start = time.monotonic()
    writes = []

    def writer():
        current = 1
        try:
            while not stop.is_set():
                try:
                    kind, payload, samples, queued = outgoing.get(timeout=0.1)
                except queue.Empty:
                    continue
                current += 1
                started = time.monotonic()
                send(worker.stdin, kind, current, payload)
                if kind != 1:
                    writes.append(
                        {
                            "sequence": current,
                            "kind": kind,
                            "samples": samples,
                            "queued_ms": (queued - capture_start) * 1000,
                            "write_started_ms": (started - capture_start) * 1000,
                            "written_ms": (time.monotonic() - capture_start) * 1000,
                        }
                    )
                if kind == 3:
                    return
        except Exception as e:
            errors.put(type(e).__name__)

    def reader():
        try:
            while not stop.is_set():
                reply = receive(worker.stdout)
                incoming.put((reply, time.monotonic()), timeout=2)
                if reply["kind"] in ("done", "error"):
                    return
        except Exception as e:
            if not stop.is_set():
                errors.put(type(e).__name__)

    threads = [threading.Thread(target=writer), threading.Thread(target=reader)]
    for t in threads:
        t.start()
    pending = finishing = final_queued = False
    next_draft = capture_start + delay
    finish_request = None
    max_capture_lateness_ms = 0
    deadline = capture_start + len(pcm) / 32000 + 90
    while True:
        now = time.monotonic()
        assert now < deadline and errors.empty(), "worker/source deadline or error"
        available = min(len(pcm), int((now - capture_start) / 0.02) * 640)
        while captured < available:
            end = min(available, captured + 640)
            due = capture_start + end / 32000
            max_capture_lateness_ms = max(max_capture_lateness_ms, (now - due) * 1000)
            outgoing.put_nowait((1, pcm[captured:end], end // 2, now))
            captured = end
        if captured == len(pcm) and not finishing:
            finishing = True
            finish_request = now
            # Newer PCM is already in the same ordered FIFO, as in the actor.
            if pending and writes and captured // 2 > writes[-1]["samples"]:
                os.kill(worker.pid, signal.SIGUSR1)
        if not pending:
            if finishing and not final_queued:
                outgoing.put_nowait((3, b"", captured // 2, now))
                pending = final_queued = True
            elif not finishing and now >= next_draft:
                outgoing.put_nowait((2, b"", captured // 2, now))
                pending = True
                next_draft = now + 1
        try:
            reply, replied = incoming.get_nowait()
        except queue.Empty:
            time.sleep(0.005)
            continue
        assert reply["kind"] in ("draft", "done"), reply
        assert writes and reply["sequence"] == writes[-1]["sequence"]
        if reply["kind"] == "done":
            assert finishing and final_queued
            final = reply["text"]
            final_at = replied
            break
        pending = False
        entry = dict(
            writes[-1],
            text=reply["text"],
            reply_ms=(replied - capture_start) * 1000,
            inference_ms=(replied - capture_start) * 1000 - writes[-1]["written_ms"],
            suppressed_after_finish=finishing,
        )
        drafts.append(entry)
    assert worker.wait(timeout=10) == 0
    diagnostics = preparation_diagnostics(worker.stderr.read(4097), True)
    result = {
        "case": a.case,
        "profile": a.scenario,
        "first_delay_seconds": delay,
        "language": language,
        "source_pcm_sha256": hashlib.sha256(pcm).hexdigest(),
        "samples": len(pcm) // 2,
        "capture_duration_ms": len(pcm) / 32,
        "max_capture_lateness_ms": max_capture_lateness_ms,
        "review_pcm_offset_seconds": 4 if a.scenario.startswith("speech-") else 0,
        "review_amplitude_scale": {
            "speech-quiet": 0.1,
            "speech-quiet100": 0.01,
            "speech-quiet100-loud-tail": 0.01,
        }.get(a.scenario, 1),
        "loud_tail": a.scenario == "speech-quiet100-loud-tail",
        "loud_tail_source_seconds": [0, 3]
        if a.scenario == "speech-quiet100-loud-tail"
        else None,
        "loud_tail_normalized_peak": 0.5
        if a.scenario == "speech-quiet100-loud-tail"
        else None,
        "onset_ground_truth": "Unaligned public excerpt; leading noise identified by waveform-energy review",
        "first_lexical_preview_ms": next(
            (
                d["reply_ms"]
                for d in drafts
                if not d["suppressed_after_finish"]
                and any(t.isalnum() for t in d["text"])
            ),
            None,
        ),
        "drafts": drafts,
        "final_text": final,
        "reference": " ".join(references),
        "append_case": a.append_case,
        "finish_to_final_ms": (final_at - finish_request) * 1000,
        "total_wall_ms": (final_at - capture_start) * 1000,
        "preparation": diagnostics,
        "full_reference_aligned": cutoff is None,
        "physical_microphone": False,
        "normal_admission": False,
    }
finally:
    stop.set()
    if worker:
        if worker.poll() is None:
            worker.kill()
        worker.wait(timeout=10)
        for t in threads:
            t.join(timeout=6)
            assert not t.is_alive()
        worker.stdin.close()
        worker.stdout.close()
        worker.stderr.close()
    if holder:
        holder.stdin.close()
        assert holder.wait(timeout=10) == 0
        holder.stdout.close()
if result:
    result["owned_source_worker_witness_exited"] = True
    print("SPEECH_OPENING_PROBE " + json.dumps(result, ensure_ascii=False), flush=True)
