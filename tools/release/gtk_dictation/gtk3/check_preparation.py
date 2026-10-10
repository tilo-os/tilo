#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise real preparation and refusal paths using already acquired sources."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import tempfile

from prepare import prepare, digest


def check(archive, history):
    with tempfile.TemporaryDirectory(prefix="tilo-gtk3-source-check-") as directory:
        root = Path(directory)
        bad = root / "bad.tar.xz"
        bad.write_bytes(b"unverified source")
        wrong = root / "history"
        (wrong / "gtk").mkdir(parents=True)
        (wrong / "gtk/gtktexthistory.c").write_bytes(b"unverified history")
        for candidate_archive, candidate_history in [(bad, history), (archive, wrong)]:
            output = root / "refused"
            try:
                prepare(candidate_archive, candidate_history, output)
            except ValueError:
                pass
            else:
                raise AssertionError("An unverified source was prepared")
            assert not output.exists(), "Refusal must precede output mutation"
        existing = root / "existing"
        existing.mkdir()
        witness = existing / "preserve"
        witness.write_text("original")
        try:
            prepare(archive, history, existing)
        except FileExistsError:
            pass
        else:
            raise AssertionError("Existing output was overwritten")
        assert witness.read_text() == "original"
        output = root / "prepared"
        with contextlib.redirect_stdout(io.StringIO()):
            prepare(archive, history, output)
        receipt = json.loads((output / "preparation-receipt.json").read_text())
        assert receipt["compiled"] is False and receipt["installed"] is False
        assert receipt["production_ready"] is False
        for name, expected in receipt["prepared_files"].items():
            assert digest(output / "gtk-3.24.52" / name) == expected
    print("PASS: exact-source application, archive refusal, history refusal, existing-output preservation")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gtk3-archive", type=Path, required=True)
    parser.add_argument("--history-source", type=Path, required=True)
    args = parser.parse_args()
    check(args.gtk3_archive.resolve(), args.history_source.resolve())
