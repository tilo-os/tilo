#!/usr/bin/env python3
"""A surface draws the same at a size whether it opened there or was resized to it.

`--responsive-matrix` constructs an app at its default size and then resizes the
capture to each preset, which is what a tiling desktop does to a window all day:
a tile is resized when a neighbour opens, not reopened at its new size. If an
app decides its layout once, from the size it had at construction, the resized
frame shows the wide layout clipped into a narrow rect while the same app opened
narrow draws correctly — and nobody who does not resize ever sees it.

So for every preset: a frame from a process that opened at that size must equal
a frame from a process that opened at the default size and was resized to it.
That equality is the whole check. It found the Settings defect that three
readings of the source missed, and it is the reason to keep the matrix honest.

    python3 tools/ci/capture_parity.py --app settings
    python3 tools/ci/capture_parity.py --binary ./target/debug/calculator
"""
from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# `RESPONSIVE_VIEWPORTS` in crates/sdk/ui-core/src/launch.rs. The check reads
# them from the source so a new preset is covered without editing this file.
PRESETS_SOURCE = ROOT / "crates/sdk/ui-core/src/launch.rs"


def presets() -> list[tuple[str, int, int]]:
    text = PRESETS_SOURCE.read_text(encoding="utf-8")
    start = text.index("pub const RESPONSIVE_VIEWPORTS")
    body = text[start : text.index("];", start)]
    found = [
        (name, int(width), int(height))
        for name, width, height in re.findall(
            r'\("([^"]+)",\s*\((\d+),\s*(\d+)\)\)', body
        )
    ]
    if not found:
        raise SystemExit("capture parity: no presets found; update PRESETS_SOURCE")
    return found


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# A frame is not reproducible to the byte: the calculator's graph differs by one
# antialiased pixel between identical runs of the same binary, because the
# sample either side of a pixel boundary ties. A parity check that demanded
# byte equality would fail half the time on a correct app, and flaky gates get
# switched off. So a difference is only a layout difference when it is a
# region, and the tolerance is stated rather than implied.
TOLERATED_PIXELS = 64


def differing_pixels(first: Path, second: Path) -> int:
    """Pixels that differ between two frames of the same size."""
    try:
        from PIL import Image, ImageChops  # noqa: PLC0415
    except ImportError as error:  # pragma: no cover - environment, not code
        raise SystemExit(
            "capture parity: comparing frames needs Pillow (python3 -m pip install pillow). "
            "A check that cannot observe its subject must not pass silently."
        ) from error
    left = Image.open(first).convert("RGB")
    right = Image.open(second).convert("RGB")
    if left.size != right.size:
        return left.size[0] * left.size[1]
    difference = ImageChops.difference(left, right)
    return sum(1 for pixel in difference.get_flattened_data() if pixel != (0, 0, 0))


def run(binary: Path, arguments: list[str]) -> None:
    result = subprocess.run(
        [str(binary), *arguments], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise SystemExit(
            f"capture parity: {binary.name} {' '.join(arguments)} failed "
            f"({result.returncode}):\n{result.stderr.strip()}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    which = parser.add_mutually_exclusive_group(required=True)
    which.add_argument("--app", help="app package name, built already")
    which.add_argument("--binary", type=Path, help="an app binary to check")
    parser.add_argument(
        "--theme", default="dark", help="theme to capture in (default: dark)"
    )
    parser.add_argument(
        "--keep",
        type=Path,
        help="copy the frames that failed here, for a look at what moved",
    )
    arguments = parser.parse_args()

    binary = arguments.binary
    if binary is None:
        binary = ROOT / "target/debug" / arguments.app
    if not binary.is_file():
        raise SystemExit(f"capture parity: no binary at {binary}; build it first")

    failures = []
    with tempfile.TemporaryDirectory() as directory:
        temporary = Path(directory)
        matrix = temporary / "matrix"
        matrix.mkdir()
        run(binary, ["--responsive-matrix", str(matrix), "--theme", arguments.theme])
        # The matrix names its frames `<app id>-<preset>.png`, and the app id is
        # the package id (`so.tilo.calculator`), not the binary's file name.
        app_id = None
        for candidate in sorted(matrix.glob("*.png")):
            stem = candidate.name[: -len("-" + presets()[0][0] + ".png")]
            if all((matrix / f"{stem}-{name}.png").is_file() for name, _, _ in presets()):
                app_id = stem
                break
        if app_id is None:
            raise SystemExit("capture parity: the matrix wrote no frame per preset")
        for name, width, height in presets():
            resized = matrix / f"{app_id}-{name}.png"
            if not resized.is_file():
                raise SystemExit(f"capture parity: the matrix wrote no {resized.name}")
            opened = temporary / f"opened-{name}.png"
            run(
                binary,
                [
                    "--shots",
                    str(opened),
                    "--viewport",
                    f"{width}x{height}",
                    "--theme",
                    arguments.theme,
                ],
            )
            if digest(opened) == digest(resized):
                print(f"  {name:8} {width}x{height}: same")
                continue
            differing = differing_pixels(opened, resized)
            same = differing <= TOLERATED_PIXELS
            print(
                f"  {name:8} {width}x{height}: "
                + (
                    f"same ({differing} antialiased pixel(s))"
                    if same
                    else f"DIFFERS in {differing} pixels"
                )
            )
            if not same:
                failures.append((name, opened, resized))
                if arguments.keep:
                    arguments.keep.mkdir(parents=True, exist_ok=True)
                    for kept, frame in (("opened", opened), ("resized", resized)):
                        (arguments.keep / f"{kept}-{name}.png").write_bytes(frame.read_bytes())

    if failures:
        print(
            "capture parity FAILED: a surface draws differently depending on whether it "
            "opened at that size or was resized to it.\n"
            "  Derive the layout from the rect the surface is laid out in, every layout "
            "pass (ui::widget::NavMode), never from the size it had at construction.",
            file=sys.stderr,
        )
        for name, opened, resized in failures:
            print(f"  {name}: opened={opened} resized={resized}", file=sys.stderr)
        if arguments.keep:
            print(f"  frames kept in {arguments.keep}", file=sys.stderr)
        return 1
    print(f"capture parity OK ({app_id}: {len(presets())} preset(s))")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
