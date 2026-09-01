#!/usr/bin/env python3
"""Keep qOS applications on the resource/action application model.

Every graphical app must publish its current resources from the widget tree and
describe its operations with ``ActionSpec``. The headless agent is the one
application-model consumer: it surveys peer resources and invokes their action
descriptors. This cheap source-boundary check makes a new bespoke command bus or
an app with no OS-visible surface fail presubmit instead of silently forking the
platform again.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

RESOURCE_PROVIDER = re.compile(r"\bfn\s+resources\s*\(")
ACTION_SPEC = re.compile(r"\bActionSpec\b")
RESOURCE_CONSUMER = re.compile(r"\bActionDescriptor\b")
LEGACY_ENTRY_POINTS = {
    "legacy room-action override": re.compile(r"\bfn\s+apply_room_action\s*\("),
}


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str

    def display(self, root: Path) -> str:
        return f"{self.path.relative_to(root)}:{self.line}: {self.reason}"


def dependency_tables(manifest: dict) -> list[dict]:
    tables = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        value = manifest.get(key, {})
        if isinstance(value, dict):
            tables.append(value)
    return tables


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def audit(root: Path = ROOT) -> tuple[list[Violation], int]:
    violations: list[Violation] = []
    manifests = sorted((root / "apps").glob("*/Cargo.toml"))
    for manifest_path in manifests:
        with manifest_path.open("rb") as handle:
            manifest = tomllib.load(handle)
        dependencies = {
            name for table in dependency_tables(manifest) for name in table
        }
        if "qos-actions" not in dependencies:
            violations.append(
                Violation(
                    manifest_path,
                    1,
                    "app does not depend on `qos-actions`, the application protocol",
                )
            )

        sources: list[tuple[Path, str]] = []
        for source in sorted((manifest_path.parent / "src").rglob("*.rs")):
            sources.append((source, source.read_text(encoding="utf-8")))
        combined = "\n".join(text for _, text in sources)
        app_name = manifest_path.parent.name
        if app_name == "agent":
            if not RESOURCE_PROVIDER.search(combined) or not RESOURCE_CONSUMER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "headless agent must survey resources and invoke ActionDescriptor entries",
                    )
                )
        else:
            if not RESOURCE_PROVIDER.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app does not implement Widget::resources",
                    )
                )
            if not ACTION_SPEC.search(combined):
                violations.append(
                    Violation(
                        manifest_path,
                        1,
                        "app exposes no ActionSpec operations",
                    )
                )

        for source, text in sources:
            for reason, pattern in LEGACY_ENTRY_POINTS.items():
                for match in pattern.finditer(text):
                    violations.append(
                        Violation(source, line_number(text, match.start()), reason)
                    )

    return violations, len(manifests)


def main() -> int:
    violations, app_count = audit()
    if violations:
        print("application resource/action contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation.display(ROOT)}", file=sys.stderr)
        return 1
    print(
        "application resource/action contract OK "
        f"({app_count} app crates; one resource/action protocol)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
