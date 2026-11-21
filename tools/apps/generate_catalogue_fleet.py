#!/usr/bin/env python3
"""Generate the manifest metadata of every Tilo app, compiled into app-fleet.

Every Tilo app in `apps/` contributes what search, the Store and the browser
desktop show for it whether or not a device has it: where it runs, its
category and tag line, the names and words people know it by, the kinds of
resource it creates ("New workbook") and the actions it offers while closed
(`[[offers]]`). The sources are the ones the packer signs into each app's
manifest:

* `apps/<app>/tilo-app.toml`: id, name, category, `platforms`,
  `[package.names]`, `[resources]` (its kinds, and the ones each member keeps
  as their own), `[[offers]]`;
* `apps/<app>/surface.toml`: each resource kind's label, and the kind of the
  record a window of the app opens (its first resource);
* `apps/<app>/search/*.ftl`: `search-keywords`, and `app-tagline` (English
  here), the tag line the packer signs into `[package.taglines]`;
* `apps/<app>/src/**/*.rs`: whether each window of the app shows a document of
  its own, as the app declares it (`const WINDOW_RESOURCE: bool = true;`).

A tilolang package under `apps/tl/` that declares an app contributes the same
facts, read from the package itself by `tilolang describe --fleet`: its
metadata (id, name, category, tag line, other-language names, keywords, the
kinds of record it declares and the actions it offers while closed) and the
`surface.toml` generated from it, so a tilolang app replacing a Rust one is
known as fully while it is not installed. That needs a built `tilolang`
(`--tilolang PATH`, else `$TILOLANG`, else `target/debug/tilolang`).

It also carries each app's whole `surface.toml`, so a server that runs no app
of its own (the API server, serving a hosted workspace's programs) still
compiles a program against the app's real declarations.

Writes `crates/packaging/app-fleet/src/generated.rs`. With `--check`
it fails when that file is out of date instead.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "crates/packaging/app-fleet/src/generated.rs"
# Where an app runs when its manifest names no `platforms`.
DEFAULT_PLATFORMS = ("linux", "macos", "web")


def search_keywords(app_dir: pathlib.Path) -> list[str]:
    """Every language's `search-keywords`, English first, without repeats."""
    message = re.compile(r"^search-keywords\s*=\s*(.+)$", re.M)
    words: list[str] = []
    files = sorted((app_dir / "search").glob("*.ftl"), key=lambda p: (p.stem != "en", p.stem))
    for source in files:
        found = message.search(source.read_text(encoding="utf-8"))
        if not found:
            raise ValueError(f"{source}: no `search-keywords` message")
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


def own_kinds(manifest: dict) -> list[str]:
    """The kinds each member of a workspace keeps one record of as their own."""
    return list(manifest.get("resources", {}).get("own", []))


def window_kind(app_dir: pathlib.Path) -> str:
    """The kind of the record a window of the app opens: its surface's first resource."""
    surface_path = app_dir / "surface.toml"
    if not surface_path.is_file():
        return ""
    for resource in tomllib.loads(surface_path.read_text(encoding="utf-8")).get("resources", []):
        if isinstance(resource, dict) and "kind" in resource:
            return resource["kind"]
    return ""


def windows_are_personal(app_dir: pathlib.Path) -> bool:
    """Whether a window of the app is the person's own, as the app declares it:
    `const WINDOWS_ARE_PERSONAL: bool = true;` in its `ResourceApp`."""
    declared = re.compile(r"^\s*const\s+WINDOWS_ARE_PERSONAL\s*:\s*bool\s*=\s*true\s*;", re.M)
    return any(
        declared.search(source.read_text(encoding="utf-8"))
        for source in sorted((app_dir / "src").rglob("*.rs"))
    )


def window_documents(app_dir: pathlib.Path) -> bool:
    """Whether each window of the app shows a document of its own (a workbook, a
    terminal's session) and the app keeps none for the workspace: the app's own
    declaration, `const WINDOW_RESOURCE: bool = true;` in its `ResourceApp`."""
    declared = re.compile(r"^\s*const\s+WINDOW_RESOURCE\s*:\s*bool\s*=\s*true\s*;", re.M)
    return any(
        declared.search(source.read_text(encoding="utf-8"))
        for source in sorted((app_dir / "src").rglob("*.rs"))
    )


def offers(manifest: dict) -> list[dict]:
    return [
        dict(
            resource=offer["resource"],
            resource_kind=offer.get("resource-kind", ""),
            action=offer["action"],
            label=offer["label"],
            description=offer.get("description", ""),
            kind=offer.get("kind", "undoable"),
            operation=offer.get("operation", "effect"),
            confirm=bool(offer.get("confirm", False)),
            asks=offer.get("asks", ""),
            keywords=list(offer.get("keywords", [])),
            icon=offer.get("icon", ""),
        )
        for offer in manifest.get("offers", [])
        if isinstance(offer, dict) and {"resource", "action", "label"} <= offer.keys()
    ]


def native_apps(root: pathlib.Path) -> list[dict]:
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
                name=package.get("name", package["id"]),
                category=package.get("category", ""),
                platforms=list(package.get("platforms", DEFAULT_PLATFORMS)),
                tagline=tagline(app_dir),
                names=other_names(package),
                keywords=search_keywords(app_dir),
                resource_types=resource_types(app_dir, manifest, package),
                actions=offers(manifest),
                own=own_kinds(manifest),
                window_kind=window_kind(app_dir),
                windows_personal=windows_are_personal(app_dir),
                window_documents=window_documents(app_dir),
                surface=((app_dir / "surface.toml").read_text(encoding="utf-8")
                         if (app_dir / "surface.toml").is_file() else ""),
                previously=list(package.get("previously", [])),
            )
        )
    return apps


def tilolang_apps(root: pathlib.Path, tilolang: pathlib.Path) -> list[dict]:
    """Every `apps/tl/` package that declares an app, as `tilolang describe --fleet` reads it."""
    if not tilolang.is_file():
        raise SystemExit(
            f"{tilolang}: no tilolang binary; build it with "
            "`cargo build --locked -p tilolang-cli --bin tilolang` or pass --tilolang"
        )
    apps = []
    for app_dir in sorted(p for p in (root / "apps" / "tl").iterdir() if p.is_dir()):
        if not (app_dir / "app.tl").is_file():
            continue
        found = subprocess.run(
            [str(tilolang), "describe", "--fleet", str(app_dir)],
            capture_output=True,
            text=True,
        )
        if found.returncode != 0:
            raise SystemExit(
                f"{tilolang} describe --fleet {app_dir}: {found.stderr.strip()}"
            )
        entry = json.loads(found.stdout)
        if entry is None:
            # A package of views the app it belongs to compiles in: it declares no app.
            continue
        entry["platforms"] = list(DEFAULT_PLATFORMS)
        entry["actions"] = entry.pop("offers")
        apps.append(entry)
    return apps


def claims(app: dict) -> list[str]:
    """Every id `app` answers by: its own and the ones it was published under before."""
    return [app["id"], *app["previously"]]


def fleet(root: pathlib.Path, tilolang: pathlib.Path) -> list[dict]:
    apps = native_apps(root)
    apps.extend(tilolang_apps(root, tilolang))
    claimed: dict[str, str] = {}
    for app in apps:
        for app_id in claims(app):
            if app_id in claimed:
                raise SystemExit(
                    f"{app_id} is claimed by {claimed[app_id]} and {app['id']}: one app per id"
                )
            claimed[app_id] = app["id"]
    return apps


def text(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def blob(value: str) -> str:
    """`value` as a Rust string literal, its lines kept one after another."""
    return '"' + (value.replace("\\", "\\\\").replace('"', '\\"')
                  .replace("\n", "\\n").replace("\t", "\\t")
                  .replace("\x1f", "\\u{1f}")) + '"'


def strings(items: list[str]) -> str:
    return "&[" + ", ".join(text(item) for item in items) + "]"


def render(apps: list[dict]) -> str:
    lines = [
        "//! @generated by `tools/apps/generate_catalogue_fleet.py` from `apps/*/tilo-app.toml`,",
        "//! `apps/*/surface.toml`, `apps/*/search/*.ftl`, `apps/*/src`, and `tilolang describe",
        "//! --fleet` for `apps/tl/*`. Do not edit; change the sources and run the generator.",
        "",
        "use super::FleetApp;",
        "",
        "/// `(an id an app answers by besides its own, the app it names)`.",
        "pub(super) const PREVIOUS: &[(&str, &str)] = &[",
        *(f"    ({text(old)}, {text(app['id'])})," for app in apps for old in app["previously"]),
        "];",
        "",
        "/// `(app, kind)` of each kind an app's members each keep as their own (`[resources] own`).",
        "pub(super) const OWN_KINDS: &[(&str, &str)] = &[",
        *(f"    ({text(app['id'])}, {text(kind)})," for app in apps for kind in app["own"]),
        "];",
        "",
        "/// The apps a window of which is the person's own (`WINDOWS_ARE_PERSONAL`).",
        "pub(super) const OWN_WINDOWS: &[&str] = &[",
        *(f"    {text(app['id'])}," for app in apps if app["windows_personal"]),
        "];",
        "",
        "/// The apps each window of which shows a document of its own (`ResourceApp::WINDOW_RESOURCE`).",
        "pub(super) const DOCUMENT_WINDOWS: &[&str] = &[",
        *(f"    {text(app['id'])}," for app in apps if app["window_documents"]),
        "];",
        "",
        "/// Each app's signed `surface.toml`, as its package ships it: what a task's programs",
        "/// compile against there, by the app's id. The `surfaces` feature carries it; apps and",
        "/// the web desktop leave it off.",
        '#[cfg(feature = "surfaces")]',
        "pub(super) const SURFACES: &[(&str, &str)] = &[",
        *(f"    ({text(app['id'])}, {blob(app['surface'])})," for app in apps if app["surface"]),
        "];",
        "",
        "pub(super) const APPS: &[FleetApp] = &[",
    ]
    for app in apps:
        lines.append("    FleetApp {")
        lines.append(f"        id: {text(app['id'])},")
        lines.append(f"        name: {text(app['name'])},")
        lines.append(f"        category: {text(app['category'])},")
        lines.append(f"        platforms: {strings(app['platforms'])},")
        lines.append(f"        tagline: {text(app['tagline'])},")
        lines.append(f"        names: {strings(app['names'])},")
        lines.append(f"        keywords: {strings(app['keywords'])},")
        types = ", ".join(f"({text(kind)}, {text(label)})" for kind, label in app["resource_types"])
        lines.append(f"        resource_types: &[{types}],")
        offers = "".join(
            "\t".join([
                action["resource"], action["resource_kind"], action["action"], action["label"],
                action["description"], action["kind"], action["operation"],
                "1" if action["confirm"] else "0", action["asks"],
                "\x1f".join(action["keywords"]), action["icon"],
            ]) + "\n"
            for action in app["actions"]
        )
        lines.append(f"        offers: {blob(offers)},")
        lines.append("    },")
    lines.append("];")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail when the generated file is stale")
    parser.add_argument(
        "--tilolang",
        type=pathlib.Path,
        default=pathlib.Path(os.environ.get("TILOLANG", ROOT / "target" / "debug" / "tilolang")),
        help="the `tilolang` binary the tl packages are read with, as `tilolang describe --fleet` "
        "(default: $TILOLANG, else target/debug/tilolang)",
    )
    args = parser.parse_args()
    # Formatted as `cargo fmt` would, so the format check and `--check` agree.
    rendered = subprocess.run(
        ["rustfmt", "--edition", "2024", "--emit", "stdout"],
        input=render(fleet(ROOT, args.tilolang)),
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
