#!/usr/bin/env python3
"""Generate the shared app catalogue's manifest metadata for the Tilo fleet.

Every Tilo app in `apps/` contributes what search and the Store show for it
whether or not a device has it: its tag line, the names and words people know
it by, the kinds of resource it creates ("New workbook") and the actions it
offers while closed (`[[offers]]`). The sources are the ones the packer signs
into each app's manifest:

* `apps/<app>/tilo-app.toml`: id, name, `[package.names]`, `[resources]`,
  `[[offers]]`;
* `apps/<app>/surface.toml`: each resource kind's label;
* `apps/<app>/search/*.ftl`: `search-keywords`, and `app-tagline` (English
  here), the tag line the packer signs into `[package.taglines]`.

Writes `crates/packaging/app-catalogue/src/fleet_generated.rs`. With `--check`
it fails when that file is out of date instead.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "crates/packaging/app-catalogue/src/fleet_generated.rs"


def search_keywords(app_dir: pathlib.Path) -> list[str]:
    """Every language's `search-keywords`, English first, without repeats."""
    message = re.compile(r"^search-keywords\s*=\s*(.+)$", re.M)
    words: list[str] = []
    files = sorted((app_dir / "search").glob("*.ftl"), key=lambda p: (p.stem != "en", p.stem))
    for source in files:
        found = message.search(source.read_text(encoding="utf-8"))
        if not found:
            continue
        for word in (w.strip() for w in found.group(1).split(",")):
            if word and word.lower() not in {w.lower() for w in words}:
                words.append(word)
    return words


def tagline(app_dir: pathlib.Path) -> str:
    """The app's English `app-tagline`, as the packer signs it."""
    source = app_dir / "search" / "en.ftl"
    if not source.is_file():
        return ""
    found = re.search(r"^app-tagline\s*=\s*(.+)$", source.read_text(encoding="utf-8"), re.M)
    return found.group(1).strip() if found else ""


def other_names(package: dict) -> list[str]:
    name = package.get("name", package["id"])
    names: list[str] = []
    for _, localized in sorted(package.get("names", {}).items()):
        if localized.lower() != name.lower() and localized.lower() not in {n.lower() for n in names}:
            names.append(localized)
    return names


def resource_types(app_dir: pathlib.Path, manifest: dict, package: dict) -> list[tuple[str, str]]:
    """The documents the app creates, with their labels ("Workbook").

    Only an app that opens files makes documents. Its kind is its own when
    it lives in the app's namespace (`sheets.workbook` for Sheets); a kind it
    only uses (`settings.locale`) is another app's. A per-person kind
    (`person = true`: a watch history, a settings pane) exists once and is
    never new, and a kind labelled with the app's own name is the app itself.
    """
    providers = manifest.get("providers", {})
    if not providers.get("ext") and not providers.get("mime"):
        return []
    surface_path = app_dir / "surface.toml"
    if not surface_path.is_file():
        return []
    surface = tomllib.loads(surface_path.read_text(encoding="utf-8"))
    labels = {
        resource["kind"]: resource.get("label", "")
        for resource in surface.get("resources", [])
        if isinstance(resource, dict) and "kind" in resource and not resource.get("person", False)
    }
    namespaces = {app_dir.name, package["id"].rsplit(".", 1)[-1]}
    name = package.get("name", package["id"]).strip().lower()
    out: list[tuple[str, str]] = []
    for kind in manifest.get("resources", {}).get("kinds", []):
        label = labels.get(kind, "").strip()
        if not label or kind.split(".", 1)[0] not in namespaces or label.lower() == name:
            continue
        if kind not in {k for k, _ in out}:
            out.append((kind, label))
    return out


def offers(manifest: dict) -> list[dict]:
    return [
        dict(
            resource=offer["resource"],
            action=offer["action"],
            label=offer["label"],
            description=offer.get("description", ""),
        )
        for offer in manifest.get("offers", [])
        if isinstance(offer, dict) and {"resource", "action", "label"} <= offer.keys()
    ]


def fleet(root: pathlib.Path) -> list[dict]:
    apps = []
    for manifest_path in sorted((root / "apps").glob("*/tilo-app.toml")):
        app_dir = manifest_path.parent
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        package = manifest.get("package", {})
        if package.get("kind", "app") != "app" or "id" not in package:
            continue
        entry_type = manifest.get("entrypoint", {}).get("type", "gui")
        if entry_type != "gui":
            continue
        apps.append(
            dict(
                id=package["id"],
                tagline=tagline(app_dir),
                names=other_names(package),
                keywords=search_keywords(app_dir),
                resource_types=resource_types(app_dir, manifest, package),
                actions=offers(manifest),
            )
        )
    return apps


def text(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def strings(items: list[str]) -> str:
    return "&[" + ", ".join(text(item) for item in items) + "]"


def render(apps: list[dict]) -> str:
    lines = [
        "//! @generated by `tools/apps/generate_catalogue_fleet.py` from `apps/*/tilo-app.toml`,",
        "//! `apps/*/surface.toml` and `apps/*/search/*.ftl`.",
        "//! Do not edit; change the sources and run the generator.",
        "",
        "use super::{FleetAction, FleetApp};",
        "",
        "pub(super) const APPS: &[FleetApp] = &[",
    ]
    for app in apps:
        lines.append("    FleetApp {")
        lines.append(f"        id: {text(app['id'])},")
        lines.append(f"        tagline: {text(app['tagline'])},")
        lines.append(f"        names: {strings(app['names'])},")
        lines.append(f"        keywords: {strings(app['keywords'])},")
        types = ", ".join(f"({text(kind)}, {text(label)})" for kind, label in app["resource_types"])
        lines.append(f"        resource_types: &[{types}],")
        if app["actions"]:
            lines.append("        actions: &[")
            for action in app["actions"]:
                lines.append(
                    "            FleetAction {"
                    f" resource: {text(action['resource'])},"
                    f" action: {text(action['action'])},"
                    f" label: {text(action['label'])},"
                    f" description: {text(action['description'])} }},"
                )
            lines.append("        ],")
        else:
            lines.append("        actions: &[],")
        lines.append("    },")
    lines.append("];")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail when the generated file is stale")
    args = parser.parse_args()
    # Formatted as `cargo fmt` would, so the format check and `--check` agree.
    rendered = subprocess.run(
        ["rustfmt", "--edition", "2024", "--emit", "stdout"],
        input=render(fleet(ROOT)),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    if args.check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.is_file() else ""
        if current != rendered:
            print(f"{OUTPUT.relative_to(ROOT)} is stale: run tools/apps/generate_catalogue_fleet.py", file=sys.stderr)
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
