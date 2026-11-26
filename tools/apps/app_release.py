#!/usr/bin/env python3
"""Plan independently buildable tapp app releases and named collections."""

from __future__ import annotations

import argparse
from collections.abc import Iterable
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

TOOLS = Path(__file__).resolve().parents[1]
ROOT = TOOLS.parent
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from ci import runner_labels
from ci import workspace_graph as ci_select


CONFIG = ROOT / "dist/apps/app-collections.toml"
BINARY_INPUTS = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "dist/apps/app-collections.toml",
    "tools/apps/app_package_isa.py",
    "tools/ci/rustc-thin-app-wrapper.sh",
    "tools/ci/runtime-dylib-lazy-link.py",
    "os/scripts/build-thin-apps.sh",
    "os/Containerfile.builder",
    "os/external-assets/github-assets.tsv",
    "os/dev/cache-github-assets.sh",
    "tools/ci/check-build-env.sh",
    "tools/ci/resolve_builder.py",
    "tools/ci/images.toml",
    "tools/ci/python.py",
    "tools/ci/sccache.sh",
}
PACKAGING_INPUTS = {
    "dist/apps/package-apps.sh",
    # The tilolang packages' build and the runtime bundle they run on: a change
    # to either changes every tilolang tapp the collection ships.
    "tools/apps/tilolang_packages.sh",
    "dist/apps/assemble-collection.sh",
    "tools/apps/assemble_app_collection.py",
    "os/scripts/bake-runtime-bundle.sh",
    "os/scripts/collect-runtime-dependencies.py",
    "os/scripts/stamp-runtime.sh",
    "tools/apps/stage_sdk_services.py",
}
VERIFICATION_INPUTS = {
    "tools/apps/app_release.py",
    "tools/ci/__init__.py",
    "tools/ci/workspace_graph.py",
    "tools/ci/runner_labels.py",
    "tools/apps/app_channel_manifest.py",
    "tools/apps/canonical_app_verifier.py",
    "tools/apps/final_app_receipt.py",
    "os/scripts/verify-elf-hardening.sh",
    "os/scripts/verify-runtime-bundle-closure.sh",
    "tools/canonical_verifier_bundle.py",
}
GLOBAL_INPUTS = BINARY_INPUTS | PACKAGING_INPUTS | VERIFICATION_INPUTS
GLOBAL_PREFIXES = (".cargo/", "vendor/", "tools/web/wasm-split/")
FULL_REBUILD_PREFIXES = (
    "dist/apps/",
    "tools/apps/",
)
# These are explicitly built by package-apps.sh, not ordinary app dependencies.
# A change reaching their graph affects the shared runtime or every emitted tapp.
FLEET_BUILD_PACKAGES = {"runtime-dylib", "tilo-app-package-cli", "tilo-runtime-bundle"}
# Runtime engines are built with every runtime and signed into its
# bundle, so a change reaching one changes the runtime every app names.
ENGINES = "crates/engines/loadable"


def runtime_engines(root: Path = ROOT) -> set[str]:
    """The packages under crates/engines/loadable/, which build-thin-apps.sh builds."""
    return {
        tomllib.loads(manifest.read_text())["package"]["name"]
        for manifest in sorted((root / ENGINES).glob("*/Cargo.toml"))
    }


def architecture_matrix(
    ref: str,
    event: str,
    requested: str = "",
    main_architectures: str = "",
) -> dict:
    if ref.startswith("refs/tags/"):
        raw = "x86_64 aarch64"
    elif ref == "refs/heads/stable" and event == "push":
        # A promotion publishes the collection for both native architectures;
        # the variable narrows it while one architecture's slot is unavailable.
        raw = main_architectures or "x86_64 aarch64"
    else:
        raw = requested or "x86_64 aarch64"
    architectures = sorted(set(filter(None, re.split(r"[ ,]+", raw))))
    invalid = set(architectures).difference({"x86_64", "aarch64"})
    if not architectures or invalid:
        detail = ", ".join(sorted(invalid)) or "empty selection"
        raise ValueError(f"architectures must contain only x86_64 and/or aarch64: {detail}")
    # Tools from the first architecture run on its small runner; compiling the
    # whole collection is CPU-bound and takes the large one.
    return {
        "include": [
            {
                "arch": architecture,
                "runner": runner_labels.NATIVE[architecture],
                "build_runner": runner_labels.LARGE[architecture],
            }
            for architecture in architectures
        ]
    }


def kind_of(app: dict) -> str:
    """What an app is: a native build or a tilolang package.

    A catalog a fixture builds by hand need not say, and every app it names is native.
    """
    return app.get("kind", "native")


def tilolang_runtime(root: Path = ROOT) -> str | None:
    """The tilolang runtime every tilolang app executes, as its bundle provides it: the build
    its manifest declares, keyed by the standard library it carries
    (`tilolang/<version>+tilolang.<std digest>`, as tools/apps/tilolang_packages.sh and
    `tilolang_tilo::runtime` name it).

    `None` for a tree that has no tilolang runtime (a fixture root, a collection that only
    reads a few apps).
    """
    manifest = root / "crates/lang/tilolang-process/tilo-runtime.toml"
    if not manifest.is_file():
        return None
    provides = tomllib.loads(manifest.read_text())["package"]["provides"]
    if not provides.startswith("tilolang/"):
        raise ValueError(f"{manifest}: provides {provides!r}, not a tilolang runtime")
    return f"{provides}.{tilolang_std_digest(root)}"


def tilolang_std_digest(root: Path = ROOT) -> str:
    """The digest of the standard library in `root` (`tilolang::stdlib::digest`): the first 16
    hex digits of the SHA-256 of its `app.tl` followed by its `app.ids`."""
    std = root / "crates/lang/tilolang/std"
    return hashlib.sha256((std / "app.tl").read_bytes() + (std / "app.ids").read_bytes()).hexdigest()[:16]


def tilolang_catalog(root: Path = ROOT) -> dict[str, dict[str, str]]:
    """The tilolang packages of `apps/tl`, under the name `tl-<directory>`.

    A package has no Cargo package and no binary of its own: `tilolang build` compiles it into
    its tapp, which runs on the tilolang runtime bundle. Its id and title are what its `app`
    declaration says, as the language's own parser reads them.
    """
    result: dict[str, dict[str, str]] = {}
    runtime = tilolang_runtime(root)
    if runtime is None:
        return result
    for source in sorted((root / "apps/tl").glob("*/app.tl")):
        if not (source.parent / "app.ids").is_file():
            continue
        text = source.read_text()
        declared = re.search(r'^\s*app\s+(\S+)\s+"([^"]*)"', text, re.MULTILINE)
        if declared is None:
            raise ValueError(f"{source}: no `app` declaration")
        app_id, title = declared.group(1), declared.group(2)
        uses = sorted(set(re.findall(r"^\s*use\s+(\S+)\s+as\s+\S+", text, re.MULTILINE)))
        name = f"tl-{source.parent.name}"
        manifest = source.parent / "app.toml"
        optional_runtimes = []
        if manifest.is_file():
            optional_runtimes = (
                tomllib.loads(manifest.read_text())["package"].get("optional-runtimes", [])
            )
        result[name] = {
            "name": name,
            "kind": "tilolang",
            "package": "",
            "binary": "",
            "id": app_id,
            "title": title,
            "version": "0.1.0",
            "runtime": "tilo-runtime/2026.07",
            "tilolang_runtime": runtime,
            "optional_runtimes": optional_runtimes,
            "manifest": str(source.relative_to(root)),
            "dir": str(source.parent.relative_to(root)),
            "uses": uses,
        }
    return result


def app_catalog(root: Path = ROOT) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for manifest in sorted((root / "apps").glob("*/tilo-app.toml")):
        directory = manifest.parent
        cargo = tomllib.loads((directory / "Cargo.toml").read_text())
        app = tomllib.loads(manifest.read_text())
        name = directory.name
        binary = Path(app["entrypoint"]["bin"]).name
        result[name] = {
            "name": name,
            "kind": "native",
            "package": cargo["package"]["name"],
            "binary": binary,
            "id": app["package"]["id"],
            "version": app["package"]["version"],
            "runtime": app["package"]["runtime"],
            "optional_runtimes": app["package"].get("optional-runtimes", []),
            "manifest": str(manifest.relative_to(root)),
        }
    result.update(tilolang_catalog(root))
    if not result:
        raise ValueError("no apps/*/tilo-app.toml manifests found")
    # One app per id: a tilolang package takes over a native app's id only in the change that
    # deletes the native app, so a collection can never ship both as one name in the launcher.
    claimed: dict[str, str] = {}
    for name, app in sorted(result.items()):
        earlier = claimed.setdefault(app["id"], name)
        if earlier != name:
            raise ValueError(
                f"apps {earlier} and {name} both claim the id {app['id']}"
            )
    return result


def config(path: Path = CONFIG) -> dict:
    return tomllib.loads(path.read_text())


def resolve_collection(name: str, catalog: dict[str, dict], cfg: dict) -> list[str]:
    collections = cfg.get("collections", {})
    visiting: set[str] = set()

    def visit(current: str) -> set[str]:
        if current in visiting:
            raise ValueError(f"collection cycle at {current}")
        if current not in collections:
            raise ValueError(f"unknown app collection: {current}")
        visiting.add(current)
        spec = collections[current]
        selected: set[str] = set()
        for parent in spec.get("extends", []):
            selected.update(visit(parent))
        includes = spec.get("include", [])
        if "*" in includes:
            # `*` is every native app. A tilolang package is named beside it (`tl-notes`), in
            # the change that retires the native app it replaces, so the collection never grows
            # a package nothing asked for.
            selected.update(
                name for name, app in catalog.items() if kind_of(app) == "native"
            )
        selected.update(item for item in includes if item != "*")
        selected.difference_update(spec.get("exclude", []))
        visiting.remove(current)
        return selected

    selected = visit(name)
    unknown = selected.difference(catalog)
    if unknown:
        raise ValueError(f"collection {name} names unknown apps: {', '.join(sorted(unknown))}")
    return sorted(selected)


def with_used_packages(selected: Iterable[str], catalog: dict[str, dict]) -> set[str]:
    """`selected` and every tilolang package they use, transitively.

    A tilolang package is built with every package it uses, and each one is a signed app of
    its own, so a selection brings its packages along.
    """
    selected = set(selected)
    tilolang_by_id = {
        app["id"]: name for name, app in catalog.items() if kind_of(app) == "tilolang"
    }
    frontier = [
        name
        for name in selected
        if kind_of(catalog[name]) == "tilolang" and catalog[name].get("uses")
    ]
    while frontier:
        for used in catalog[frontier.pop()]["uses"]:
            dependency = tilolang_by_id.get(used)
            if dependency is not None and dependency not in selected:
                selected.add(dependency)
                frontier.append(dependency)
    return selected


def changed_paths(base: str, root: Path = ROOT) -> list[str]:
    # Share the NUL-delimited, rename-as-delete/add reader used by check CI.
    return ci_select.changed_files(root, base, "HEAD")


def lockfile_affected_packages(
    base: str,
    metadata: dict,
    root: Path = ROOT,
) -> set[str] | None:
    """Return workspace packages reached by the changed lockfile records.

    Cargo.lock contains the complete external dependency graph, while the cheap
    ``cargo metadata --no-deps`` result contains each workspace package's direct
    dependencies. Joining the two by Cargo package name lets a lockfile-only
    update select the workspace reverse closure instead of every app. Duplicate
    external versions collapse to one name in the join, which can over-select
    but cannot omit another version's consumers.

    ``None`` means the comparison could not be proved. Callers retain the old
    full-fleet behavior in that case.
    """
    merge_base = ci_select.comparison_merge_base(root, base, "HEAD")
    before = subprocess.run(
        ["git", "show", f"{merge_base}:Cargo.lock"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if before.returncode:
        return None
    try:
        after = (root / "Cargo.lock").read_bytes()
    except OSError:
        return None
    packages, _ = ci_select.package_maps(metadata, root)
    change = ci_select.lockfile_change(before.stdout, after, packages)
    if change is None:
        return None
    affected_names = change.reached | change.seeds
    if not affected_names:
        return set()
    workspace_seeds = {
        name
        for name, package in packages.items()
        if name in affected_names
        or any(
            dependency["name"] in affected_names
            for dependency in package.get("dependencies", [])
        )
    }
    return ci_select.closure(
        workspace_seeds,
        ci_select.reverse_dependencies(packages, root),
    )


def input_classification(paths: list[str], metadata: dict | None = None,
                         root: Path = ROOT) -> dict[str, list[str]]:
    """Artifact invalidation versus metadata revalidation, not compile commands.

    The current executor rebuilds for all three classes. Keeping them distinct
    allows a later trusted replay worker to reuse binaries for receipt-only work.
    """
    classified = {
        "binary": sorted({p for p in paths if p in BINARY_INPUTS or p.startswith(GLOBAL_PREFIXES)}),
        "packaging": sorted(set(paths) & PACKAGING_INPUTS),
        "verification": sorted(set(paths) & VERIFICATION_INPUTS),
    }
    if metadata is not None:
        packages, by_dir = ci_select.package_maps(metadata, root)
        reverse = ci_select.reverse_dependencies(packages, root)
        app_packages = {name for directory, name in by_dir.items()
                        if directory.parts and directory.parts[0] == "apps"}
        for path in paths:
            owner = ci_select.owner_for(path, by_dir)
            if owner is not None:
                affected = ci_select.closure({owner}, reverse)
                packaging = bool(affected & {"tilo-app-package-cli", "tilo-runtime-bundle"})
                if packaging:
                    classified["packaging"].append(path)
                if not packaging or affected & (app_packages | {"runtime-dylib"}):
                    classified["binary"].append(path)
            elif path.startswith(("apps/", "crates/", "fs/")):
                classified["binary"].append(path)
    return {kind: sorted(set(inputs)) for kind, inputs in classified.items()}


def apps_from_changes(
    paths: list[str],
    catalog: dict[str, dict],
    metadata: dict | None = None,
    root: Path = ROOT,
    lock_affected_packages: set[str] | None = None,
) -> list[str]:
    # A tilolang package is its directory under apps/tl. One that declares an app selects
    # itself; a library beside it is built into every package that uses it, so its change
    # selects every tilolang app, as does a change to the toolchain or runtime they run on.
    tilolang = {
        name for name, app in catalog.items() if kind_of(app) == "tilolang"
    }
    shared = any(
        path.startswith(("crates/lang/tilolang", "crates/platform/tilolang"))
        for path in paths
    )
    if any(
        (
            path in GLOBAL_INPUTS
            and not (path == "Cargo.lock" and lock_affected_packages is not None)
        )
        or path.startswith(GLOBAL_PREFIXES + FULL_REBUILD_PREFIXES)
        # Thin apps and the runtime resolve the complete collection together.
        # App dependency/feature changes can alter shared crate hashes even
        # when the changed app is not a dependency of runtime-dylib itself.
        or (path.startswith("apps/") and path.endswith("/Cargo.toml"))
        for path in paths
    ):
        return sorted(catalog)
    selected = {
        parts[1]
        for path in paths
        if len(parts := Path(path).parts) >= 2
        and parts[0] == "apps"
        and parts[1] in catalog
    }
    # A tilolang package is its directory under apps/tl. One that declares an app selects
    # itself; a library beside it is built into every package that uses it, so its change
    # selects every tilolang app, as does a change to the runtime the packages execute.
    tilolang = {
        name for name, app in catalog.items() if kind_of(app) == "tilolang"
    }
    for path in paths:
        parts = Path(path).parts
        if len(parts) >= 3 and parts[0] == "apps" and parts[1] == "tl":
            name = f"tl-{parts[2]}"
            if name in catalog:
                selected.add(name)
            else:
                selected.update(tilolang)
    if shared:
        selected.update(tilolang)
    if metadata is not None:
        packages, by_dir = ci_select.package_maps(metadata, root)
        if any(path.startswith(("apps/", "crates/", "fs/"))
               and ci_select.owner_for(path, by_dir) is None for path in paths):
            return sorted(catalog)
        seeds = {
            owner
            for path in paths
            if (owner := ci_select.owner_for(path, by_dir)) is not None
        }
        seeds.update(lock_affected_packages or ())
        affected = ci_select.closure(seeds, ci_select.reverse_dependencies(packages, root))
        if affected & (FLEET_BUILD_PACKAGES | runtime_engines(root)):
            return sorted(catalog)
        selected.update(
            name for name, app in catalog.items() if app["package"] in affected
        )
    return sorted(selected)


def prior_collection_base(source: str, collection_builder: str, builder: str) -> str | None:
    """The commit a main push may select a partial rebuild relative to, or None.

    A partial rebuild reuses the published collection's other tapps and needs
    its shared runtime to be byte-identical to the one built now. That runtime
    is compiled from source, which the commit range selects on, and carries
    system libraries from the builder image, which no source diff shows. So a
    collection built with another builder image, or with an unrecorded one,
    must be rebuilt in full; publication would otherwise refuse the changed
    runtime after every app was built.
    """
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        return None
    if not builder or collection_builder != builder:
        return None
    return source


def app_from_tag(tag: str, catalog: dict[str, dict]) -> str | None:
    match = re.fullmatch(r"app-(.+)-v[0-9].*", tag)
    if not match:
        return None
    name = match.group(1)
    if name not in catalog:
        raise ValueError(f"tag selects unknown app: {name}")
    return name


def plan(args: argparse.Namespace) -> dict:
    catalog = app_catalog(args.root)
    cfg = config(args.config)
    tag_app = app_from_tag(args.tag, catalog) if args.tag else None
    classification = {"binary": [], "packaging": [], "verification": []}
    if tag_app:
        tag_version = args.tag.rsplit("-v", 1)[1]
        if catalog[tag_app]["version"] != tag_version:
            raise ValueError(
                f"tag version {tag_version} does not match {tag_app} manifest "
                f"version {catalog[tag_app]['version']}"
            )
        selected = [tag_app]
        reason = f"component tag selected {tag_app}"
    elif args.apps:
        selected = sorted(set(filter(None, re.split(r"[ ,]+", args.apps))))
        reason = "explicit app selection"
    elif args.base:
        changes = changed_paths(args.base, args.root)
        metadata = ci_select.cargo_metadata(args.root)
        lock_affected = (
            lockfile_affected_packages(args.base, metadata, args.root)
            if "Cargo.lock" in changes
            else None
        )
        classification = input_classification(changes, metadata, args.root)
        selected = apps_from_changes(
            changes,
            catalog,
            metadata,
            args.root,
            lock_affected,
        )
        reason = "apps affected by the commit range"
        kinds = [kind for kind, inputs in classification.items() if inputs]
        if kinds:
            reason = "release inputs changed: " + ", ".join(kinds)
    else:
        selected = resolve_collection(args.collection, catalog, cfg)
        reason = f"{args.collection} collection"
    unknown = set(selected).difference(catalog)
    if unknown:
        raise ValueError(f"unknown apps: {', '.join(sorted(unknown))}")
    selected = sorted(with_used_packages(selected, catalog))
    features = cfg.get("features", {})
    for name in selected:
        if kind_of(catalog[name]) != "native":
            continue
        cargo_manifest = args.root / "apps" / name / "Cargo.toml"
        if not features.get(name) or not cargo_manifest.is_file():
            continue
        declared = tomllib.loads(cargo_manifest.read_text()).get("features", {})
        undeclared = [feature for feature in features[name] if feature not in declared]
        if undeclared:
            raise ValueError(
                f"{args.config.name} selects features {name} does not declare: "
                + ", ".join(undeclared)
            )
    cargo_features = [
        f"{catalog[name]['package']}/{feature}"
        for name in selected
        if kind_of(catalog[name]) == "native"
        for feature in features.get(name, [])
    ]
    return {
        "reason": reason,
        "input_classification": classification,
        "count": len(selected),
        "apps": selected,
        "packages": [
            catalog[name]["package"]
            for name in selected
            if kind_of(catalog[name]) == "native"
        ],
        "tilolang": [
            {
                "app": name,
                "dir": catalog[name]["dir"],
                "id": catalog[name]["id"],
                "version": catalog[name]["version"],
                "runtime": catalog[name]["tilolang_runtime"],
            }
            for name in selected
            if kind_of(catalog[name]) == "tilolang"
        ],
        "features": cargo_features,
        # Every runtime the selected apps name besides the ones baked beside
        # the Tilo runtime (the tilolang packages' own), so the packing builds
        # the packages those apps install with them.
        "runtimes": sorted(
            {
                runtime
                for name in selected
                for runtime in catalog[name].get("optional_runtimes", [])
            }
        ),
        "catalog": [catalog[name] for name in selected],
    }


def emit_outputs(result: dict, path: Path) -> None:
    values = {
        "count": str(result["count"]),
        "apps": " ".join(result["apps"]),
        "packages": " ".join(f"-p {name}" for name in result["packages"]),
        "features": ",".join(result["features"]),
        "runtimes": " ".join(result["runtimes"]),
    }
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--config", type=Path, default=CONFIG)
    parser.add_argument("--collection", default="default")
    parser.add_argument("--apps", default="")
    parser.add_argument("--base")
    parser.add_argument("--tag", default="")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--architecture-matrix", action="store_true")
    parser.add_argument("--ref", default="")
    parser.add_argument("--event", default="")
    parser.add_argument("--architectures", default="")
    parser.add_argument("--main-architectures", default="")
    parser.add_argument("--prior-collection-base", action="store_true",
                        help="print the partial-rebuild base, or nothing for a full rebuild")
    parser.add_argument("--collection-source", default="")
    parser.add_argument("--collection-builder", default="")
    parser.add_argument("--builder", default="")
    args = parser.parse_args()
    try:
        if args.prior_collection_base:
            print(prior_collection_base(
                args.collection_source, args.collection_builder, args.builder) or "")
            return 0
        if args.architecture_matrix:
            print(
                json.dumps(
                    architecture_matrix(
                        args.ref, args.event, args.architectures, args.main_architectures
                    ),
                    separators=(",", ":"),
                )
            )
            return 0
        result = plan(args)
    except (KeyError, OSError, subprocess.CalledProcessError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"app release plan: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2))
    if args.github_output:
        emit_outputs(result, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
