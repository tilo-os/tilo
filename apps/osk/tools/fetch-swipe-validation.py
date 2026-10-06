#!/usr/bin/env python3
"""Bounded MIT gesture-data import for remote-only evaluation, never training."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import urllib.request

REVISION = "d71bf5fd7f45b3e7c2ed2d76a21b0dbd3b4ba566"
BASE = f"https://huggingface.co/datasets/futo-org/swipe.futo.org"
PINNED = {
    "LICENSE": "60968505ce2909b45997d104b678eb42474d47174924338cd0fa652abe0fff60",
    "swipe-5/layouts/qwerty.json": "1965ecd59c9e4bff89446bb56ff3a2d0070b16eeae4ce424ce08b06ed6864632",
}
MAX_BYTES = 16 * 1024 * 1024


def fetch_metadata(name):
    with urllib.request.urlopen(f"{BASE}/raw/{REVISION}/{name}", timeout=30) as response:
        raw = response.read(64 * 1024)
    if hashlib.sha256(raw).hexdigest() != PINNED[name]:
        raise ValueError(f"pinned metadata checksum mismatch: {name}")
    return raw


def fetch(out, limit):
    if not 1 <= limit <= 1000:
        raise ValueError("limit must be 1..1000")
    license_bytes = fetch_metadata("LICENSE")
    layout_bytes = fetch_metadata("swipe-5/layouts/qwerty.json")
    records, sessions = [], set()
    digest, count = hashlib.sha256(), 0
    url = f"{BASE}/resolve/{REVISION}/dev.jsonl"
    with urllib.request.urlopen(url, timeout=30) as response:
        for _ in range(limit):
            raw = response.readline(256 * 1024)
            count += len(raw)
            if not raw or not raw.endswith(b"\n") or count > MAX_BYTES:
                raise ValueError("validation prefix exceeded byte/line bound or ended early")
            digest.update(raw)
            row = json.loads(raw)
            sessions.add(hashlib.sha256(row["session"].encode()).hexdigest())
            points = [{"x": p["x"], "y": p["y"]} for p in row["data"]]
            if any(not math.isfinite(p[axis]) for p in points for axis in ["x", "y"]):
                raise ValueError("non-finite source coordinates")
            # No session IDs, sentence text or timestamps enter the evaluator.
            records.append({"word": row["word"].lower(), "width": row["canvas_width"],
                            "height": row["canvas_height"], "points": points})
    out.mkdir(parents=True, exist_ok=True)
    (out / "LICENSE.FUTO-data").write_bytes(license_bytes)
    (out / "layout.json").write_bytes(layout_bytes)
    encoded = "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in records).encode()
    (out / "validation.jsonl").write_bytes(encoded)
    manifest = {"dataset": BASE, "revision": REVISION, "split": "validation",
                "selection": f"first {limit} rows, original dev split, no repartition",
                "source_prefix_sha256": digest.hexdigest(), "source_prefix_bytes": count,
                "fixture_sha256": hashlib.sha256(encoded).hexdigest(),
                "records": len(records), "sessions": len(sessions), "metadata_sha256": PINNED,
                "training": False,
                "limitations": "bounded prefix, not representative accuracy; source QWERTY geometry"}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--limit", type=int, default=256)
    args = parser.parse_args()
    fetch(args.out, args.limit)
