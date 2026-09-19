#!/usr/bin/env python3
"""Run two real GUI apps through the production runtime; retain non-secret evidence."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import sys


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=repo / "target/debug")
    parser.add_argument("--benchmark", action="store_true", help="Measure 10 warmup + 50 actions per app and live CPU frame timings")
    parser.add_argument("--output", type=Path, default=repo / "target/runtime-proof" / time.strftime("%Y%m%d-%H%M%S"))
    args = parser.parse_args()
    if args.benchmark:
        sys.path.insert(0, str(repo / "scripts/perf"))
        from runtime_benchmark import binary_hashes, report
        hashes = binary_hashes(args.bin_dir)
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = {"command": sys.argv, "binary_directory": str(args.bin_dir.resolve()),
                "binary_sha256_before_launch": hashes if args.benchmark else None}
    (args.output / "run-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    with tempfile.TemporaryDirectory(prefix="qos-rt-", dir="/tmp") as temporary:
        root = Path(temporary)
        env = os.environ.copy()
        # Do not inherit an unrelated qOS session's service descriptors/identity.
        for key in list(env):
            if key.startswith("QOS_") or key.startswith("QUICK_"):
                del env[key]
        env.update({
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "QOS_IDENTITY_ROOT": str(root / "identity"),
            "QOS_BLOB_STORE": str(root / "blobs"),
            "RUST_LOG": "info",
        })
        command = [str(args.bin_dir / "examples/shared-runtime-proof"), str(root),
                   str(args.bin_dir / "photos"), str(args.bin_dir / "word-processor")]
        if args.benchmark:
            command.append("--benchmark")
        with (args.output / "events.jsonl").open("w") as out, (args.output / "runtime.log").open("w") as err:
            process = subprocess.Popen(command, env=env, stdout=out, stderr=err, start_new_session=True)
            try:
                status = process.wait(timeout=600 if args.benchmark else 120)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                status = 124
            finally:
                for name in ["dev.qos.photos.log", "dev.qos.word-processor.log", "dev.qos.photos.frames.jsonl", "dev.qos.word-processor.frames.jsonl", "source.png", "received.automerge", "signed-document-events.json"]:
                    if (root / name).exists():
                        shutil.copyfile(root / name, args.output / name)
        events = [json.loads(line) for line in (args.output / "events.jsonl").read_text().splitlines() if line.startswith("{")]
        metadata["runtime_exit_status"] = status
        (args.output / "run-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
        passed = [event for event in events if event.get("event") == "cross_app_proof_passed"]
        assert status == 0 and len(passed) == 1, f"Proof failed (exit {status}); inspect {args.output}"
        evidence = passed[0]
        assert len(set(evidence["app_pids"])) == 2
        assert evidence["runtime_pid"] not in evidence["app_pids"]
        assert evidence["both_apps_alive"] and evidence["signed_document_events"] > 0
        assert any(event.get("event") == "unauthorized_invocation_denied" for event in events)
        print(json.dumps(evidence, indent=2))
        if args.benchmark:
            try:
                result = report(args.output, events, args.bin_dir, hashes)
            except Exception as error:
                metadata["benchmark_report_status"] = "failed"
                metadata["benchmark_report_error"] = str(error)
                (args.output / "run-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
                raise
            metadata["benchmark_report_status"] = "passed"
            (args.output / "run-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
            (args.output / "benchmark.json").write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(result, indent=2))
        print(f"Evidence: {args.output}")


if __name__ == "__main__":
    main()
