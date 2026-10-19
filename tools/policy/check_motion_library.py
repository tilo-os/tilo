#!/usr/bin/env python3
"""Every animation is composed from the motion library.

Tilo's standard motions live in ``crates/sdk/ui-core/src/motion/`` (``ui::motion``):
appear, drill, disclose, panel slide, cross-fade, list change, snap, zoom,
window motion, feedback, attention. A call site names the motion; how it moves
is decided there, once. This gate fails on the ways an animation gets hand
rolled instead, anywhere else in the toolkit, the shell and the apps:

* easing math: a ``Curve::…`` value, a call to or a definition of an
  ``ease_*`` / ``smoothstep`` helper, a cubic ease written out
  (``1.0 - (1.0 - t).powi(3)``) (a smoothstep written out is as often a
  shape as a motion, so it is caught where it matters, by its time ratio);
* an ad-hoc spring: ``Spring::new(…)`` or a ``SpringParams { … }`` literal;
* lerp over time: an elapsed time divided by a duration
  (``elapsed().as_secs_f32() / …``, ``duration_since(…).as_secs_f64() / …``),
  or any span divided by a duration constant (``remain.as_secs_f32() /
  FADE.as_secs_f32()``): a literal duration timing a fade;
* a wall-clock tween built by hand: ``Animated::new`` / ``Animated::starting_at``.

Only production code counts (tests, benches, examples, ``#[cfg(test)]`` items
and comments are exempt). The motion library itself and the legacy primitives it
replaces (``src/motion/``, ``src/animation/``, the motion tokens) are where this
math belongs.

``EXEMPT`` names the few places whose curves are not UI motion at all but the
content's own (a video's keyframes, paint physics), each with its reason. There
is no allowance beyond it: a new motion goes into the library.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
# The toolkit, the session (shell, desktop) and the apps: everything that draws UI.
SCANNED = (Path("crates/sdk"), Path("crates/session"), Path("apps"))

# Where motion math belongs.
LIBRARY = (
    "crates/sdk/ui-core/src/motion/",
    "crates/sdk/ui-core/src/animation/",
    "crates/sdk/ui-core/src/tokens/motion.rs",
)

# Curves that belong to the content, not to UI motion. Keep this short; every
# entry says why.
EXEMPT = {
    "apps/video-editor/": "keyframe interpolation and transitions are the video's own content",
    "crates/media/": "paint, brush and media physics simulate the medium, not UI motion",
    "apps/canvas/src/brushes.rs": "brush dynamics are part of the stroke the person paints",
    "crates/sdk/ui-core/src/theme.rs": "blends two themes' tokens, spring presets included",
    "vendor/": "vendored upstream code",
}

PATTERNS = (
    ("easing curve", re.compile(r"\bCurve::(?:Linear|EaseOut|EaseInOut|Spring)\b")),
    (
        "easing helper",
        re.compile(r"\b(?:ease_(?:in_out|out|in)\w*|smoothstep)\s*\("),
    ),
    ("hand-rolled easing helper", re.compile(r"\bfn\s+(?:ease|smoothstep)\w*\s*[<(]")),
    ("cubic ease written out", re.compile(r"1\.0\s*-\s*\(\s*1\.0\s*-\s*\w+\s*\)\.powi\(\s*3\s*\)")),
    ("ad-hoc spring", re.compile(r"\bSpring::new\s*\(|\bSpringParams\s*\{")),
    (
        "lerp over time",
        re.compile(
            r"(?:\belapsed\(\)|\b(?:elapsed|age|remaining|left)\b|\b(?:saturating_)?duration_since\([^()]*(?:\([^()]*\))?[^()]*\))"
            r"\s*\.as_secs_f(?:32|64)\(\)\s*/"
        ),
    ),
    (
        "fade timed by a literal duration",
        re.compile(r"\.as_secs_f(?:32|64)\(\)\s*/\s*[A-Z][A-Z0-9_]*\.as_secs_f(?:32|64)\(\)"),
    ),
    ("hand-built tween", re.compile(r"\bAnimated::(?:new|starting_at)\s*\(")),
    (
        "hand-rolled time ratio",
        re.compile(r"\b(?:duration_fraction|elapsed_ms_f32)\s*\("),
    ),
)


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    what: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.what}; use a named motion from ui::motion"


def in_scope(relative: str) -> bool:
    if any(relative.startswith(prefix) for prefix in LIBRARY):
        return False
    return not any(relative.startswith(prefix) for prefix in EXEMPT)


def scan_file(root: Path, path: Path) -> list[Finding]:
    relative = path.relative_to(root).as_posix()
    if not in_scope(relative):
        return []
    parts = Path(relative).parts
    if is_test_file(Path(relative)) or "examples" in parts:
        return []
    text = production_source(path.read_text(encoding="utf-8", errors="replace"))
    findings = []
    for what, pattern in PATTERNS:
        for match in pattern.finditer(text):
            findings.append(Finding(relative, line_number(text, match.start()), what))
    return sorted(findings, key=lambda f: (f.line, f.what))


def scan(root: Path) -> list[Finding]:
    findings: list[Finding] = []
    for top in SCANNED:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            if "target" in path.relative_to(root).parts:
                continue
            findings.extend(scan_file(root, path))
    return findings


def main(argv: list[str]) -> int:
    findings = scan(ROOT)
    if "--list" in argv or not findings:
        for finding in findings:
            print(finding)
        return 0
    print("Animations are composed from ui::motion (crates/sdk/ui-core/src/motion/):")
    for finding in findings:
        print(finding)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
