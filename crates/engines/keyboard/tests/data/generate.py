# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build the swipe decoder's fixtures from FUTO's swipe dataset.

    uv run apps/osk/tests/data/generate.py

Input: the swipe.futo.org dataset (FUTO, MIT, see LICENSE.FUTO-SWIPE), pinned
to one Hugging Face commit. Every file is checked against its byte size and
SHA-256 and the run aborts on any drift. The two JSONL files (~575 MB) are
cached in ``~/.cache/tilo-swipe-data`` (``--cache`` to move it).

Outputs, written next to this script:

* ``swipe_tune.txt``  swipes from the dataset's ``dev`` file, to fit decoder
  constants on.
* ``swipe_eval.txt``  swipes from its ``test`` file, which
  ``swipe::tests::futo_held_out_accuracy`` scores the decoder on.

Both come from the ``swipe-1`` collection (its 5 GB ``train`` file is not
needed). ``dev`` and ``test`` share no contributor session, which the run
checks, so no one's swiping style leaks from tuning into evaluation. Words are
lower-cased and kept only when they are in ``data/words-en.txt``; each file is a
deterministic sample (ordered by the SHA-256 of the swipe's file and id).

Format: ``word<TAB>x,y x,y ...`` in hundredths of an OSK *key unit* (x:
tenths of the key area's width, y: rows), thinned to points at least THIN key
units apart; the decoder resamples by arc length, so thinning barely changes
what it sees.

Coordinates: the dataset normalises each swipe to its letter area (three
rows, 0..1 on both axes) of a standard phone QWERTY with half-key row offsets
(``swipe-5/layouts/qwerty.json``). The OSK's QWERTY
(``KeyboardLayout::qwerty`` + ``key_rects``) shares the top and bottom letter
rows but spreads its nine home-row keys over the full width. Points are
mapped with a per-row affine x map that lands every dataset key centre on the
OSK key centre, blended linearly between row centres, so a swipe keeps its
shape relative to the keys it passed. Same inputs, byte-identical outputs.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORDS = HERE.parent.parent / "data" / "words-en.txt"

REVISION = "d71bf5fd7f45b3e7c2ed2d76a21b0dbd3b4ba566"
BASE = f"https://huggingface.co/datasets/futo-org/swipe.futo.org/resolve/{REVISION}"
# name -> (byte size, SHA-256)
INPUTS: dict[str, tuple[int, str]] = {
    "LICENSE": (1060, "60968505ce2909b45997d104b678eb42474d47174924338cd0fa652abe0fff60"),
    "swipe-5/layouts/qwerty.json": (
        4667,
        "1965ecd59c9e4bff89446bb56ff3a2d0070b16eeae4ce424ce08b06ed6864632",
    ),
    "dev.jsonl": (304264958, "8d9d34ec1c12f564d02dcb69cf132ba19d45b75632a205af90d3a81290e24972"),
    "test.jsonl": (269612671, "d4e42e34ced1cae925a5ec94672bdaf48b4532598a1e39d72884f9c7b588721e"),
}
# (source file, output, sample size)
FIXTURES = (
    ("dev.jsonl", "swipe_tune.txt", 2000),
    ("test.jsonl", "swipe_eval.txt", 1500),
)
THIN = 0.2

OSK_ROWS = ("qwertyuiop", "asdfghjkl", "zxcvbnm")
# Key widths (layout weight units) of the letters' rows: rows 0 and 1 are bare
# letters, row 2 is Shift (1.5) + letters + Backspace (1.5).
OSK_ROW_WIDTHS = ([1.0] * 10, [1.0] * 9, [1.5] + [1.0] * 7 + [1.5])


def fetch(name: str, cache: Path) -> Path:
    size, sha = INPUTS[name]
    dest = cache / name
    if dest.exists() and dest.stat().st_size == size and digest(dest) == sha:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    print(f"fetching {name} ({size} bytes)", file=sys.stderr)
    h = hashlib.sha256()
    n = 0
    with urllib.request.urlopen(f"{BASE}/{name}") as resp, tmp.open("wb") as out:
        while chunk := resp.read(1 << 20):
            h.update(chunk)
            n += len(chunk)
            out.write(chunk)
    if n != size or h.hexdigest() != sha:
        tmp.unlink()
        sys.exit(f"{name}: got {n} bytes sha256 {h.hexdigest()}, pinned {size} {sha}")
    tmp.rename(dest)
    return dest


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def osk_centres() -> dict[str, tuple[float, float]]:
    """Letter key centres of the OSK QWERTY in key units."""
    out = {}
    for r, letters in enumerate(OSK_ROWS):
        widths = OSK_ROW_WIDTHS[r]
        unit = 10.0 / sum(widths)
        labels = list(letters) if r < 2 else [None, *letters, None]
        x = 0.0
        for label, w in zip(labels, widths):
            if label:
                out[label] = (x + w * unit / 2, r + 0.5)
            x += w * unit
    return out


class Mapper:
    """Dataset (0..1 letter area) -> OSK key units."""

    def __init__(self, cache: Path):
        layout = json.loads(fetch("swipe-5/layouts/qwerty.json", cache).read_text())
        ds = {k["letter"]: (k["cx"], k["cy"]) for k in layout["keys"]}
        osk = osk_centres()
        self.rows = []  # (dataset row centre y, a, b) with x_osk = a*x + b
        for letters in OSK_ROWS:
            xs = [ds[c][0] for c in letters]
            xo = [osk[c][0] for c in letters]
            n = len(xs)
            mx, mo = sum(xs) / n, sum(xo) / n
            a = sum((x - mx) * (o - mo) for x, o in zip(xs, xo)) / sum((x - mx) ** 2 for x in xs)
            self.rows.append((ds[letters[0]][1], a, mo - a * mx))
        self.row_h = self.rows[1][0] - self.rows[0][0]

    def __call__(self, x: float, y: float) -> tuple[float, float]:
        (y0, a0, b0), (y1, a1, b1), (y2, a2, b2) = self.rows
        if y <= y0:
            xo = a0 * x + b0
        elif y >= y2:
            xo = a2 * x + b2
        elif y <= y1:
            t = (y - y0) / (y1 - y0)
            xo = (1 - t) * (a0 * x + b0) + t * (a1 * x + b1)
        else:
            t = (y - y1) / (y2 - y1)
            xo = (1 - t) * (a1 * x + b1) + t * (a2 * x + b2)
        # Rows are evenly spaced in both, so y is a plain scale.
        return xo, 0.5 + (y - y0) / self.row_h


def load_words() -> set[str]:
    return {line.split("\t")[0] for line in WORDS.read_text().splitlines()}


def thin(pts: list[tuple[float, float]]) -> list[tuple[float, float]]:
    out = [pts[0]]
    for p in pts[1:-1]:
        if math.dist(p, out[-1]) >= THIN:
            out.append(p)
    out.append(pts[-1])
    return out


def sample(name: str, size: int, cache: Path, words: set[str]) -> tuple[list[str], set[str], int]:
    """(fixture lines, sessions seen, eligible swipes) for one source file."""
    mapper = Mapper(cache)
    ranked, sessions = [], set()
    with fetch(name, cache).open() as f:
        for line in f:
            rec = json.loads(line)
            sessions.add(rec["session"])
            word = rec.get("word", "").lower()
            data = rec.get("data")
            if len(word) < 2 or word not in words or not isinstance(data, list) or len(data) < 2:
                continue
            rank = hashlib.sha256(f"{name}:{rec['id']}".encode()).hexdigest()
            ranked.append((rank, word, data))
    ranked.sort(key=lambda r: r[0])
    lines = []
    for _, word, data in ranked[:size]:
        pts = thin([mapper(p["x"], p["y"]) for p in data])
        coords = " ".join(f"{round(x * 100)},{round(y * 100)}" for x, y in pts)
        lines.append(f"{word}\t{coords}\n")
    return lines, sessions, len(ranked)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cache", type=Path, default=Path.home() / ".cache" / "tilo-swipe-data")
    ap.add_argument("--out", type=Path, default=HERE)
    args = ap.parse_args()
    fetch("LICENSE", args.cache)
    words = load_words()
    seen: list[set[str]] = []
    for name, out, size in FIXTURES:
        lines, sessions, eligible = sample(name, size, args.cache, words)
        seen.append(sessions)
        (args.out / out).write_text("".join(lines))
        print(f"{name}: {eligible} eligible swipes, wrote {len(lines)} to {out}", file=sys.stderr)
    shared = set.intersection(*seen)
    if shared:
        sys.exit(f"{len(shared)} contributor sessions appear in both splits")


if __name__ == "__main__":
    main()
