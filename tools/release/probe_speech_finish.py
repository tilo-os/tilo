#!/usr/bin/env python3
"""Measure owned-worker Finish while native draft inference is in flight.

Only previously licensed, pinned public FLEURS PCM may appear in this probe's
output. Run inside the existing hardware-free qualification capsule with an
uninstalled ephemeral catalogue witness; this never grants microphone access.
Paced mode captures exactly three seconds independently of pipe backpressure.
Full recordings cover language/accuracy guards and unsignalled preview controls.
"""

import argparse, hashlib, json, os, pathlib, queue, signal, subprocess, threading, time, wave
from probe_speech_stream import send, receive, verified, preparation_diagnostics

assert os.environ.get("TILO_TEST_PRIVATE_NAMESPACE") == "1"
assert not pathlib.Path("/dev/snd").exists() and not pathlib.Path("/dev/dri").exists()
p = argparse.ArgumentParser()
p.add_argument("--manifest", type=pathlib.Path, required=True)
p.add_argument("--qualifier", required=True)
p.add_argument("--worker", required=True)
p.add_argument("--case", required=True)
p.add_argument(
    "--scenario",
    choices=["interrupt", "partial", "control", "unchanged", "paced"],
    required=True,
)
p.add_argument("--append-case")
a = p.parse_args()
m = json.loads(a.manifest.read_text())
assert m["revision"] == "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
model = m["models"]["small-q5_1"]
assert (
    model["sha256"]
    == "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb"
)
model_path = verified(a.manifest.parent, model)
cases = [
    next(c for c in m["cases"] if c["id"] == n)
    for n in [a.case, *([a.append_case] if a.append_case else [])]
]
chunks = []
for c in cases:
    with wave.open(str(verified(a.manifest.parent, c))) as f:
        assert (f.getnchannels(), f.getsampwidth(), f.getframerate()) == (1, 2, 16000)
        chunks.append(f.readframes(f.getnframes()))
full = chunks[0] + (bytes(2 * 32000) + chunks[1] if a.append_case else b"")
assert 96000 < len(chunks[0]) and len(full) <= 120 * 32000
opening = full if a.scenario == "unchanged" else chunks[0][:-32000]
if a.scenario == "partial":
    assert a.append_case
    opening = full[:-32000]
if a.scenario == "paced":
    full = chunks[0][:96000]
    opening = full[:64000]
cpus = sorted(os.sched_getaffinity(0))[:2]
assert len(cpus) == 2
os.sched_setaffinity(0, cpus)


def cpu(pid):
    f = pathlib.Path("/proc", str(pid), "stat").read_text().rsplit(") ", 1)[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")


holder = worker = None
threads = []
stop = threading.Event()
errors = []
result = None
try:
    holder = subprocess.Popen(
        [a.qualifier, "--hold-fixture", str(model_path)],
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
    sequence = 1
    send(
        worker.stdin,
        0,
        sequence,
        json.dumps({"language": "auto", "vocabulary": []}).encode(),
    )
    ready = receive(worker.stdout)
    assert ready["kind"] == "ready" and ready["sequence"] == sequence
    supported = ready.get("finish_interrupts_draft", False)
    assert type(supported) is bool
    limit = next(
        l
        for l in pathlib.Path("/proc", str(worker.pid), "limits")
        .read_text()
        .splitlines()
        if l.startswith("Max address space")
    ).split()
    assert limit[3:5] == ["1073741824", "1073741824"]
    started = time.monotonic()
    start_cpu = cpu(worker.pid)
    finish_request = None
    signal_sent = False
    draft_sequence = None
    final_sequence = None
    draft_started = None
    finish_cpu_start = None

    def append(data):
        global sequence
        for i in range(0, len(data), 64000):
            sequence += 1
            send(worker.stdin, 1, sequence, data[i : i + 64000])

    def guarded_finish():
        global finish_request, signal_sent, finish_cpu_start
        finish_request = time.monotonic()
        finish_cpu_start = cpu(worker.pid)
        if supported and len(full) > len(opening):
            os.kill(worker.pid, signal.SIGUSR1)
            signal_sent = True

    if a.scenario == "paced":
        pending = queue.Queue(maxsize=256)
        captured = threading.Event()
        draft_written = threading.Event()
        writer_finished = threading.Event()

        def write_frames():
            global sequence, draft_sequence, final_sequence, draft_started
            try:
                while not stop.is_set():
                    kind, data = pending.get(timeout=5)
                    sequence += 1
                    if kind == 2:
                        draft_sequence = sequence
                        draft_started = time.monotonic()
                    if kind == 3:
                        final_sequence = sequence
                    send(worker.stdin, kind, sequence, data)
                    if kind == 2:
                        draft_written.set()
                    if kind == 3:
                        break
            except Exception as e:
                errors.append(type(e).__name__)
            finally:
                writer_finished.set()

        def capture_frames():
            try:
                for offset in range(0, len(full), 640):
                    if stop.wait(
                        max(0, started + (offset + 640) / 32000 - time.monotonic())
                    ):
                        return
                    pending.put((1, full[offset : offset + 640]), timeout=2)
                    if offset + 640 == len(opening):
                        pending.put((2, b""), timeout=2)
                guarded_finish()
                captured.set()
            except Exception as e:
                errors.append(type(e).__name__)
                captured.set()

        threads = [
            threading.Thread(target=write_frames),
            threading.Thread(target=capture_frames),
        ]
        for t in threads:
            t.start()
        assert draft_written.wait(10)
        draft = receive(worker.stdout)
        draft_replied = time.monotonic()
        assert draft["kind"] == "draft" and draft["sequence"] == draft_sequence
        assert captured.wait(5) and not errors and finish_request is not None
        pending.put((3, b""), timeout=2)
        assert writer_finished.wait(10) and not errors
    else:
        append(opening)
        sequence += 1
        draft_sequence = sequence
        draft_started = time.monotonic()
        send(worker.stdin, 2, sequence)
        if a.scenario == "control":
            draft = receive(worker.stdout)
            draft_replied = time.monotonic()
            finish_request = time.monotonic()
            finish_cpu_start = cpu(worker.pid)
            append(full[len(opening) :])
        else:
            time.sleep(6 if a.scenario == "partial" else 0.5)
            guarded_finish()

            def append_tail():
                try:
                    append(full[len(opening) :])
                except Exception as e:
                    errors.append(type(e).__name__)

            threads = [threading.Thread(target=append_tail)]
            threads[0].start()
            draft = receive(worker.stdout)
            draft_replied = time.monotonic()
            threads[0].join(timeout=10)
            assert not threads[0].is_alive() and not errors
        assert draft["kind"] == "draft" and draft["sequence"] == draft_sequence
        sequence += 1
        final_sequence = sequence
        send(worker.stdin, 3, sequence)
    final = receive(worker.stdout)
    final_replied = time.monotonic()
    assert final["kind"] == "done" and final["sequence"] == final_sequence
    finish_cpu = cpu(worker.pid) - finish_cpu_start
    total_cpu = cpu(worker.pid) - start_cpu
    assert worker.wait(timeout=10) == 0
    prep = preparation_diagnostics(worker.stderr.read(4097), True)
    assert prep["flash_attention"]
    result = {
        "case": a.case,
        "append_case": a.append_case,
        "scenario": a.scenario,
        "language": "auto",
        "worker_advertised_interruption": supported,
        "signal_sent": signal_sent,
        "initial_pcm_sha256": hashlib.sha256(opening).hexdigest(),
        "initial_samples": len(opening) // 2,
        "complete_pcm_sha256": hashlib.sha256(full).hexdigest(),
        "complete_samples": len(full) // 2,
        "first_draft_text": draft["text"],
        "first_draft_inference_ms": (draft_replied - draft_started) * 1000,
        "finish_request_ms_from_source": (finish_request - started) * 1000,
        "finish_to_draft_ack_ms": max(0, (draft_replied - finish_request) * 1000),
        "finish_to_final_ms": (final_replied - finish_request) * 1000,
        "finish_to_final_worker_cpu_seconds": finish_cpu,
        "total_worker_cpu_seconds": total_cpu,
        "final_text": final["text"],
        "reference": " ".join(c["reference"] for c in cases),
        "full_recording_scoring_applicable": a.scenario != "paced",
        "transport": "Continuous 20ms capture into a bounded queue during native inference"
        if a.scenario == "paced"
        else "Controlled in-flight Finish with exact captured public PCM, not capture latency",
        "cpu_affinity_count": 2,
        "worker_address_space_bytes": 1073741824,
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
if result is not None:
    result["owned_worker_source_and_witness_exited"] = True
    print(
        "SPEECH_FINISH_PRIORITY_PROBE " + json.dumps(result, ensure_ascii=False),
        flush=True,
    )
