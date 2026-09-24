#!/usr/bin/env python3
"""Keep the dev image and the production image apart.

os/Containerfile.dev builds the dev VM image FROM the production image: it adds
an SSH server, `dev-overrides` binaries, tilo-control, tilo-vmd and a build
toolchain, and it is unsealed. Two ways that could leak into production:

1. The dev image is published or tagged as a production image. So:
   * os/Containerfile.dev must label and mark itself as the dev flavour, unseal
     its base and never build a UKI of its own;
   * its RPM snapshot default must be the production one (os/rpms-snapshot.env),
     and its toolchain list (os/scripts/builder-packages.txt) must be exactly
     what os/Containerfile's builder stage installs, so it compiles with what
     the production builder of the same commit compiles with;
   * every script or workflow that builds os/Containerfile.dev may tag and push
     it only under a `tilo-vm` name, never code.podesta.ai/qos/tilo or
     localhost/tilo;
   * production container policy must reject code.podesta.ai/qos/tilo-vm, so a
     production machine never switches to a dev image.
2. A production Containerfile builds or installs a dev-only piece: a dev-only
   component of os/components.toml (its layer, bins, packages or paths), a dev
   feature, or a dev-image source file.

Every finding names file:line, why it matters, and the fix.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEV_CONTAINERFILE = "os/Containerfile.dev"
DEV_REPOSITORY = "code.podesta.ai/qos/tilo-vm"
RERUN = "python3 tools/policy/check_dev_image_contract.py   (or tools/policy/run.sh)"
WHY_FLAVOUR = ("the dev image accepts a fixed lock-screen password, opens a session-driving socket and "
               "runs sshd; if it is ever tagged, pushed or installed as the production image, every "
               "machine that updates from it inherits all three")
WHY_LEAK = ("production images are built from os/Containerfile* and must never contain dev-only "
            "components, dev features or dev-image files; each one weakens the shipped system")
# Dev-image sources that only os/Containerfile.dev may use.
DEV_SOURCES = ("os/dev-image/", "crates/dev/tilo-vmd/data", "os/scripts/unseal-rootfs.py",
               "os/scripts/check-dev-kernel.sh", "os/scripts/finalize-dev-image.py", "Containerfile.dev")
PRODUCTION_NAME = re.compile(r"(?:code\.podesta\.ai/qos/tilo|localhost/tilo)(?=[:@\s\"']|$)")


@dataclass(frozen=True)
class Finding:
    where: str
    what: str
    why: str
    fix: str

    def __str__(self) -> str:
        return f"{self.where}: {self.what}\n  why: {self.why}.\n  fix: {self.fix}\n  re-run: {RERUN}"


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def logical_lines(path: Path) -> list[tuple[int, str]]:
    """(first line number, joined text) of backslash-continued lines, comments dropped."""
    result: list[tuple[int, str]] = []
    pending, start = "", 0
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        if not pending:
            start = number
        continued = stripped.endswith("\\")
        pending = f"{pending} {stripped[:-1] if continued else stripped}".strip()
        if not continued:
            if pending:
                result.append((start, pending))
            pending = ""
    if pending:
        result.append((start, pending))
    return result


def rel(repo: Path, path: Path) -> str:
    return str(path.relative_to(repo))


def production_containerfiles(repo: Path) -> list[Path]:
    return sorted(p for p in (repo / "os").glob("Containerfile*")
                  if p.is_file() and p.name != Path(DEV_CONTAINERFILE).name)


# ---- 1. the dev image is never the production image ---------------------------

def check_dev_containerfile(repo: Path) -> list[Finding]:
    path = repo / DEV_CONTAINERFILE
    shown = DEV_CONTAINERFILE
    if not path.is_file():
        return [Finding(shown, "is missing", WHY_FLAVOUR,
                        "restore os/Containerfile.dev; the dev-disk workflow builds it")]
    lines = logical_lines(path)
    findings: list[Finding] = []
    stages = [i for i, (_, text) in enumerate(lines) if re.match(r"FROM\s", text, re.I)]
    final = lines[stages[-1]:] if stages else lines
    final_text = "\n".join(text for _, text in final)
    last_line = final[0][0] if final else 1
    if not re.search(r'LABEL\b.*\bsh\.tilo\.image\.flavour="?dev"?', final_text):
        findings.append(Finding(f"{shown}:{last_line}", "the final stage does not label the image "
                                'sh.tilo.image.flavour="dev"', WHY_FLAVOUR,
                                'add `LABEL sh.tilo.image.flavour="dev"` to the last stage.'))
    if "finalize-dev-image.py" not in final_text:
        findings.append(Finding(f"{shown}:{last_line}", "the final stage never runs "
                                "os/scripts/finalize-dev-image.py, which writes the dev flavour marker "
                                "and the dev-only update policy", WHY_FLAVOUR,
                                "run it in the last stage, after every file is in place."))
    if not any(re.search(r"\bpython3\s+\S*unseal-rootfs\.py\s+/(?:\s|$)", text) for _, text in lines):
        findings.append(Finding(shown, "never runs os/scripts/unseal-rootfs.py", WHY_FLAVOUR +
                                "; and a sealed UKI over a changed rootfs cannot boot at all",
                                "run `python3 .../unseal-rootfs.py /` right after `FROM ${BASE_IMAGE}`."))
    for number, text in lines:
        if re.search(r"\bbootc\s+container\s+ukify\b|/boot/EFI/Linux", text):
            findings.append(Finding(f"{shown}:{number}", "builds or installs a UKI", WHY_FLAVOUR +
                                    "; a sealed dev image looks like a production one to bootc and "
                                    "cannot merge dev layers into /usr",
                                    "remove it: the dev image boots unsealed from /usr/lib/modules."))
    default = next(((n, m.group(1)) for n, t in lines if (m := re.fullmatch(r"ARG RPMS_REF=(\S+)", t))), None)
    if default is not None:
        findings.append(Finding(f"{shown}:{default[0]}", f"RPMS_REF has a default ({default[1]})",
                                "os/rpms-snapshot.env is the one pin: CI rewrites it when the package set "
                                "changes, and a second copy here would silently drift from it",
                                "declare a bare `ARG RPMS_REF`; the builders pass it from os/rpms-snapshot.env."))
    elif not any(re.fullmatch(r"ARG RPMS_REF", t) for _, t in lines):
        findings.append(Finding(shown, "declares no `ARG RPMS_REF`",
                                "the toolchain must come from the frozen snapshot the production builder used",
                                "add a bare `ARG RPMS_REF` before the first FROM."))
    for builder in ("os/dev/build-dev-image.sh", ".forgejo/workflows/dev-disk.yml"):
        path = repo / builder
        if path.is_file():
            body = path.read_text(encoding="utf-8")
            if not ("rpms-snapshot.env" in body and "RPMS_REF=$rpms_ref" in body):
                findings.append(Finding(builder, "builds os/Containerfile.dev without RPMS_REF from os/rpms-snapshot.env",
                                        "without it the dev image has no frozen snapshot to install its toolchain from",
                                        'read `rpms_ref` from os/rpms-snapshot.env and pass `--build-arg "RPMS_REF=$rpms_ref"`.'))
    return findings


def check_builder_packages(repo: Path) -> list[Finding]:
    """The dev toolchain list must equal what os/Containerfile's builder stage installs."""
    generator = repo / "os/scripts/builder-packages.py"
    listing = repo / "os/scripts/builder-packages.txt"
    if not generator.is_file():
        return []
    module = load("builder_packages_for_dev_contract", generator)
    try:
        committed = [line.strip() for line in listing.read_text(encoding="utf-8").splitlines()
                     if line.strip() and not line.startswith("#")]
    except OSError:
        committed = []
    findings = []
    for arch in ("x86_64", "aarch64"):
        wanted = module.builder_packages(repo / "os/Containerfile", "builder", arch)
        if committed != wanted:
            added = sorted(set(wanted) - set(committed))
            removed = sorted(set(committed) - set(wanted))
            findings.append(Finding(
                rel(repo, listing), f"differs from the packages os/Containerfile's builder stage installs "
                f"on {arch} (missing: {', '.join(added) or '-'}; extra: {', '.join(removed) or '-'})",
                "the dev image compiles its binaries and in-VM builds with this list, so it must be "
                "exactly the production builder's toolchain",
                "run `python3 os/scripts/builder-packages.py --write` and commit the result."))
            break
    return findings


def builders_of_dev_image(repo: Path) -> list[Path]:
    candidates = [*sorted((repo / ".forgejo/workflows").glob("*.y*ml")),
                  *sorted((repo / "os").rglob("*.sh")), *sorted((repo / "tools").rglob("*.sh"))]
    return [p for p in candidates if p.is_file() and "Containerfile.dev" in p.read_text(encoding="utf-8")]


def check_dev_image_names(repo: Path) -> list[Finding]:
    """Scripts and workflows that build the dev image tag and push only tilo-vm names."""
    findings: list[Finding] = []
    for path in builders_of_dev_image(repo):
        shown = rel(repo, path)
        for number, text in logical_lines(path):
            destinations: list[str] = []
            destinations += re.findall(r"(?:--tag|(?<![\w-])-t)[ =]+(\"[^\"]*\"|'[^']*'|\S+)", text)
            if re.search(r"\b(?:podman|buildah|docker)\s+(?:image\s+)?(?:push|tag)\b", text):
                words = re.findall(r"\"[^\"]*\"|'[^']*'|\S+", text)
                destinations += [w for w in words[-1:] if not w.startswith("-")]
            if re.search(r"\bskopeo\s+copy\b", text):
                words = re.findall(r"\"[^\"]*\"|'[^']*'|\S+", text)
                destinations += [w for w in words[-1:] if not w.startswith("-")]
            assignment = re.match(r"(?:export\s+|-?\s*)?([A-Z_]*DEV[A-Z_]*IMAGE[A-Z_]*)\s*[:=]\s*(\S+)", text)
            if assignment:
                destinations.append(assignment.group(2))
            for destination in destinations:
                value = destination.strip("\"'")
                if PRODUCTION_NAME.search(value):
                    findings.append(Finding(
                        f"{shown}:{number}", f"a file that builds os/Containerfile.dev names the "
                        f"production image {value!r} as a tag or push destination", WHY_FLAVOUR,
                        "tag and push the dev image only as localhost/tilo-vm:<tag> or "
                        f"{DEV_REPOSITORY}:<tag>."))
    return findings


def check_production_policy(repo: Path) -> list[Finding]:
    path = repo / "os/etc/containers/policy.json"
    shown = rel(repo, path)
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [Finding(shown, f"cannot be read: {error}", WHY_FLAVOUR, "restore a valid policy.json.")]
    scope = policy.get("transports", {}).get("docker", {}).get(DEV_REPOSITORY)
    if scope != [{"type": "reject"}]:
        line = next((n for n, t in enumerate(path.read_text(encoding="utf-8").splitlines(), 1)
                     if DEV_REPOSITORY in t), 1)
        return [Finding(f"{shown}:{line}", f"production policy does not reject {DEV_REPOSITORY} "
                        f"(it has {scope!r})", WHY_FLAVOUR + "; dev images are unsigned by design, and the "
                        "default policy would accept them",
                        f'set transports.docker["{DEV_REPOSITORY}"] to [{{"type": "reject"}}]; only the dev '
                        "image's own policy (os/scripts/finalize-dev-image.py) admits it.")]
    return []


# ---- 2. production images carry nothing dev-only ------------------------------

def dev_only_needles(repo: Path) -> list[tuple[re.Pattern[str], str]]:
    installer = load("install_components_for_dev_contract", repo / "os/scripts/install-components.py")
    manifest = installer.load_manifest(repo / "os/components.toml")
    production_packages = {p for c in manifest.production_components() for p in c.packages}
    needles: dict[str, tuple[re.Pattern[str], str]] = {}

    def add(pattern: str, label: str) -> None:
        needles.setdefault(pattern, (re.compile(pattern), label))

    dev_layers: dict[str, list[str]] = {}
    for component in manifest.components:
        for entry in component.files:
            if entry.dev_only and component.name not in dev_layers.setdefault(entry.layer, []):
                dev_layers[entry.layer].append(component.name)
    for layer, owners in sorted(dev_layers.items()):
        add(rf"/image/{re.escape(layer)}/", f"dev-only layer {layer!r} (components {', '.join(owners)})")
    for component in manifest.components:
        for feature in component.dev_features:
            add(rf"(?<![\w/-]){re.escape(feature)}(?![\w-])", f"dev feature {feature!r}")
        if not component.dev_only:
            continue
        what = f"dev-only component {component.name!r}"
        for entry in component.files:
            add(rf"(?<![\w.-]){re.escape(entry.to)}(?![\w.-])", f"{what} (it installs {entry.to})")
        for binary in component.bins:
            add(rf"(?:--bin\s+|target/[a-z-]+/|/out/){re.escape(binary)}(?![\w-])", f"{what} (bin {binary!r})")
        for package in component.packages:
            if package not in production_packages:
                add(rf"-p\s+{re.escape(package)}(?![\w-])", f"{what} (package {package!r})")
    for source in DEV_SOURCES:
        add(rf"(?<![\w-]){re.escape(source)}", f"dev-image source {source}")
    return list(needles.values())


def check_production_containerfiles(repo: Path) -> list[Finding]:
    findings: list[Finding] = []
    needles = dev_only_needles(repo)
    for path in production_containerfiles(repo):
        shown = rel(repo, path)
        for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if raw.lstrip().startswith("#"):
                continue
            for pattern, label in needles:
                if pattern.search(raw):
                    findings.append(Finding(
                        f"{shown}:{number}", f"production image build references {label}", WHY_LEAK,
                        "remove it from this production Containerfile; dev-only pieces are built and "
                        f"installed by {DEV_CONTAINERFILE} alone (install-components.py --dev)."))
    return findings


def audit(repo: Path = REPO) -> list[Finding]:
    return [*check_dev_containerfile(repo), *check_builder_packages(repo), *check_dev_image_names(repo),
            *check_production_policy(repo),
            *check_production_containerfiles(repo)]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", type=Path, default=REPO)
    args = parser.parse_args(argv)
    findings = audit(args.repo.resolve())
    if findings:
        print(f"dev image contract failed ({len(findings)} problem(s)):\n", file=sys.stderr)
        print("\n\n".join(map(str, findings)), file=sys.stderr)
        return 1
    print(f"Dev image contract OK ({len(production_containerfiles(args.repo.resolve()))} production "
          "Containerfiles carry nothing dev-only)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
