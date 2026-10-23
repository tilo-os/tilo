#!/usr/bin/env python3
"""Install Tilo build outputs into a root tree, as declared by os/components.toml.

The manifest is the single record of which Tilo-built file lands where in the
OS image. This tool is how that record is applied: the production Containerfile
runs it to arrange the image's binaries, os/Containerfile.dev runs it with
`--dev` to install the dev image's rebuilt and dev-only components, and the
same invocation with `--dest <layer>/` assembles a development sysext layer, so
none of them can place files differently.

A `from = "@manifest/path"` entry copies a checked-in input relative to the
manifest directory. It travels with its owning component on a development
swap as well as in the image, independently of the Cargo output directory.

Only the Python 3.11+ standard library is used, so it runs in the bare Fedora
bootc base as well as on a developer host.
"""

from __future__ import annotations

import argparse
import hashlib
import platform
import subprocess
import json
import os
import re
import shutil
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath

DEFAULT_MANIFEST = Path(__file__).resolve().parent.parent / "components.toml"

SCOPES = ("session", "vm")
# `boot` components only take effect on a cold boot (kernel, UKI, bootloader,
# initramfs, installer media); a dev swap is redirected to their `scenario`,
# unless the component declares `dev_swap`.
KINDS = ("userspace", "boot")
# How `tilo-vm swap` handles a boot component that it can swap: "reboot" builds
# it with `dev_build`, installs it VM-wide as the next boot entry and reboots.
DEV_SWAPS = ("reboot",)
DEV_BUILD_RE = re.compile(r"^os/[A-Za-z0-9._/-]+$")
PROFILES = ("release", "thin-app")
# The Containerfile copies `direct` files itself, straight from the builder
# stage, because the build writes them at their final path.
DIRECT_LAYER = "direct"
# Layers named `dev-*` belong to the dev image (os/Containerfile.dev) only. The
# production image never carries them, and `--all` never selects them.
DEV_LAYER_PREFIX = "dev-"
NAME_RE = re.compile(r"^[a-z0-9][a-z0-9+-]*$")
LAYER_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
SCENARIO_RE = re.compile(r"^os/dev/[A-Za-z0-9._-]+$")
UNIT_RE = re.compile(r"^[A-Za-z0-9@_.:-]+\.(service|socket|timer|path|target)$")
MODE_RE = re.compile(r"^0[0-7]{3}$")

EXAMPLES = """\
examples:
  # What would the shell component install? (no sources needed)
  install-components.py --dry-run shell

  # Everything the image ships, as JSON, for tooling
  install-components.py --dry-run --all --format json

  # What os/Containerfile.dev rebuilds: its cargo arguments, then its files
  # (components with `dev_features`, built with them, and dev-only components)
  install-components.py --dev --dry-run --format cargo
  install-components.py --dev --from /out --dest /image

  # Assemble a development sysext layer for the shell and launcher from a
  # local release build
  install-components.py --from target/release --dest /tmp/layer shell launcher

  # What the production Containerfile runs: every image file, one tree per
  # image layer under /image/<layer>/
  install-components.py --from /out --dest /image --by-layer --all

  # List components, and what a swap needs: the systemd units to restart, a
  # reboot onto a new boot entry (the kernel), or for the other boot
  # components the os/dev/ scenario that cold-boots them
  install-components.py --list
"""


class ManifestError(ValueError):
    """The manifest is malformed. The message names the offending entry."""


@dataclass(frozen=True)
class FileEntry:
    component: str
    to: str
    layer: str
    source: str | None = None  # `from`; build output, @manifest/ input, or absolute path
    manifest_dir: Path | None = None  # root for explicitly manifest-relative inputs
    link: str | None = None
    mode: str | None = None
    tree: bool = False
    optional: bool = False

    @property
    def describe_source(self) -> str:
        return f"@link:{self.link}" if self.link is not None else str(self.source)

    @property
    def dev_only(self) -> bool:
        return is_dev_layer(self.layer)


def is_dev_layer(layer: str) -> bool:
    return layer.startswith(DEV_LAYER_PREFIX)


@dataclass(frozen=True)
class Component:
    name: str
    kind: str
    scope: str
    scenario: str | None
    profile: str
    packages: tuple[str, ...]
    bins: tuple[str, ...]
    features: tuple[str, ...]
    units: tuple[str, ...]
    user_units: tuple[str, ...]
    app_manifests: str | None
    optional: bool
    files: tuple[FileEntry, ...] = field(default_factory=tuple)
    # Cargo features the dev image (and dev layers) enable on top of `features`.
    dev_features: tuple[str, ...] = ()
    # Boot components tilo-vm can swap: how, and the script that builds them.
    dev_swap: str | None = None
    dev_build: str | None = None

    @property
    def dev_only(self) -> bool:
        """Every file sits in a `dev-*` layer: the production image omits it."""
        return bool(self.files) and all(entry.dev_only for entry in self.files)

    @property
    def in_dev_image(self) -> bool:
        """os/Containerfile.dev rebuilds and installs this component."""
        return self.dev_only or bool(self.dev_features)


@dataclass(frozen=True)
class Manifest:
    path: Path
    components: tuple[Component, ...]
    external_units: tuple[str, ...]

    def component(self, name: str) -> Component:
        for component in self.components:
            if component.name == name:
                return component
        known = ", ".join(c.name for c in self.components)
        raise ManifestError(f"unknown component {name!r}; known components: {known}")

    def files(self) -> list[FileEntry]:
        return [entry for component in self.components for entry in component.files]

    def production_components(self) -> tuple[Component, ...]:
        return tuple(c for c in self.components if not c.dev_only)

    def dev_image_components(self) -> tuple[Component, ...]:
        return tuple(c for c in self.components if c.in_dev_image)

    def layers(self) -> list[str]:
        seen: list[str] = []
        for entry in self.files():
            if entry.layer not in seen:
                seen.append(entry.layer)
        return seen


def _strings(value: object, where: str, key: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
        raise ManifestError(f"{where}: `{key}` must be a list of non-empty strings")
    return tuple(value)


def _check_image_path(path: str, where: str) -> None:
    pure = PurePosixPath(path)
    if not pure.is_absolute() or ".." in pure.parts or str(pure) != path.rstrip("/") or "//" in path:
        raise ManifestError(f"{where}: `to` must be a normalized absolute image path, got {path!r}")


def _parse_file(raw: object, component: str, default_layer: str | None, optional: bool, index: int, manifest_dir: Path) -> FileEntry:
    where = f"component {component!r} file #{index + 1}"
    if not isinstance(raw, dict):
        raise ManifestError(f"{where}: must be a table like {{ from = ..., to = ... }}")
    unknown = set(raw) - {"from", "to", "link", "mode", "layer", "tree", "optional"}
    if unknown:
        raise ManifestError(f"{where}: unknown keys {sorted(unknown)}")
    to = raw.get("to")
    if not isinstance(to, str):
        raise ManifestError(f"{where}: `to` (absolute image path) is required")
    _check_image_path(to, where)
    source, link = raw.get("from"), raw.get("link")
    if (source is None) == (link is None):
        raise ManifestError(f"{where}: exactly one of `from` or `link` is required")
    if source is not None and (not isinstance(source, str) or not source or ".." in PurePosixPath(source).parts):
        raise ManifestError(f"{where}: `from` must be a build output path without `..`")
    if source is not None and source.startswith("@manifest/"):
        relative = source.removeprefix("@manifest/")
        if not relative or relative.startswith("/") or str(PurePosixPath(relative)) != relative.rstrip("/"):
            raise ManifestError(f"{where}: @manifest/ must name a normalized relative input")
    if source is not None and source.startswith("@asset/") and source != "@asset/cosign-v2.5.2":
        raise ManifestError(f"{where}: unknown reviewed native asset")
    if link is not None and (not isinstance(link, str) or not link or link.startswith("/")):
        raise ManifestError(f"{where}: `link` must be a relative symlink target")
    tree = raw.get("tree", False)
    if not isinstance(tree, bool):
        raise ManifestError(f"{where}: `tree` must be true or false")
    if tree and (link is not None or not to.endswith("/") or not str(source).endswith("/")):
        raise ManifestError(f"{where}: a `tree` copies a directory; `from` and `to` must both end in '/'")
    if not tree and to.endswith("/"):
        raise ManifestError(f"{where}: `to` ends in '/' but the entry is not a `tree`")
    mode = raw.get("mode")
    if mode is not None:
        if not isinstance(mode, str) or not MODE_RE.match(mode):
            raise ManifestError(f"{where}: `mode` must be an octal string such as \"0755\"")
        if tree or link is not None:
            raise ManifestError(f"{where}: `mode` applies to regular files only")
    elif source is not None and not tree:
        raise ManifestError(f"{where}: regular files need an explicit `mode`, such as \"0755\"")
    layer = raw.get("layer", default_layer)
    if not isinstance(layer, str) or not LAYER_RE.match(layer):
        raise ManifestError(f"{where}: `layer` (image layer name) is required, on the file or the component")
    file_optional = raw.get("optional", optional)
    if not isinstance(file_optional, bool):
        raise ManifestError(f"{where}: `optional` must be true or false")
    return FileEntry(component=component, to=to, layer=layer, source=source,
                     manifest_dir=manifest_dir, link=link,
                     mode=mode, tree=tree, optional=file_optional)


def parse_manifest(document: dict, path: Path) -> Manifest:
    unknown = set(document) - {"component", "external_units"}
    if unknown:
        raise ManifestError(f"unknown top-level keys {sorted(unknown)}")
    external_units = _strings(document.get("external_units"), "top level", "external_units")
    raw_components = document.get("component")
    if not isinstance(raw_components, list) or not raw_components:
        raise ManifestError("the manifest declares no [[component]] tables")
    components: list[Component] = []
    names: set[str] = set()
    destinations: dict[str, str] = {}
    allowed = {"name", "kind", "scope", "scenario", "profile", "packages", "bins", "features",
               "dev_features", "units", "user_units", "app_manifests", "optional", "layer", "files",
               "dev_swap", "dev_build", "outside_sessions"}
    for raw in raw_components:
        name = raw.get("name")
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise ManifestError(f"component name {name!r} must match {NAME_RE.pattern}")
        where = f"component {name!r}"
        if name in names:
            raise ManifestError(f"{where}: declared twice")
        names.add(name)
        extra = set(raw) - allowed
        if extra:
            raise ManifestError(f"{where}: unknown keys {sorted(extra)}")
        kind = raw.get("kind")
        if kind not in KINDS:
            raise ManifestError(f"{where}: `kind` must be one of {', '.join(KINDS)}")
        scope = raw.get("scope")
        if scope not in SCOPES:
            raise ManifestError(f"{where}: `scope` must be one of {', '.join(SCOPES)}")
        scenario = raw.get("scenario")
        if kind == "boot":
            if scope != "vm":
                raise ManifestError(f"{where}: a boot component is VM-wide; set scope = \"vm\"")
            if not isinstance(scenario, str) or not SCENARIO_RE.match(scenario):
                raise ManifestError(f"{where}: a boot component needs `scenario`, the os/dev/ driver "
                                    "script that exercises it on a cold boot")
        elif scenario is not None:
            raise ManifestError(f"{where}: `scenario` is only for kind = \"boot\" components")
        dev_swap, dev_build = raw.get("dev_swap"), raw.get("dev_build")
        if dev_swap is not None:
            if kind != "boot":
                raise ManifestError(f"{where}: `dev_swap` is only for kind = \"boot\" components; "
                                    "userspace components are always swappable")
            if dev_swap not in DEV_SWAPS:
                raise ManifestError(f"{where}: `dev_swap` must be one of {', '.join(DEV_SWAPS)}")
            if not isinstance(dev_build, str) or not DEV_BUILD_RE.match(dev_build) or ".." in dev_build:
                raise ManifestError(f"{where}: a `dev_swap` component needs `dev_build`, the os/ script "
                                    "that builds it for a dev VM")
        elif dev_build is not None:
            raise ManifestError(f"{where}: `dev_build` goes with `dev_swap`")
        profile = raw.get("profile", "release")
        if profile not in PROFILES:
            raise ManifestError(f"{where}: `profile` must be one of {', '.join(PROFILES)}")
        packages = _strings(raw.get("packages"), where, "packages")
        if not packages and kind != "boot":
            raise ManifestError(f"{where}: `packages` must name at least one cargo package")
        units = _strings(raw.get("units"), where, "units")
        user_units = _strings(raw.get("user_units"), where, "user_units")
        for unit in (*units, *user_units):
            if not UNIT_RE.match(unit):
                raise ManifestError(f"{where}: {unit!r} is not a systemd unit name")
        app_manifests = raw.get("app_manifests")
        if app_manifests is not None and not isinstance(app_manifests, str):
            raise ManifestError(f"{where}: `app_manifests` must be a glob string")
        optional = raw.get("optional", False)
        if not isinstance(optional, bool):
            raise ManifestError(f"{where}: `optional` must be true or false")
        if "outside_sessions" in raw and (scope != "vm" or not isinstance(raw["outside_sessions"], bool)):
            raise ManifestError(f"{where}: `outside_sessions` is true or false, for scope = \"vm\" only")
        layer = raw.get("layer")
        raw_files = raw.get("files", [])
        if not isinstance(raw_files, list) or (not raw_files and kind != "boot"):
            # Boot components whose install is not manifest-driven yet may omit files.
            raise ManifestError(f"{where}: `files` must list at least one {{ from, to, mode }} entry")
        files = tuple(_parse_file(f, name, layer, optional, i, path.resolve().parent) for i, f in enumerate(raw_files))
        dev_layers = {entry.layer for entry in files if entry.dev_only}
        if dev_layers and len(dev_layers) != len({entry.layer for entry in files}):
            raise ManifestError(f"{where}: mixes dev-only layers ({', '.join(sorted(dev_layers))}) with "
                                "production layers; a component is either in the production image or "
                                "dev-only, so put every file in a `dev-*` layer or none")
        dev_features = _strings(raw.get("dev_features"), where, "dev_features")
        if (dev_features or dev_layers) and (kind != "userspace" or profile != "release"):
            raise ManifestError(f"{where}: `dev_features` and `dev-*` layers are for kind = \"userspace\" "
                                "components with the release profile, which the dev image rebuilds")
        for entry in files:
            if entry.to in destinations:
                raise ManifestError(f"{where}: {entry.to} is already installed by component {destinations[entry.to]!r}")
            destinations[entry.to] = name
        components.append(Component(
            name=name, kind=kind, scope=scope, scenario=scenario, profile=profile, packages=packages,
            bins=_strings(raw.get("bins"), where, "bins"),
            features=_strings(raw.get("features"), where, "features"),
            units=units, user_units=user_units, app_manifests=app_manifests,
            optional=optional, files=files, dev_features=dev_features,
            dev_swap=dev_swap, dev_build=dev_build,
        ))
    return Manifest(path=path, components=tuple(components), external_units=external_units)


def load_manifest(path: Path = DEFAULT_MANIFEST) -> Manifest:
    try:
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except tomllib.TOMLDecodeError as error:
        raise ManifestError(f"{path}: {error}") from error
    return parse_manifest(document, path)


def select_components(manifest: Manifest, names: list[str], everything: bool,
                      dev: bool = False) -> tuple[Component, ...]:
    if bool(names) + everything + dev != 1:
        raise ManifestError("name components to install, or pass --all or --dev (exactly one of these)")
    if everything:
        return manifest.production_components()
    if dev:
        return manifest.dev_image_components()
    return tuple(manifest.component(n) for n in names)


def select(manifest: Manifest, names: list[str], everything: bool, layers: list[str],
           by_layer: bool, dev: bool = False) -> list[FileEntry]:
    components = select_components(manifest, names, everything, dev)
    known_layers = manifest.layers()
    for layer in layers:
        if layer not in known_layers:
            raise ManifestError(f"unknown layer {layer!r}; manifest layers: {', '.join(known_layers)}")
    chosen = [entry for component in components for entry in component.files]
    if layers:
        chosen = [entry for entry in chosen if entry.layer in layers]
    elif by_layer:
        chosen = [entry for entry in chosen if entry.layer != DIRECT_LAYER]
    return chosen


def native_cosign(manifest_dir: Path, copied: Path | None = None) -> Path:
    """Resolve the reviewed native verifier; cache bytes never establish trust."""
    machine = platform.machine()
    targets = {"x86_64": ("amd64", 62), "aarch64": ("arm64", 183)}
    if machine not in targets:
        raise ManifestError("unsupported native Cosign architecture")
    suffix, elf_machine = targets[machine]
    name = f"cosign-v2.5.2-linux-{suffix}"
    pins = manifest_dir / "external-assets/github-assets.tsv"
    matches = [line.split("\t")[0] for line in pins.read_text().splitlines()
               if not line.startswith("#") and len(line.split("\t")) == 3
               and line.split("\t")[1] == name]
    if len(matches) != 1 or not re.fullmatch(r"[0-9a-f]{64}", matches[0]):
        raise ManifestError("missing or ambiguous reviewed Cosign pin")
    cache = Path(os.environ.get("TILO_GITHUB_ASSET_CACHE", manifest_dir / "external-assets/cache"))
    candidate = copied if copied is not None else cache / name
    if not candidate.exists():
        raise ManifestError("reviewed Cosign input is absent from the image build context")
    if candidate.is_symlink() or not candidate.is_file():
        raise ManifestError("reviewed Cosign input must be a regular cache file")
    digest = hashlib.sha256()
    with candidate.open("rb") as handle:
        header = handle.read(20)
        digest.update(header)
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    if digest.hexdigest() != matches[0]:
        raise ManifestError("reviewed Cosign checksum mismatch")
    if (header[:6] != b"\x7fELF\x02\x01" or len(header) != 20
            or int.from_bytes(header[18:20], "little") != elf_machine):
        raise ManifestError("reviewed Cosign is not the native ELF executable")
    return candidate


def source_shell_fonts(entry: FileEntry, build_root: Path | None) -> bool:
    return (build_root is None and entry.component == "shell"
            and entry.source == "/usr/share/fonts/tilo/")


def font_catalog_command(*args: str) -> str:
    """Use the source's canonical verifier, including its offline artifact cache."""
    tool = Path(__file__).resolve().parents[2] / "tools/font-catalog.py"
    try:
        result = subprocess.run([sys.executable, str(tool), *args],
                                capture_output=True, text=True, timeout=600, check=True)
    except subprocess.CalledProcessError as error:
        raise ManifestError(f"mandatory shell font catalog: {error.stderr.strip()}") from error
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ManifestError(f"mandatory shell font catalog: {error}") from error
    return result.stdout.strip()


def resolve_source(entry: FileEntry, from_dirs: list[Path], build_root: Path | None) -> Path | None:
    assert entry.source is not None
    relative = entry.source.rstrip("/")
    if source_shell_fonts(entry, build_root):
        fonts = Path(__file__).resolve().parents[2] / "crates/sdk/ui-core/assets/fonts"
        return Path(font_catalog_command("root", str(fonts), "--scope", "os"))
    if relative == "@asset/cosign-v2.5.2":
        assert entry.manifest_dir is not None
        return native_cosign(entry.manifest_dir)
    if relative.startswith("@manifest/"):
        assert entry.manifest_dir is not None
        candidate = entry.manifest_dir / relative.removeprefix("@manifest/")
        return candidate if candidate.exists() or candidate.is_symlink() else None
    if entry.source.startswith("/"):
        if build_root is None:
            raise ManifestError(
                f"component {entry.component!r} installs {entry.source}, which the build writes at "
                "its final path; pass --build-root to say where that build tree is mounted")
        candidate = build_root / relative.lstrip("/")
        return candidate if candidate.exists() or candidate.is_symlink() else None
    if not from_dirs:
        raise ManifestError("pass --from DIR (the build output directory, e.g. target/release)")
    for directory in from_dirs:
        candidate = directory / relative
        if candidate.exists() or candidate.is_symlink():
            return candidate
    return None


def target_path(dest: Path, entry: FileEntry, by_layer: bool) -> Path:
    root = dest / entry.layer if by_layer else dest
    return root / entry.to.strip("/")


def _epoch() -> int | None:
    value = os.environ.get("SOURCE_DATE_EPOCH")
    return int(value) if value and value.isdigit() else None


def _make_parents(root: Path, path: Path, created: list[Path]) -> None:
    missing = []
    parent = path.parent
    while parent != root and not parent.exists():
        missing.append(parent)
        parent = parent.parent
    for directory in reversed(missing):
        directory.mkdir(mode=0o755)
        directory.chmod(0o755)
        created.append(directory)


def install(entries: list[FileEntry], from_dirs: list[Path], build_root: Path | None,
            dest: Path, by_layer: bool, log) -> int:
    dest.mkdir(parents=True, exist_ok=True)
    epoch = _epoch()
    created_dirs: list[Path] = []
    installed = 0
    for entry in entries:
        target = target_path(dest, entry, by_layer)
        root = dest / entry.layer if by_layer else dest
        root.mkdir(parents=True, exist_ok=True)
        _make_parents(root, target, created_dirs)
        if entry.link is not None:
            if target.is_symlink() or target.exists():
                target.unlink()
            target.symlink_to(entry.link)
            if epoch is not None:
                os.utime(target, (epoch, epoch), follow_symlinks=False)
            log(f"link  {entry.to} -> {entry.link}  [{entry.component}]")
            installed += 1
            continue
        source = resolve_source(entry, from_dirs, build_root)
        if source is None:
            if entry.optional:
                log(f"skip  {entry.to}: optional build output {entry.source!r} is absent  [{entry.component}]")
                continue
            searched = entry.source if entry.source.startswith("/") else ", ".join(str(d) for d in from_dirs)
            raise ManifestError(
                f"component {entry.component!r}: build output {entry.source!r} not found (searched {searched}). "
                "Build it first, or fix `from` in os/components.toml")
        if entry.tree:
            if not source.is_dir():
                raise ManifestError(f"component {entry.component!r}: {source} is not a directory")
            if source_shell_fonts(entry, build_root):
                # Stage only admitted faces and notices, and verify copied bytes.
                # A shell layer must never depend on the guest's older catalog.
                font_catalog_command("stage", str(source), str(target), "--scope", "os")
            else:
                shutil.copytree(source, target, symlinks=True, dirs_exist_ok=True)
        else:
            if source.is_dir():
                raise ManifestError(f"component {entry.component!r}: {source} is a directory; mark the entry `tree = true`")
            temporary = target.with_name(f".{target.name}.tmp")
            shutil.copy2(source, temporary, follow_symlinks=True)
            if entry.source == "@asset/cosign-v2.5.2":
                assert entry.manifest_dir is not None
                native_cosign(entry.manifest_dir, temporary)
            temporary.chmod(int(str(entry.mode), 8))
            os.replace(temporary, target)
        log(f"file  {entry.to}  <- {source}  [{entry.component}]")
        installed += 1
    if epoch is not None:
        for directory in created_dirs:
            os.utime(directory, (epoch, epoch))
    return installed


def plan_rows(entries: list[FileEntry]) -> list[dict]:
    return [{
        "component": e.component, "layer": e.layer, "from": e.source, "link": e.link,
        "to": e.to, "mode": e.mode, "tree": e.tree, "optional": e.optional,
    } for e in entries]


def cargo_plan(components: tuple[Component, ...], dev: bool) -> tuple[list[str], list[str], list[str]]:
    """(packages, bins, features) that build these components; `dev` adds `dev_features`."""
    packages: list[str] = []
    bins: list[str] = []
    features: list[str] = []
    for component in components:
        if component.profile != "release":
            raise ManifestError(f"component {component.name!r} uses profile {component.profile!r}; only "
                                "release components build from a plain cargo argument list")
        for values, into in ((component.packages, packages), (component.bins, bins),
                             (component.features, features),
                             (component.dev_features if dev else (), features)):
            into.extend(v for v in values if v not in into)
    return packages, bins, features


def print_cargo(components: tuple[Component, ...], dev: bool, fmt: str) -> None:
    packages, bins, features = cargo_plan(components, dev)
    if fmt == "bins":
        for name in bins:
            print(name)
        return
    words = [w for p in packages for w in ("-p", p)] + [w for b in bins for w in ("--bin", b)]
    if features:
        words += ["--features", ",".join(features)]
    print(" ".join(words))


def print_plan(entries: list[FileEntry], fmt: str) -> None:
    if fmt == "json":
        json.dump(plan_rows(entries), sys.stdout, indent=2)
        sys.stdout.write("\n")
    elif fmt == "contract":
        # Same shape as os/tests/lib/containerfile.sh's copy contract:
        #   source-stage<TAB>source<TAB>destination
        for e in entries:
            print(f"components.toml\t{e.component}:{e.describe_source}\t{e.to}")
    else:
        for e in entries:
            mode = e.mode or ("tree" if e.tree else "link")
            flag = " (optional)" if e.optional else ""
            print(f"{e.component:<16} {e.layer:<17} {mode:<5} {e.to}  <- {e.describe_source}{flag}")


def print_components(manifest: Manifest, fmt: str) -> None:
    rows = [{
        "name": c.name, "kind": c.kind, "scope": c.scope, "scenario": c.scenario,
        "profile": c.profile, "packages": list(c.packages),
        "bins": list(c.bins), "features": list(c.features), "units": list(c.units),
        "user_units": list(c.user_units), "optional": c.optional,
        "dev_features": list(c.dev_features), "dev_only": c.dev_only,
        "dev_swap": c.dev_swap, "dev_build": c.dev_build,
    } for c in manifest.components]
    if fmt == "json":
        json.dump(rows, sys.stdout, indent=2)
        sys.stdout.write("\n")
        return
    for row in rows:
        if row["kind"] == "boot" and row["dev_swap"] == "reboot":
            action = f"swap: build {row['dev_build']}, next boot entry, reboot"
        elif row["kind"] == "boot":
            action = f"cold boot: {row['scenario']}"
        else:
            units = [*row["units"], *(f"--user:{u}" for u in row["user_units"])]
            action = "restart: " + (" ".join(units) or "-")
        packages = " ".join(row["packages"]) or "-"
        flavour = "  (dev image only)" if row["dev_only"] else ""
        print(f"{row['name']:<16} {row['kind']:<9} {row['scope']:<8} {packages:<36} {action}{flavour}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Install Tilo build outputs into a root tree as declared by os/components.toml.",
        epilog=EXAMPLES, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("components", nargs="*", metavar="COMPONENT", help="components to install")
    parser.add_argument("--all", action="store_true",
                        help="every production image component (never the dev-only ones)")
    parser.add_argument("--dev", action="store_true",
                        help="what the dev image rebuilds: components with `dev_features` (built with "
                        "them) and dev-only components (files in `dev-*` layers)")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST,
                        help="component manifest (default: %(default)s)")
    parser.add_argument("--from", dest="from_dirs", type=Path, action="append", default=[],
                        metavar="DIR", help="directory holding build outputs named by relative `from` "
                        "paths; repeat to search several, first match wins")
    parser.add_argument("--build-root", type=Path, metavar="DIR",
                        help="root of the build tree for absolute `from` paths (outputs the build "
                        "writes at their final path, like the baked runtime bundle)")
    parser.add_argument("--dest", type=Path, metavar="DIR", help="destination root to install into")
    parser.add_argument("--layer", dest="layers", action="append", default=[], metavar="NAME",
                        help="only files of this image layer (repeatable)")
    parser.add_argument("--by-layer", action="store_true",
                        help=f"install each file under DEST/<layer>/ and skip `{DIRECT_LAYER}` files, "
                        "which the Containerfile copies itself")
    parser.add_argument("--dry-run", action="store_true", help="print the install plan; touch nothing")
    parser.add_argument("--list", action="store_true", help="list components, scopes and units")
    parser.add_argument("--format", choices=("text", "json", "contract", "cargo", "bins"), default="text",
                        help="output format for --dry-run and --list; `cargo` prints the cargo build "
                        "arguments of the selection, `bins` its binaries one per line")
    parser.add_argument("--quiet", action="store_true", help="print only errors")
    args = parser.parse_args(argv)

    try:
        manifest = load_manifest(args.manifest)
        if args.list:
            print_components(manifest, args.format)
            return 0
        if args.format in ("cargo", "bins"):
            if not args.dry_run:
                parser.error(f"--format {args.format} needs --dry-run")
            print_cargo(select_components(manifest, args.components, args.all, args.dev),
                        args.dev, args.format)
            return 0
        entries = select(manifest, args.components, args.all, args.layers, args.by_layer, args.dev)
        if args.dry_run:
            print_plan(entries, args.format)
            return 0
        if args.dest is None:
            parser.error("--dest is required unless --dry-run or --list is given")
        log = (lambda _message: None) if args.quiet else (lambda message: print(message))
        count = install(entries, args.from_dirs, args.build_root, args.dest, args.by_layer, log)
        if not args.quiet:
            print(f"installed {count} entries into {args.dest}")
        return 0
    except ManifestError as error:
        print(f"install-components: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
