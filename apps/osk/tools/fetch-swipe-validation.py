#!/usr/bin/env python3
"""Bounded MIT gesture-data import for remote-only evaluation, never training."""
import argparse
import hashlib
import json
import math
import time
from pathlib import Path
import urllib.request

REVISION = "d71bf5fd7f45b3e7c2ed2d76a21b0dbd3b4ba566"
BASE = f"https://huggingface.co/datasets/futo-org/swipe.futo.org"
PINNED = {
    "LICENSE": "60968505ce2909b45997d104b678eb42474d47174924338cd0fa652abe0fff60",
    "swipe-5/layouts/qwerty.json": "1965ecd59c9e4bff89446bb56ff3a2d0070b16eeae4ce424ce08b06ed6864632",
}
MAX_BYTES = 16 * 1024 * 1024
FULL_ROWS = 54269
TEST_ROWS = 49970
TEST_SHA256 = "d4e42e34ced1cae925a5ec94672bdaf48b4532598a1e39d72884f9c7b588721e"
VALIDATION_SHA256 = "8d9d34ec1c12f564d02dcb69cf132ba19d45b75632a205af90d3a81290e24972"
FULL_MAX_BYTES = 512 * 1024 * 1024


def fetch_metadata(name):
    with urllib.request.urlopen(f"{BASE}/raw/{REVISION}/{name}", timeout=30) as response:
        raw = response.read(64 * 1024)
    if hashlib.sha256(raw).hexdigest() != PINNED[name]:
        raise ValueError(f"pinned metadata checksum mismatch: {name}")
    return raw


def verify(out):
    manifest = json.loads((out / "manifest.json").read_text())
    digest = hashlib.sha256()
    with (out / "validation.jsonl").open("rb") as fixture:
        for block in iter(lambda: fixture.read(1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != manifest["fixture_sha256"]:
        raise ValueError("fixture checksum mismatch")
    for name, local in [("LICENSE", "LICENSE.FUTO-data"),
                        ("swipe-5/layouts/qwerty.json", "layout.json")]:
        if hashlib.sha256((out / local).read_bytes()).hexdigest() != PINNED[name]:
            raise ValueError(f"fixture metadata checksum mismatch: {local}")


def source_info():
    with urllib.request.urlopen(f"https://huggingface.co/api/datasets/futo-org/swipe.futo.org/tree/{REVISION}", timeout=30) as response:
        rows = json.loads(response.read(128 * 1024))
    selected = [row for row in rows if row.get("path") in ["dev.jsonl", "test.jsonl"]]
    if len(selected) != 2:
        raise ValueError("pinned split metadata unavailable")
    print(json.dumps(selected))


def fetch(out, limit, full=False, test=False):
    if not full and not 1 <= limit <= 1000:
        raise ValueError("limit must be 1..1000")
    license_bytes = fetch_metadata("LICENSE")
    layout_bytes = fetch_metadata("swipe-5/layouts/qwerty.json")
    sessions = {}
    digest, count = hashlib.sha256(), 0
    rows, groups = 0, ({"test": 0} if test else {"analysis": 0, "qualification": 0})
    out.mkdir(parents=True, exist_ok=True)
    fixture = out / "validation.jsonl.part"
    fixture_digest = hashlib.sha256()
    started = time.monotonic()
    source = "test" if test else "dev"
    url = f"{BASE}/resolve/{REVISION}/{source}.jsonl"
    with urllib.request.urlopen(url, timeout=30) as response, fixture.open("wb") as writer:
        while full or rows < limit:
            raw = response.readline(256 * 1024)
            if full and not raw:
                break
            count += len(raw)
            bound = FULL_MAX_BYTES if full else MAX_BYTES
            if (not raw or not raw.endswith(b"\n") or count > bound
                    or rows >= 60000 or time.monotonic() - started > 900):
                raise ValueError("validation prefix exceeded byte/line bound or ended early")
            digest.update(raw)
            row = json.loads(raw)
            session = hashlib.sha256(row["session"].encode()).digest()
            session_number = sessions.setdefault(session, len(sessions))
            points = [{"x": p["x"], "y": p["y"]} for p in row["data"]]
            if any(not math.isfinite(p[axis]) for p in points for axis in ["x", "y"]):
                raise ValueError("non-finite source coordinates")
            if any(not math.isfinite(row[k]) or row[k] <= 0
                   for k in ["canvas_width", "canvas_height"]):
                raise ValueError("invalid source geometry")
            # No session IDs, sentence text or timestamps enter the evaluator.
            record = {"word": row["word"].lower(), "width": row["canvas_width"],
                      "height": row["canvas_height"], "points": points}
            if full:
                group = "test" if test else ("analysis" if session[0] < 51 else "qualification")
                groups[group] += 1
                record.update(session=session_number, group=group,
                              orientation=row.get("orientation", "unknown"),
                              invalid_sentence=row.get("potentially_invalid_sentence", False))
            encoded = (json.dumps(record, separators=(",", ":")) + "\n").encode()
            writer.write(encoded)
            fixture_digest.update(encoded)
            rows += 1
    expected_rows = TEST_ROWS if test else FULL_ROWS
    if full and rows != expected_rows:
        raise ValueError(f"pinned {source} split should have {expected_rows} rows, got {rows}")
    if full and digest.hexdigest() != (TEST_SHA256 if test else VALIDATION_SHA256):
        raise ValueError("pinned complete validation checksum mismatch")
    fixture.replace(out / "validation.jsonl")
    (out / "LICENSE.FUTO-data").write_bytes(license_bytes)
    (out / "layout.json").write_bytes(layout_bytes)
    manifest = {"dataset": BASE, "revision": REVISION, "split": "test" if test else "validation",
                "selection": (f"complete original {source} split" if full else
                              f"first {limit} rows, original dev split, no repartition"),
                "source_prefix_sha256": digest.hexdigest(), "source_prefix_bytes": count,
                "fixture_sha256": fixture_digest.hexdigest(),
                "records": rows, "sessions": len(sessions), "metadata_sha256": PINNED,
                "training": False,
                "limitations": ("English QWERTY donation corpus; original source geometry; "
                                "no physical Tilo touch-to-photon measurement" if full else
                                "bounded prefix, not representative accuracy; source QWERTY geometry")}
    if full and not test:
        manifest["development_groups"] = {
            "method": "SHA256(original session)[0] < 51: analysis, else qualification",
            "records": groups,
            "original_split_membership_unchanged": True,
            "test_split_read": False,
        }
    if test:
        manifest["test_evaluation"] = {"requires_frozen_matcher": True,
                                       "no_test_error_export": True,
                                       "original_split_membership_unchanged": True}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, nargs="?")
    parser.add_argument("--limit", type=int, default=256)
    full_mode = parser.add_mutually_exclusive_group()
    full_mode.add_argument("--full-validation", action="store_true")
    full_mode.add_argument("--full-test", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--source-info", action="store_true")
    args = parser.parse_args()
    if args.source_info:
        source_info()
    elif args.out is None:
        parser.error("output directory required")
    elif args.verify_only:
        verify(args.out)
    else:
        fetch(args.out, args.limit, args.full_validation or args.full_test, args.full_test)
