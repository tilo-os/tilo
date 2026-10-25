#!/usr/bin/env python3
"""Enforce the application boundary around Tilo text rendering.

Apps may compose the public ``ui`` text widgets, editors, ``RichTextLayout``
or ``TextRenderer``. The shaping implementation and Vello glyph emission stay
private to ``ui`` so fallback, OpenType features, variable axes, wrapping and
caret geometry cannot fork between applications.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_app_resource_action_contract import is_compute_package  # noqa: E402
from rust_source import line_number  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
APPS = ROOT / "apps"

PRIVATE_DEPENDENCIES = {
    "cosmic-text",
    "cosmic_text",
    "fontdb",
    "fontique",
    "parley",
    "skrifa",
    "swash",
}

PRIVATE_SOURCE_PATTERNS = {
    "cosmic-text API": re.compile(r"\bcosmic_text::"),
    "Parley API": re.compile(r"\bparley::"),
    "private font system": re.compile(r"\b(?:FontSystem|SwashCache)::new\s*\("),
    "raw Vello glyph emission": re.compile(r"\.draw_glyphs\s*\(|\bvello::Glyph\b"),
    "product font family literal": re.compile(
        r'"(?:Noto (?:Sans(?: Mono| Math)?|Serif)|New Computer Modern[^"]*)"'
    ),
}

SHARED_TEXT_SURFACES = re.compile(
    r"\b(?:TextRenderer|RichTextLayout|TextInput|RichTextLine|"
    r"BlockEditor|CodeEditor|QueryComposition|widget::Text)\b"
)


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str

    def display(self) -> str:
        relative = self.path.relative_to(ROOT)
        return f"{relative}:{self.line}: {self.reason}"


def dependency_tables(manifest: dict) -> list[dict]:
    tables = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        value = manifest.get(key, {})
        if isinstance(value, dict):
            tables.append(value)
    target = manifest.get("target", {})
    if isinstance(target, dict):
        for target_cfg in target.values():
            if not isinstance(target_cfg, dict):
                continue
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                value = target_cfg.get(key, {})
                if isinstance(value, dict):
                    tables.append(value)
    return tables


def audit(root: Path = ROOT) -> tuple[list[Violation], int, int]:
    apps = root / "apps"
    manifests = sorted(apps.glob("*/Cargo.toml"))
    violations: list[Violation] = []
    shared_users: set[Path] = set()

    for manifest_path in manifests:
        with manifest_path.open("rb") as handle:
            manifest = tomllib.load(handle)
        dependencies = {
            name for table in dependency_tables(manifest) for name in table
        }
        for dependency in sorted(dependencies & PRIVATE_DEPENDENCIES):
            violations.append(
                Violation(
                    manifest_path,
                    1,
                    f"depends directly on private text crate `{dependency}`; use `ui`",
                )
            )
        # An app model crate (`apps/*-model`) is data shared with an app, not
        # an app, and a compute package runs windowless for another app's
        # action: either may have no UI at all.
        is_model = manifest_path.parent.name.endswith("-model")
        windowless = is_model or is_compute_package(manifest_path.parent)
        if "ui" not in dependencies and not windowless:
            violations.append(
                Violation(manifest_path, 1, "app does not depend on the shared `ui` crate")
            )

        app_dir = manifest_path.parent
        for source in sorted(app_dir.rglob("*.rs")):
            if any(part in {"target", "vendor"} for part in source.parts):
                continue
            text = source.read_text(encoding="utf-8")
            if SHARED_TEXT_SURFACES.search(text):
                shared_users.add(app_dir)
            for reason, pattern in PRIVATE_SOURCE_PATTERNS.items():
                for match in pattern.finditer(text):
                    violations.append(
                        Violation(
                            source,
                            line_number(text, match.start()),
                            f"uses {reason}; use a public `ui` text surface",
                        )
                    )

    return violations, len(manifests), len(shared_users)


def main() -> int:
    violations, app_count, shared_users = audit()
    if violations:
        print("application text contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation.display())
        return 1
    print(
        "application text contract OK "
        f"({app_count} app crates use ui; {shared_users} contain text surfaces; "
        "0 private shapers or raw glyph painters)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
