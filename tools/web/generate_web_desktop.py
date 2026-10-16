#!/usr/bin/env python3
"""Generate the browser desktop bundle's application registry from manifests.

Every app under apps/ whose tilo-app.toml lists "web" in `package.platforms`
(the default when the key is absent) is linked into
crates/browser/tilo-web-desktop and registered deferred at start: its code is
split into a module of its own, fetched when the app first opens. The
generator owns:

- the dependency block between the `# generated apps` markers in the bundle's
  Cargo.toml,
- src/apps.rs (the registration list),
- apps.json (id, name, category, version and the split `module` the app's
  code is in, for discovery before the wasm module loads, and for a page that
  fetches an app's module beside the start module; `unavailable` lists the other apps with their platforms, so
  a page can show what only runs on Tilo OS or a native desktop).

An app whose default features still reach native-only crates (a feature not
yet routed through a platform crate) lists the features the page build uses
in its Cargo.toml:

    [package.metadata.tilo]
    web-features = ["..."]

and is then linked with `default-features = false` and exactly those.
Features that name a platform crate's backend (`media/decode`, `ui/portal`)
need no entry: platform crates compile them to nothing on the web.

It writes the whole registry (id, name, category, platforms, keywords) to
crates/session/desktop-search/src/catalog.rs, so the browser desktop and the
native shell list the apps they cannot run with the reason the Cloud console
shows.

It also links the same apps into crates/agent/agent-participant (the
dependency block in its Cargo.toml and src/web_apps.rs), so an agent in a
browser session can open exactly the apps the page can mount.

`--check` fails when any of them is stale, for CI.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
BUNDLE = REPO / "crates/browser/tilo-web-desktop"
AGENTS = REPO / "crates/agent/agent-participant"
DEFAULT_PLATFORMS = ("linux", "macos", "web")
BEGIN = "# generated apps: begin (tools/web/generate_web_desktop.py)"
END = "# generated apps: end"


def search_keywords(app_dir: pathlib.Path) -> list[str]:
    """Every language's words from `search/*.ftl`, English first.

    Those files are the only source of an app's search keywords; the packer
    writes them into the signed manifest (tilo-app-package-cli), so
    `tilo-app.toml` carries none.
    """
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


def all_apps(repo: pathlib.Path) -> list[dict]:
    apps = []
    for manifest_path in sorted((repo / "apps").glob("*/tilo-app.toml")):
        app_dir = manifest_path.parent
        manifest = tomllib.loads(manifest_path.read_text())
        package = manifest.get("package", {})
        if package.get("kind", "app") != "app":
            continue
        platforms = package.get("platforms", list(DEFAULT_PLATFORMS))
        cargo = tomllib.loads((app_dir / "Cargo.toml").read_text())
        crate = cargo["package"]["name"]
        web_features = cargo["package"].get("metadata", {}).get("tilo", {}).get("web-features")
        lib = cargo.get("lib", {}).get("name", crate.replace("-", "_"))
        lib_path = app_dir / cargo.get("lib", {}).get("path", "src/lib.rs")
        apps.append(
            dict(
                dir=app_dir.name,
                crate=crate,
                lib=lib,
                lib_path=lib_path,
                id=package["id"],
                name=package.get("name", package["id"]),
                category=package.get("category", ""),
                version=package.get("version", ""),
                web_features=web_features,
                platforms=list(platforms),
                keywords=search_keywords(app_dir),
            )
        )
    return apps


def web_apps(repo: pathlib.Path) -> list[dict]:
    return [a for a in all_apps(repo) if "web" in a["platforms"]]


def cargo_block(apps: list[dict]) -> str:
    lines = [BEGIN]
    for app in apps:
        spec = f'path = "../../../apps/{app["dir"]}"'
        if app["web_features"] is not None:
            features = ", ".join(json.dumps(f) for f in app["web_features"])
            spec += f", default-features = false, features = [{features}]"
        lines.append(f'{app["crate"]} = {{ {spec} }}')
    lines.append(END)
    return "\n".join(lines)


def split_point(app: dict) -> tuple[str, str]:
    """The app's module name and the splitter's hash for its split point."""
    return app["lib"], hashlib.sha256(app["id"].encode()).hexdigest()[:16]


def apps_rs(apps: list[dict]) -> str:
    text = (
        "//! Generated by `tools/web/generate_web_desktop.py` from `apps/*/tilo-app.toml`.\n"
        "//! Do not edit; declare `platforms` in an app's manifest instead.\n"
        "//!\n"
        "//! Every app registers deferred and is loaded from its own module when it\n"
        "//! first opens (`crate::split`); the page may fetch that module earlier\n"
        "//! (`apps.json` names it).\n"
        "\n"
        "/// Register every web-capable application with the page runtime, in\n"
        "/// launcher order.\n"
        "pub(crate) fn register() {\n"
    )
    for app in apps:
        text += f"    {app['lib']}_split::register();\n"
    text += "}\n"
    for app in apps:
        lib = app["lib"]
        module, digest = split_point(app)
        symbol = f"__wasm_split_00___{module}___00_{{}}_{digest}_definition"
        text += (
            "\n"
            f"mod {lib}_split {{\n"
            "    use std::ffi::c_void;\n"
            "\n"
            "    use ui::application::ApplicationDefinition;\n"
            "    use ui::application::registry::{self, Loading};\n"
            "\n"
            "    use crate::split::Callback;\n"
            "\n"
            '    #[link(wasm_import_module = "./__wasm_split.js")]\n'
            '    #[allow(improper_ctypes, reason = "the definition only passes between Rust")]\n'
            '    unsafe extern "C" {\n'
            f'        #[link_name = "__wasm_split_load_{module}_{digest}_definition"]\n'
            "        fn load(callback: Callback, data: *const c_void);\n"
            f'        #[link_name = "{symbol.format("import")}"]\n'
            "        fn definition() -> *const ApplicationDefinition;\n"
            "    }\n"
            "\n"
            f'    #[unsafe(export_name = "{symbol.format("export")}")]\n'
            '    #[allow(improper_ctypes_definitions, reason = "called only from Rust")]\n'
            '    extern "C" fn export() -> *const ApplicationDefinition {\n'
            f"        {lib}::definition()\n"
            "    }\n"
            "\n"
            "    fn open() -> Loading {\n"
            "        Box::pin(crate::split::definition(load, definition))\n"
            "    }\n"
            "\n"
            "    pub(super) fn register() {\n"
            f"        registry::defer({json.dumps(app['id'])}, {json.dumps(app['name'], ensure_ascii=False)}, open);\n"
            "    }\n"
            "}\n"
        )
    return text


def web_apps_rs(apps: list[dict]) -> str:
    calls = "".join(f"        {app['lib']}::definition(),\n" for app in apps)
    return (
        "//! Generated by `tools/web/generate_web_desktop.py` from `apps/*/tilo-app.toml`.\n"
        "//! Do not edit; declare `platforms` in an app's manifest instead.\n"
        "\n"
        "use ui::application::ApplicationDefinition;\n"
        "\n"
        "/// Every application the browser desktop registers, in the same order.\n"
        "#[must_use]\n"
        "pub fn web_apps() -> Vec<&'static ApplicationDefinition> {\n"
        "    vec![\n"
        f"{calls}"
        "    ]\n"
        "}\n"
    )


def rust_list(field: str, items: list[str]) -> str:
    """`field: &[items],` as rustfmt lays it out at struct-field depth.

    On one line when it fits the 100-column width; otherwise one item per
    line, or, when every item is at most 10 columns wide, as many per line
    as fit (rustfmt's `short_array_element_width_threshold`). The generated
    file is then already formatted, so `cargo fmt --check` and `--check`
    here agree.
    """
    one_line = f"        {field}: &[{', '.join(items)}],"
    if len(one_line) <= 100:
        return one_line + "\n"
    indent = " " * 12
    if all(len(item) <= 10 for item in items):
        lines: list[str] = []
        line = ""
        for item in items:
            candidate = f"{line} {item}," if line else f"{item},"
            if line and len(indent) + len(candidate) > 100:
                lines.append(line)
                line = f"{item},"
            else:
                line = candidate
        lines.append(line)
    else:
        lines = [f"{item}," for item in items]
    body = "".join(f"{indent}{line}\n" for line in lines)
    return f"        {field}: &[\n{body}        ],\n"


def catalog_rs(apps: list[dict]) -> str:
    def text(value: str) -> str:
        return json.dumps(value, ensure_ascii=False)

    entries = "".join(
        "    CatalogApp {\n"
        f"        id: {text(a['id'])},\n"
        f"        name: {text(a['name'])},\n"
        f"        category: {text(a['category'])},\n"
        + rust_list("platforms", [text(p) for p in a["platforms"]])
        + rust_list("keywords", [text(k) for k in a["keywords"]])
        + "    },\n"
        for a in apps
    )
    return (
        "//! @generated by `tools/web/generate_web_desktop.py` from `apps/*/tilo-app.toml`.\n"
        "//! Do not edit; declare `platforms` in an app's manifest and keywords in its search/*.ftl instead.\n"
        "\n"
        "use super::CatalogApp;\n"
        "\n"
        "pub(super) const APPS: &[CatalogApp] = &[\n"
        f"{entries}"
        "];\n"
    )


def apps_json(apps: list[dict]) -> str:
    entries = [
        dict(id=a["id"], name=a["name"], category=a["category"], version=a["version"],
             module=split_point(a)[0])
        for a in apps
        if "web" in a["platforms"]
    ]
    unavailable = [
        dict(id=a["id"], name=a["name"], category=a["category"], platforms=a["platforms"])
        for a in apps
        if "web" not in a["platforms"]
    ]
    return json.dumps(dict(schema=1, applications=entries, unavailable=unavailable), indent=2) + "\n"


def outputs(repo: pathlib.Path) -> tuple[dict[pathlib.Path, str], list[str]]:
    everything = all_apps(repo)
    apps = [a for a in everything if "web" in a["platforms"]]
    problems = [
        f"{a['dir']}: declares web but its library exports no `pub fn definition()`"
        for a in apps
        # `ui::dev_reload_entry!(App)` defines `pub fn definition()` too.
        if not re.search(
            r"^(pub (fn definition\(\)|use [\w:]*\bdefinition;)|ui::dev_reload_entry!\()",
            a["lib_path"].read_text(),
            re.M,
        )
    ]
    bundle = repo / "crates/browser/tilo-web-desktop"
    agents = repo / "crates/agent/agent-participant"
    block = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END), re.S)
    manifests = {}
    for crate in (bundle, agents):
        manifest = (crate / "Cargo.toml").read_text()
        if not block.search(manifest):
            problems.append(f"{crate / 'Cargo.toml'}: generated-apps markers missing")
        manifests[crate / "Cargo.toml"] = block.sub(lambda _: cargo_block(apps), manifest)
    return (
        {
            **manifests,
            bundle / "src/apps.rs": apps_rs(apps),
            bundle / "apps.json": apps_json(everything),
            repo / "crates/session/desktop-search/src/catalog.rs": catalog_rs(everything),
            agents / "src/web_apps.rs": web_apps_rs(apps),
        },
        problems,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if outputs are stale")
    parser.add_argument("--repo", type=pathlib.Path, default=REPO)
    args = parser.parse_args()
    files, problems = outputs(args.repo)
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    stale = [path for path, text in files.items() if not path.exists() or path.read_text() != text]
    if args.check:
        for path in stale:
            print(f"error: {path.relative_to(args.repo)} is stale; run {pathlib.Path(__file__).relative_to(REPO)}", file=sys.stderr)
        return 1 if stale or problems else 0
    for path in stale:
        path.write_text(files[path])
        print(f"wrote {path.relative_to(args.repo)}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
