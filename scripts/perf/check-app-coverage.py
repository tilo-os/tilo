#!/usr/bin/env python3
"""Check inventory coverage and evidence references, never certify performance."""
import argparse
import json
from pathlib import Path
import tomllib


def validate(root, inventory):
    errors = []
    apps, excluded = inventory["apps"], inventory["excluded"]
    if not isinstance(apps, list) or not isinstance(excluded, list):
        raise ValueError("apps and excluded must be arrays")
    actual = {path.relative_to(root).as_posix(): path
              for path in (root / "apps").glob("*/src/main.rs")}
    if not actual:
        errors.append("No native entrypoints found; check --root")
    seen_paths, seen_apps = set(), set()
    unqualified = []
    if inventory.get("gui_app_count") != len(apps):
        errors.append("gui_app_count does not match apps length")
    for group, entries in (("apps", apps), ("excluded", excluded)):
        for entry in entries:
            name, source = entry["app"], entry["native_entrypoint"]
            if not isinstance(name, str) or not isinstance(source, str):
                raise ValueError("app and native_entrypoint must be strings")
            if name in seen_apps or source in seen_paths:
                errors.append(f"duplicate app or native_entrypoint: {name} ({source})")
            seen_apps.add(name)
            seen_paths.add(source)
            if source not in actual:
                errors.append(f"stale or unknown native_entrypoint: {source}")
            else:
                directory = actual[source].parent.parent
                if name != directory.name:
                    errors.append(f"app identifier does not match entrypoint directory: {name}")
                manifest = tomllib.loads((directory / "Cargo.toml").read_text())
                package = manifest["package"]["name"]
                if entry.get("cargo_package") != package:
                    errors.append(f"{name}: cargo_package must be {package}")
                binaries = [item["name"] for item in manifest.get("bin", [])
                            if item.get("path", "src/main.rs") == "src/main.rs"]
                if not binaries and manifest["package"].get("autobins", True):
                    binaries = [package]
                if entry.get("binary") not in binaries:
                    errors.append(f"{name}: binary does not match manifest entrypoint")
            if group == "excluded":
                reason = entry.get("reason")
                if not isinstance(reason, str) or not reason.strip():
                    errors.append(f"{name}: exclusion reason is required")
                continue
            if entry.get("status") != "qualified":
                unqualified.append(name)
            evidence = entry.get("qualification_evidence")
            if not isinstance(evidence, list) or not evidence:
                errors.append(f"{name}: qualification_evidence must contain file references")
            else:
                for reference in evidence:
                    if (not isinstance(reference, str) or not reference.strip()
                            or not (root / reference).is_file()):
                        errors.append(f"{name}: evidence reference is not an existing file: {reference!r}")
    for source in sorted(actual.keys() - seen_paths):
        errors.append(f"missing native_entrypoint: {source}")
    return {
        "coverage_only": True,
        "performance_certified": False,
        "performance_gate": "pending; evidence contents and 60fps are not validated",
        "passed": not errors and not unqualified,
        "gui_apps": len(apps),
        "excluded_apps": len(excluded),
        "unqualified_apps": unqualified,
        "unqualified_count": len(unqualified),
        "errors": errors,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--inventory", type=Path, help="Defaults to ROOT/scripts/perf/app-suite.json")
    args = parser.parse_args()
    root = args.root.resolve()
    path = args.inventory or root / "scripts/perf/app-suite.json"
    try:
        result = validate(root, json.loads(path.read_text()))
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        result = {"coverage_only": True, "performance_certified": False,
                  "passed": False, "errors": [f"Invalid inventory or manifest: {error}"]}
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
