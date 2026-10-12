#!/usr/bin/env python3
"""Prepare the narrow proxy adaptation from the already verified pinned source.

The source pin is checked independently here. This creates only a build tree;
it runs no provider and creates no credentials, signing identity or trust.
"""
import argparse
import hashlib
import json
from pathlib import Path
import tarfile

PIN = Path(__file__).resolve().parents[1] / "external-assets/signal-runtime.json"


def prepare(source, output, pin):
    with source.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != pin["assets"]["source"]["sha256"]:
            raise ValueError("Signal source digest differs")
    member = ("signal-cli-" + pin["signal_source_commit"]
              + "/lib/src/main/java/org/asamk/signal/manager/config/LiveConfig.java")
    with tarfile.open(source) as archive:
        matches = [entry for entry in archive.getmembers() if entry.name == member]
        if len(matches) != 1 or not matches[0].isfile() or matches[0].size > 32768:
            raise ValueError("Pinned Signal configuration is missing or ambiguous")
        reader = archive.extractfile(matches[0])
        if reader is None:
            raise ValueError("Pinned Signal configuration is unreadable")
        code = reader.read().decode("utf-8")
    old = "private static final Optional<HttpProxy> systemProxy = Optional.empty();"
    if code.count(old) != 1:
        raise ValueError("Pinned Signal proxy declaration differs")
    code = code.replace(old, "private static final Optional<HttpProxy> systemProxy = TiloHttpProxy.nativeGate();")
    destination = output / "org/asamk/signal/manager/config/LiveConfig.java"
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise ValueError("Signal configuration output already exists")
    destination.write_text(code)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    prepare(args.source, args.out, json.loads(PIN.read_text()))


if __name__ == "__main__":
    main()
