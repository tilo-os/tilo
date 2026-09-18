#!/usr/bin/env python3
"""Reduce Bazel execution logs without committing toolchain/input inventories."""
import argparse
import json
from pathlib import Path


def objects(text):
    decoder = json.JSONDecoder()
    while text.strip():
        text = text.lstrip()
        value, length = decoder.raw_decode(text)
        yield value
        text = text[length:]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    result = {}
    for path in sorted((args.run / "logs").glob("*-actions.json")):
        result[path.name] = [{key: action.get(key) for key in
                             ["mnemonic", "targetLabel", "runner", "cacheHit", "status", "exitCode"]}
                            for action in objects(path.read_text())]
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
