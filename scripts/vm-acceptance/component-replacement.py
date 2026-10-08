#!/usr/bin/env python3
"""Validate captured owned-slot replacement evidence; performs no VM operations."""
import argparse
import json
from pathlib import Path

from acceptance.replacement import ReplacementError, audit_directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--vm", required=True)
    parser.add_argument("--slot", required=True)
    parser.add_argument("--source", required=True, help="full candidate source commit")
    args = parser.parse_args()
    try:
        result = audit_directory(args.evidence, args.vm, args.slot, args.source)
    except (ReplacementError, OSError, ValueError, KeyError, TypeError) as error:
        print(json.dumps({"status": "failed", "error": str(error)}))
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
