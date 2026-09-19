#!/usr/bin/env python3
"""Opt-in native Photos edited-content → Writer proof, using production typed SDK dispatch.

Build shared-runtime-proof, photos and word-processor beforehand. This does not
run performance measurements. Retained live result evidence is not a durable
redacted broker receipt. Headless parity remains separate.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=repo / "target/debug")
    parser.add_argument("--output", type=Path, default=repo / "target/edited-handoff-proof" / time.strftime("%Y%m%d-%H%M%S"))
    args = parser.parse_args()
    binaries = [args.bin_dir / name for name in ("examples/shared-runtime-proof", "photos", "word-processor")]
    hashes = {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest() for path in binaries}
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = {"binary_sha256": hashes, "runtime_exit_status": None}
    with tempfile.TemporaryDirectory(prefix="qos-edit-", dir="/tmp") as temporary:
        root = Path(temporary)
        env = {key: value for key, value in os.environ.items() if not key.startswith(("QOS_", "QUICK_"))}
        env.update({"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
                    "XDG_CACHE_HOME": str(root / "cache"), "QOS_IDENTITY_ROOT": str(root / "identity"),
                    "QOS_BLOB_STORE": str(root / "blobs"), "RUST_LOG": "info"})
        command = [str(binaries[0]), str(root), str(binaries[1]), str(binaries[2]), "--edited-handoff"]
        metadata["command"] = command
        with (args.output / "events.jsonl").open("w") as out, (args.output / "runtime.log").open("w") as err:
            process = subprocess.Popen(command, env=env, stdout=out, stderr=err, start_new_session=True)
            try:
                status = process.wait(timeout=180)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                status = 124
            finally:
                # Never retain identity keys or the runtime's profile directory.
                for pattern in ("*.log", "source.png", "source-content.png", "edited.png", "received.automerge", "signed-document-events.json", "live-producer-result.json", "persisted-producer-receipt.json", "edited-proof.json", "request-*.json", "response-*.json"):
                    for path in root.glob(pattern):
                        shutil.copyfile(path, args.output / path.name)
                if (root / "denied").exists():
                    shutil.copytree(root / "denied", args.output / "denied")
        metadata["runtime_exit_status"] = status
    (args.output / "run-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    events = [json.loads(line) for line in (args.output / "events.jsonl").read_text().splitlines() if line.startswith("{")]
    passed = [event for event in events if event.get("event") == "edited_cross_app_proof_passed"]
    if status != 0 or len(passed) != 1:
        raise SystemExit(f"Edited proof failed (exit {status}); inspect {args.output}")
    evidence = passed[0]
    assert evidence["source_hash"] != evidence["edited_hash"]
    assert evidence["permission_denials_proven"]
    assert evidence["persisted_receipt_output_redaction_verified"]
    assert {event["action"] for event in events if event.get("event") == "typed_permission_denied"} == {"export-edited-content", "place-image"}
    assert len(set(evidence["app_pids"])) == 2
    assert evidence["runtime_pid"] not in evidence["app_pids"]
    assert evidence["both_apps_alive"] and evidence["signed_document_events"] > 0
    assert any(event.get("event") == "unavailable_consumer_content_rejected" for event in events)
    print(json.dumps(evidence, indent=2))
    print(f"Evidence: {args.output}")


if __name__ == "__main__":
    main()
