#!/usr/bin/env python3
"""Keep os/components.toml the single source of truth for Tilo image binaries.

The production image and development sysext layers are both assembled from
os/components.toml. This check fails when the two could drift:

* os/Containerfile copies a build output into the image other than through
  the manifest (`COPY --from=component-files /image/<layer>/ /`), or through a
  `direct` entry the manifest declares;
* os/image/compile.sh, which compiles the image's binaries outside any image
  build, compiles or stages a binary the manifest does not declare, or the
  manifest declares one it never compiles;
* the manifest names a cargo package, bin or feature missing from the
  workspace, or a systemd unit the image does not ship.

Static and fast: TOML and text parsing only, nothing is compiled.
"""

from __future__ import annotations

import argparse
import glob
import importlib.util
import re
import shlex
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RERUN = "python3 tools/policy/check_component_manifest.py   (or tools/policy/run.sh)"
WHY = ("the image and dev layers are both assembled from os/components.toml, so a Tilo "
       "build output that bypasses it ships in one and not the other, and they drift")
PAYLOAD_STAGE = "native-builder-payload"
# The stage that takes the compiled tree (the `compiled` build context,
# os/image/compile.sh's output) and packs the runtimes and the app store from it.
BUILDER_STAGE = "apps-payload"
COMPILED_CONTEXT = "compiled"
# What compiles the image's binaries: its release part, after the `--dev` branch.
COMPILE_SCRIPT = "os/image/compile.sh"
INSTALL_STAGE = "component-files"
# The baked runtime with the font catalog's other two names hard-linked to its
# fonts, assembled under /image so the rootfs copies it in one COPY and the
# fonts' bytes land in one layer. It installs `direct` trees written
# at their final paths, each named in the stage's RUN as /image<path>.
ASSEMBLED_STAGE = "runtime-image-payload"
ASSEMBLED_COPY = ("/image/usr/", "/usr/")
# The app store as the rootfs takes it: the payload stage's store, copied whole
# and with the env-runtime's host-identical files left out. Its one
# COPY installs the `direct` store tree the payload stage carries.
SEEDED_STAGE = "app-store"
SEEDED_TREE = "/usr/share/tilo-apps/"
SEEDED_COPY = ("/out" + SEEDED_TREE, SEEDED_TREE)


def load_installer(repo: Path):
    path = repo / "os/scripts/install-components.py"
    spec = importlib.util.spec_from_file_location("install_components", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


@dataclass(frozen=True)
class Instruction:
    line: int
    stage: str
    words: tuple[str, ...]
    text: str


def instructions(path: Path) -> list[Instruction]:
    """Logical Containerfile instructions with their first line number."""
    result: list[Instruction] = []
    stage, logical, start = "", "", 0
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not logical and (not line or line.startswith("#")):
            continue
        if logical and line.startswith("#"):
            continue
        continued = line.endswith("\\")
        line = line[:-1].rstrip() if continued else line
        if not logical:
            start = number
        logical = f"{logical} {line}".strip()
        if continued:
            continue
        try:
            words = tuple(shlex.split(logical, posix=True))
        except ValueError:
            words = tuple(logical.split())
        if words and words[0].upper() == "FROM":
            stage = words[-1] if len(words) >= 4 and words[-2].upper() == "AS" else words[-1]
        result.append(Instruction(start, stage, words, logical))
        logical = ""
    return result


def compile_steps(path: Path) -> list[Instruction]:
    """The logical lines of the release compile in `path` (os/image/compile.sh), with their
    first line number: what the image build's builder stage ran before compiling left it."""
    if not path.is_file():
        return []
    result: list[Instruction] = []
    logical, start, release = "", 0, False
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not release:
            # The `--dev` branch ends with the script's first top-level `fi`.
            release = raw == "fi"
            continue
        line = raw.strip()
        if not logical and (not line or line.startswith("#")):
            continue
        if logical and line.startswith("#"):
            continue
        if not logical:
            start = number
        continued = line.endswith("\\")
        logical = f"{logical} {line[:-1].rstrip() if continued else line}".strip()
        if not continued:
            result.append(Instruction(start, "compile", tuple(logical.split()), logical))
            logical = ""
    return result


def copy_pairs(instruction: Instruction) -> tuple[str | None, list[tuple[str, str]]]:
    """(source stage, [(source, effective destination)]) of a COPY."""
    words = list(instruction.words[1:])
    source_stage = None
    while words and words[0].startswith("--"):
        if words[0].startswith("--from="):
            source_stage = words[0].removeprefix("--from=")
        words.pop(0)
    if len(words) < 2:
        return source_stage, []
    *sources, destination = words
    pairs = []
    for source in sources:
        effective = destination
        if len(sources) > 1:
            effective = destination + source.rstrip("/").rsplit("/", 1)[-1]
        pairs.append((source, effective))
    return source_stage, pairs


@dataclass
class Package:
    name: str
    directory: Path
    bins: set[str]
    features: set[str]


def workspace_packages(repo: Path) -> dict[str, Package]:
    document = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))
    packages: dict[str, Package] = {}
    for pattern in document.get("workspace", {}).get("members", []):
        for member in sorted(glob.glob(str(repo / pattern))):
            manifest = Path(member) / "Cargo.toml"
            if not manifest.is_file():
                continue
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
            package = data.get("package", {})
            name = package.get("name")
            if not isinstance(name, str):
                continue
            directory = manifest.parent
            bins = {b["name"] for b in data.get("bin", []) if isinstance(b, dict) and "name" in b}
            if package.get("autobins", True):
                if (directory / "src/main.rs").is_file():
                    bins.add(name)
                bin_dir = directory / "src/bin"
                if bin_dir.is_dir():
                    bins.update(p.stem for p in bin_dir.glob("*.rs"))
                    bins.update(p.name for p in bin_dir.iterdir() if (p / "main.rs").is_file())
            features = set(data.get("features", {}))
            # An optional dependency is an implicit feature of the same name.
            features.update(dep for dep, spec in data.get("dependencies", {}).items()
                            if isinstance(spec, dict) and spec.get("optional"))
            packages[name] = Package(name, directory, bins, features)
    return packages


class Report:
    def __init__(self, repo: Path):
        self.repo = repo
        self.errors: list[str] = []
        manifest = repo / "os/components.toml"
        self.manifest_lines = manifest.read_text(encoding="utf-8").splitlines() if manifest.is_file() else []

    def manifest_line(self, component: str | None) -> str:
        if component is not None:
            wanted = f'name = "{component}"'
            for number, line in enumerate(self.manifest_lines, 1):
                if line.strip() == wanted:
                    return f"os/components.toml:{number}"
        return "os/components.toml"

    def error(self, where: str, what: str, fix: str) -> None:
        self.errors.append(f"{where}: {what}\n  why: {WHY}.\n  fix: {fix}\n  re-run: {RERUN}")


def snippet(name: str, output: str, destination: str, *, direct: bool = False) -> str:
    layer = "direct" if direct else "desktop-binaries"
    mode = "0644" if destination.startswith("/usr/share/") else "0755"
    entry = (f'{{ from = "{output}", to = "{destination}", tree = true }}' if destination.endswith("/")
             else f'{{ from = "{output}", to = "{destination}", mode = "{mode}" }}')
    return ("add the file to the component that builds it in os/components.toml, or a new one:\n"
            "\n"
            "        [[component]]\n"
            f'        name = "{name}"\n'
            '        scope = "session"            # or "vm" for system services\n'
            f'        packages = ["<cargo package>"]\n'
            f'        bins = ["{Path(output).name}"]\n'
            f'        layer = "{layer}"\n'
            "        files = [\n"
            f"          {entry},\n"
            "        ]\n")


def check_manifest_against_workspace(report: Report, manifest, packages: dict[str, Package]) -> None:
    for component in manifest.components:
        where = report.manifest_line(component.name)
        if component.name == "shell":
            companion = next((entry for entry in component.files
                              if entry.to == "/usr/libexec/tilo-session"), None)
            if (companion is None or companion.source != "@manifest/usr/libexec/tilo-session"
                    or companion.optional or str(companion.mode) != "0755"):
                report.error(where, "shell must ship its matching mandatory PAM session wrapper",
                             'add { from = "@manifest/usr/libexec/tilo-session", '
                             'to = "/usr/libexec/tilo-session", mode = "0755" } to shell files; '
                             'shell-only swaps can otherwise recover locked under an older wrapper.')
        if component.scenario and not (report.repo / component.scenario).is_file():
            report.error(where, f"boot component {component.name!r} names scenario {component.scenario!r}, "
                         "which does not exist",
                         "point `scenario` at the os/dev/ driver script that cold-boots this component.")
        if component.dev_build and not (report.repo / component.dev_build).is_file():
            report.error(where, f"boot component {component.name!r} names dev_build {component.dev_build!r}, "
                         "which does not exist",
                         "point `dev_build` at the os/ script that builds this component for a dev VM "
                         "(tilo-vm swap runs it).")
        component_packages = list(component.packages)
        for package in component.packages:
            if package not in packages:
                report.error(where, f"component {component.name!r} names cargo package {package!r}, "
                             "which is not a workspace member",
                             "correct `packages` to the `[package] name` in the crate's Cargo.toml, "
                             "or add the crate to the workspace.")
        app_packages: set[str] = set()
        if component.app_manifests:
            matches = sorted(report.repo.glob(component.app_manifests))
            if not matches:
                report.error(where, f"`app_manifests = {component.app_manifests!r}` matches no file",
                             "point it at the apps' tilo-app.toml files, e.g. \"apps/*/tilo-app.toml\".")
            by_directory = {p.directory.resolve(): p.name for p in packages.values()}
            for match in matches:
                name = by_directory.get(match.parent.resolve())
                if name is None:
                    report.error(where, f"{match.relative_to(report.repo)} sits in no workspace package",
                                 "add that app crate to the workspace members.")
                else:
                    app_packages.add(name)
            component_packages += sorted(app_packages)
        available = set().union(*(packages[p].bins for p in component_packages if p in packages))
        for binary in component.bins:
            if binary not in available:
                report.error(where, f"component {component.name!r} ships cargo bin {binary!r}, which none "
                             f"of its packages ({', '.join(component.packages)}) builds",
                             "fix the name in `bins` (and the matching `from`), or add the package "
                             "that owns the bin to `packages`.")
        for key, features in (("features", component.features), ("dev_features", component.dev_features)):
            for feature in features:
                if key == "dev_features" and "/" not in feature and len(component_packages) == 1:
                    package = component_packages[0]
                    if package in packages and feature not in packages[package].features:
                        report.error(where, f"{key} entry {feature!r}: package {package!r} declares no feature",
                                     f"add it to [features] in {packages[package].directory.relative_to(report.repo)}"
                                     "/Cargo.toml or drop it from `dev_features`.")
                    continue
                package, _, name = feature.rpartition("/")
                if not package or package not in component_packages:
                    report.error(where, f"{key} entry {feature!r} must be qualified with one of the "
                                 f"component's packages ({', '.join(component_packages[:6])}...)",
                                 'write it as "<package>/<feature>" (or use an unqualified dev feature '
                                 'when the component has one package).')
                elif package in packages and name not in packages[package].features:
                    report.error(where, f"{key} entry {feature!r}: package {package!r} declares no feature "
                                 f"{name!r}",
                                 f"add it to [features] in {packages[package].directory.relative_to(report.repo)}"
                                 f"/Cargo.toml or drop it from `{key}`.")


def check_units(report: Report, manifest, containerfile_text: str) -> None:
    for component in manifest.components:
        where = report.manifest_line(component.name)
        for kind, units in (("system", component.units), ("user", component.user_units)):
            for unit in units:
                if unit in manifest.external_units:
                    continue
                source = f"os/usr/lib/systemd/{kind}/{unit}"
                if not (report.repo / source).is_file():
                    report.error(where, f"component {component.name!r} lists {kind} unit {unit!r}, but "
                                 f"{source} does not exist",
                                 f"fix the unit name, move it to `{'user_units' if kind == 'system' else 'units'}` "
                                 "if it is the other kind, or list distro-provided units in `external_units`.")
                elif source not in containerfile_text:
                    report.error(where, f"component {component.name!r} lists {unit!r}, which os/Containerfile "
                                 "never copies into the image",
                                 f"COPY {source} in the rootfs stage, or drop the unit from the component.")


def check_containerfile(report: Report, manifest, installer, containerfile: Path, thin_script: Path,
                        compile_script: Path) -> None:
    relative = containerfile.relative_to(report.repo)
    compiler = compile_script.relative_to(report.repo)
    parsed = instructions(containerfile)
    # Dev-only components (files in `dev-*` layers) are os/Containerfile.dev's;
    # tools/policy/check_dev_image_contract.py keeps them out of this image.
    production = manifest.production_components()
    files = [entry for component in production for entry in component.files]
    dev_layers = {e.layer for e in manifest.files() if e.dev_only}
    direct = {(e.source, e.to): e for e in files if e.layer == installer.DIRECT_LAYER and e.source}
    layers = {e.layer for e in files if e.layer != installer.DIRECT_LAYER}
    consumed: dict[str, list[int]] = {}
    matched_direct: set[tuple[str, str]] = set()

    for instruction in parsed:
        if not instruction.words or instruction.words[0].upper() != "COPY":
            continue
        source_stage, pairs = copy_pairs(instruction)
        where = f"{relative}:{instruction.line}"
        if source_stage == INSTALL_STAGE:
            for source, destination in pairs:
                match = re.fullmatch(r"/image/([a-z0-9-]+)/", source)
                if not match or destination != "/":
                    report.error(where, f"COPY --from={INSTALL_STAGE} {source} {destination} does not copy "
                                 "one whole manifest layer",
                                 f"copy a layer as a tree: COPY --from={INSTALL_STAGE} /image/<layer>/ /")
                    continue
                if match.group(1) in dev_layers:
                    # Reported with the dev-image contract's reasoning by
                    # check_dev_image_contract.py; not a manifest-drift error.
                    continue
                if match.group(1) not in layers:
                    report.error(where, f"layer {match.group(1)!r} is copied but no os/components.toml "
                                 "file uses it",
                                 f"use a layer the manifest declares ({', '.join(sorted(layers))}) or "
                                 "set `layer` on the components that belong in it.")
                consumed.setdefault(match.group(1), []).append(instruction.line)
            continue
        if source_stage == ASSEMBLED_STAGE and instruction.stage != ASSEMBLED_STAGE:
            assembled = " ".join(i.text for i in parsed if i.stage == ASSEMBLED_STAGE
                                 and i.words and i.words[0].upper() == "RUN")
            if pairs != [ASSEMBLED_COPY]:
                report.error(where, f"COPY --from={ASSEMBLED_STAGE} copies {pairs}",
                             f"copy the assembled tree whole: COPY --from={ASSEMBLED_STAGE} "
                             f"{ASSEMBLED_COPY[0]} {ASSEMBLED_COPY[1]}")
                continue
            for key in direct:
                source, destination = key
                if not destination.startswith(ASSEMBLED_COPY[1]) \
                        or "/image" + destination.rstrip("/") not in assembled:
                    continue
                # A tree the build wrote at its final path, or a builder output
                # (a runtime's publisher key) the stage mounts from the same
                # builder result as the runtime it signs.
                if source == destination or (not source.startswith("/")
                                             and f"from={BUILDER_STAGE},source=/out/{source}," in assembled):
                    matched_direct.add(key)
            continue
        if source_stage == SEEDED_STAGE and instruction.stage != SEEDED_STAGE:
            seeded = " ".join(i.text for i in parsed if i.stage == SEEDED_STAGE
                              and i.words and i.words[0].upper() == "RUN")
            mount = f"from={PAYLOAD_STAGE},source={SEEDED_TREE.rstrip('/')},"
            if pairs != [SEEDED_COPY] or mount not in seeded:
                report.error(where, f"COPY --from={SEEDED_STAGE} copies {pairs}",
                             f"copy the seeded store whole, from a {SEEDED_STAGE} stage that mounts "
                             f"{PAYLOAD_STAGE}'s {SEEDED_TREE}: COPY --from={SEEDED_STAGE} "
                             f"{SEEDED_COPY[0]} {SEEDED_COPY[1]}")
                continue
            matched_direct.add((SEEDED_TREE, SEEDED_TREE))
            continue
        # The image stages take build outputs from the payload stage, an exact
        # copy of what the builder stage packed from the compiled tree.
        if source_stage not in (COMPILED_CONTEXT, BUILDER_STAGE, PAYLOAD_STAGE) \
                or instruction.stage in (BUILDER_STAGE, PAYLOAD_STAGE):
            continue
        for source, destination in pairs:
            key = (source.removeprefix("/out/") if source.startswith("/out/") else source, destination)
            if key in direct:
                matched_direct.add(key)
                continue
            output = key[0]
            report.error(
                where,
                f"COPY --from={source_stage} {source} -> {destination} installs a Tilo build output outside "
                "os/components.toml",
                f"delete this COPY and {snippet(Path(output.rstrip('/')).name or 'component', output, destination)}"
                "      The component-files stage then installs it with its layer. Only outputs the build "
                'writes at their final path may stay a COPY, declared with layer = "direct".')

    for layer in sorted(layers):
        lines = consumed.get(layer, [])
        if len(lines) != 1:
            state = "never" if not lines else f"{len(lines)} times (lines {', '.join(map(str, lines))})"
            owners = sorted({e.component for e in files if e.layer == layer})
            report.error(f"{relative}", f"manifest layer {layer!r} (components {', '.join(owners)}) is "
                         f"copied into the image {state}",
                         f"apply it exactly once, in the stage that should commit it:\n"
                         f"        COPY --from={INSTALL_STAGE} /image/{layer}/ /")
    for key, entry in direct.items():
        if key not in matched_direct:
            source = key[0] if key[0].startswith("/") else f"/out/{key[0]}"
            report.error(report.manifest_line(entry.component),
                         f"direct file {entry.to} (component {entry.component!r}) is never copied by "
                         f"{relative}",
                         f"add `COPY --from={PAYLOAD_STAGE} {source} {entry.to}` to the rootfs stage, or give the "
                         "entry a real layer so the installer places it.")

    install_runs = [i for i in parsed if i.stage == INSTALL_STAGE and i.words and i.words[0].upper() == "RUN"]
    if not any("install-components.py" in i.text and "--all" in i.words and "--by-layer" in i.words
               for i in install_runs):
        report.error(f"{relative}", f"no `{INSTALL_STAGE}` stage runs install-components.py --by-layer --all",
                     f"restore the stage: FROM ${{BASE_REF}} AS {INSTALL_STAGE}, then RUN "
                     "python3 .../install-components.py --from /out --dest /image "
                     "--by-layer --all")

    # Build/stage drift: what the compile builds and stages versus the manifest.
    release = [c for c in production if c.profile == "release"]
    declared_bins = {b: c.name for c in release for b in c.bins}
    declared_outputs = {e.source for e in files if e.source and not e.source.startswith("/")}
    builder_runs = compile_steps(compile_script)
    for instruction in builder_runs:
        where = f"{compiler}:{instruction.line}"
        body = instruction.text
        for binary in sorted(set(re.findall(r"--bin\s+([A-Za-z0-9_-]+)", body))):
            if binary not in declared_bins:
                owner = next((e.component for e in files if e.source == binary), None)
                fix = (f'add "{binary}" to `bins` of component {owner!r} in os/components.toml, which '
                       "already installs that output." if owner else snippet(binary, binary, f"/usr/bin/{binary}"))
                report.error(where, f"{compiler} compiles --bin {binary}, which no os/components.toml "
                             "component declares", fix)
        for output in sorted(set(re.findall(r"(?:target/release|desktop-artifacts)/([A-Za-z0-9_-]+)", body))):
            if output not in declared_outputs:
                report.error(where, f"{compiler} stages build output {output!r}, which os/components.toml "
                             "does not install", snippet(output, output, f"/usr/bin/{output}"))
    builder_text = "\n".join(i.text for i in builder_runs)
    for component in release:
        where = report.manifest_line(component.name)
        for package in component.packages:
            if not re.search(rf"-p\s+{re.escape(package)}(\s|$|\")", builder_text):
                report.error(where, f"component {component.name!r} declares package {package!r}, which "
                             f"{compiler} never builds",
                             f"add `-p {package}` to its cargo invocation, or fix `packages`.")
        for binary in component.bins:
            if not re.search(rf"--bin\s+{re.escape(binary)}(\s|$|\")", builder_text):
                report.error(where, f"component {component.name!r} declares bin {binary!r}, which "
                             f"{compiler} never builds",
                             f"add `--bin {binary}` to its cargo invocation, or fix `bins`.")
        for feature in component.features:
            if feature not in builder_text:
                report.error(where, f"component {component.name!r} declares feature {feature!r}, which "
                             f"{compiler} never enables",
                             "enable it in its cargo invocation, or drop it from `features`.")
    thin_text = thin_script.read_text(encoding="utf-8") if thin_script.is_file() else ""
    for component in manifest.components:
        if component.profile != "thin-app":
            continue
        where = report.manifest_line(component.name)
        for package in component.packages:
            if f"-p {package}" not in thin_text:
                report.error(where, f"thin-app package {package!r} is not built by "
                             f"{thin_script.relative_to(report.repo)}",
                             "build it in that script's single cargo graph, or fix `packages`.")
        match = re.search(r'APP_FEATURES="([^"]*)"', builder_text)
        image_features = set(match.group(1).split(",")) if match else set()
        for feature in component.features:
            if feature not in image_features:
                report.error(where, f"feature {feature!r} is declared but missing from APP_FEATURES in "
                             f"{compiler}", "add it to APP_FEATURES, or drop it from `features`.")
        for feature in sorted(image_features - set(component.features)):
            report.error(f"{compiler}", f"APP_FEATURES enables {feature!r}, which component "
                         f"{component.name!r} does not declare",
                         f"add \"{feature}\" to `features` of {component.name!r} in os/components.toml.")


def audit(repo: Path) -> list[str]:
    report = Report(repo)
    installer = load_installer(repo)
    try:
        manifest = installer.load_manifest(repo / "os/components.toml")
    except (installer.ManifestError, OSError) as error:
        component = re.search(r"component '([^']+)'", str(error))
        report.error(report.manifest_line(component.group(1) if component else None),
                     f"os/components.toml is invalid: {error}",
                     "correct the entry; the schema is described in the header of os/components.toml.")
        return report.errors
    containerfile = repo / "os/Containerfile"
    check_manifest_against_workspace(report, manifest, workspace_packages(repo))
    check_units(report, manifest, containerfile.read_text(encoding="utf-8"))
    check_containerfile(report, manifest, installer, containerfile, repo / "os/scripts/build-thin-apps.sh",
                        repo / COMPILE_SCRIPT)
    return report.errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", type=Path, default=REPO, help="repository root (default: this checkout)")
    args = parser.parse_args(argv)
    errors = audit(args.repo.resolve())
    if errors:
        print(f"component manifest check failed ({len(errors)} problem(s)):\n", file=sys.stderr)
        print("\n\n".join(errors), file=sys.stderr)
        return 1
    print(f"os/components.toml matches os/Containerfile, {COMPILE_SCRIPT} and the Cargo workspace")
    return 0


if __name__ == "__main__":
    sys.exit(main())
