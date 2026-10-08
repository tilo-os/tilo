#!/usr/bin/env python3
"""Stage the original MIT icon from LF text into fresh Android build resources."""
import argparse
from pathlib import Path
import shutil


def prepare(output):
    source = Path(__file__).resolve().parent / "res"
    if output.exists():
        raise ValueError("refusing existing resource output")
    shutil.copytree(source, output, ignore=shutil.ignore_patterns("*.hex"))
    icon = bytes.fromhex((source / "drawable/icon.png.hex").read_text(encoding="ascii"))
    (output / "drawable/icon.png").write_bytes(icon)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    prepare(parser.parse_args().output)
