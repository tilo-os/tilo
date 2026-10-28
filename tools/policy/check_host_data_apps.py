#!/usr/bin/env python3
"""Apps that show the machine they run on are captured only from scenarios.

An app whose manifest grants it the machine's own state shows that state:
`system-info` (its hardware, drives and load), or a required `processes`
(it runs the host's programs: the journal, the container engine, the
firewall, a shell) or a required `files.paths` (it works on the person's
real files). Such an app declares what it shows with `const READS_HOST` on
its `ResourceApp`; a headless drive that captures it must then name a
scenario, and is built from that scenario's fixtures instead of the
machine (`ui_core::launch::HostSource`).

For every such app this check wants:
- `const READS_HOST` in its sources, unless it is listed in `EXCEPTIONS`
  with the reason it shows nothing of the machine on its own;
- a screenshot plan (`tools/screenshots/<app>.toml`, which
  `tools/screenshots/run.py` drives headless) whose every situation names
  a scenario, since the app refuses a capture without one.
"""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLANS = Path("tools/screenshots")

# Apps whose manifest grants them the machine but that show only what the
# person opens in them, never the machine by themselves.
EXCEPTIONS = {
    "backups": "shows the backup location the person chose, nothing until then",
    "code-editor": "shows the project the person opens, nothing until then",
    "git-client": "shows the repository the person opens, nothing until then",
}


def reads_machine(manifest: dict) -> bool:
    capabilities = manifest.get("capabilities", {})
    required = set(capabilities.get("required", []))
    every = required | set(capabilities.get("optional", []))
    return "system-info" in every or bool(required & {"processes", "files.paths"})


def declares(directory: Path) -> bool:
    return any(
        "const READS_HOST" in path.read_text(encoding="utf-8", errors="replace")
        for path in sorted((directory / "src").rglob("*.rs"))
    )


def plan_findings(root: Path, app: str) -> list[str]:
    path = root / PLANS / f"{app}.toml"
    relative = path.relative_to(root).as_posix()
    if not path.is_file():
        return [
            f"{relative}: missing; {app} shows the machine it runs on, so its screenshot "
            "situations must each name a scenario"
        ]
    plan = tomllib.loads(path.read_text(encoding="utf-8"))
    return [
        f"{relative}: situation {situation.get('name', '?')!r} names no scenario; "
        f"{app} refuses a capture without one"
        for situation in plan.get("situation", [])
        if not situation.get("scenario")
    ]


def check(root: Path) -> list[str]:
    findings = []
    for manifest_path in sorted((root / "apps").glob("*/tilo-app.toml")):
        directory = manifest_path.parent
        app = directory.name
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest.get("package", {}).get("kind", "app") != "app":
            continue
        declared = declares(directory)
        if reads_machine(manifest) and app not in EXCEPTIONS and not declared:
            findings.append(
                f"apps/{app}/tilo-app.toml: its manifest grants it the machine's state; declare what it "
                "shows with `const READS_HOST: &'static [ui::launch::HostSource]` on its "
                "ResourceApp (or list it in EXCEPTIONS with the reason it shows none)"
            )
        if declared:
            findings.extend(plan_findings(root, app))
    stale = sorted(app for app in EXCEPTIONS if not (root / "apps" / app).is_dir())
    findings.extend(
        f"tools/policy/check_host_data_apps.py: EXCEPTIONS: {app} is no app any more" for app in stale
    )
    return findings


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    findings = check(args.root)
    for finding in findings:
        print(finding)
    if findings:
        return 1
    print("host-data apps OK: each declares what it shows and is captured from scenarios", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
