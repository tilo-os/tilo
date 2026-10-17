#!/usr/bin/python3
"""Resolve a downloadable system extension for one published OS image.

    resolve.py resolve --recipe DIR/recipe.toml --image REF@sha256:... --out manifest.json
    resolve.py fingerprint            print this system's base package fingerprint
    resolve.py check MANIFEST         validate a manifest; print "id arch fingerprint revision"
    resolve.py same OLD NEW           exit 0 when NEW publishes nothing OLD does not

`resolve` runs inside the OS image it resolves for (CI starts the published
image with `podman run`), as root and with network: the image's own rpm
database is the base, its own dnf5 talks to the recipe's repositories, and its
own rpmkeys checks every package. It writes the unsigned manifest of
os/usr/share/tilo/system-extensions/README.md ("Downloaded system extensions"):
for each part of the recipe, the upstream packages a device may download for
exactly this base, each pinned by URL, size and SHA-256.

Nothing the base installed may be replaced, only depended on: every base
package name is excluded from every repository. Every package must carry the
signature of the key its repository is pinned to, checked in an isolated
keyring holding only the recipe's pinned keys. Every published URL has served
exactly the verified bytes over HTTPS before it is written.
"""

from __future__ import annotations

import argparse
import base64
import configparser
import dataclasses
import datetime
import functools
import hashlib
import http.client
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable, Iterable

MANIFEST_TYPE = "tilo.system-extension.sysext-manifest"
MANIFEST_SCHEMA = 1
FORMAT = "rpm"
# A prebuilt systemd-sysext image: one artifact, built by Tilo CI, pinned by size
# and SHA-256 (tools/extensions/sysext_image.py).
RAW_FORMAT = "sysext-raw"
PART_STATES = ("available", "waiting", "unsupported")
RULES = ("exact", "compatible", "newest")
ARCHITECTURES = ("x86_64", "aarch64")

# The base package fingerprint. The device computes the same value from its
# own rpm database (crates/system/extension-helper); a change here is a change
# of the published URL layout and of the contract.
FINGERPRINT_DOMAIN = b"tilo.base-packages.v1\n"
RPM_FINGERPRINT_QUERY = "%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n"
FINGERPRINT_EXCLUDED = frozenset({"gpg-pubkey"})

HEX64 = re.compile(r"[0-9a-f]{64}")
IMAGE_REF = re.compile(r"[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+@sha256:[0-9a-f]{64}")
NVRA_SAFE = re.compile(r"[A-Za-z0-9._+~^-]+")


class ResolveError(Exception):
    """A resolution that must not produce a manifest."""


# ---------------------------------------------------------------------------
# Base package fingerprint
# ---------------------------------------------------------------------------

def base_package_lines(rpm_output: str) -> list[str]:
    """`rpm -qa --qf RPM_FINGERPRINT_QUERY` output as fingerprint lines."""
    lines = []
    for line in rpm_output.splitlines():
        if not line.strip():
            continue
        if line.split(" ", 1)[0] in FINGERPRINT_EXCLUDED:
            continue
        lines.append(line + "\n")
    return lines


def base_packages_sha256(lines: Iterable[str]) -> str:
    """SHA-256 of the domain line and the package lines, sorted bytewise."""
    digest = hashlib.sha256(FINGERPRINT_DOMAIN)
    for line in sorted(line.encode() for line in lines):
        digest.update(line)
    return digest.hexdigest()


# ---------------------------------------------------------------------------
# Versions and candidates
# ---------------------------------------------------------------------------

def _vercmp_segments(value: str) -> list[str]:
    return re.findall(r"~|\^|[0-9]+|[A-Za-z]+", value)


def rpmvercmp(left: str, right: str) -> int:
    """rpm's version comparison (tilde sorts first, caret after the base)."""
    if left == right:
        return 0
    a = _vercmp_segments(left)
    b = _vercmp_segments(right)
    while a or b:
        x = a.pop(0) if a else None
        y = b.pop(0) if b else None
        if x == "~" or y == "~":
            if x != y:
                return -1 if x == "~" else 1
            continue
        if x == "^" or y == "^":
            if x is None:
                return -1
            if y is None:
                return 1
            if x != y:
                return -1 if x == "^" else 1
            continue
        if x is None:
            return -1
        if y is None:
            return 1
        if x.isdigit() != y.isdigit():
            return 1 if x.isdigit() else -1
        if x.isdigit():
            xi, yi = int(x), int(y)
            if xi != yi:
                return 1 if xi > yi else -1
        elif x != y:
            return 1 if x > y else -1
    return 0


@dataclasses.dataclass(frozen=True)
class Nevra:
    name: str
    epoch: int
    version: str
    release: str
    arch: str
    repo: str = ""
    location: str = ""

    @property
    def evr(self) -> str:
        prefix = f"{self.epoch}:" if self.epoch else ""
        return f"{prefix}{self.version}-{self.release}"

    @property
    def epochnum_evr(self) -> str:
        """The `epochnum:version-release` form requires_base uses."""
        return f"{self.epoch}:{self.version}-{self.release}"

    @property
    def nvra(self) -> str:
        return f"{self.name}-{self.version}-{self.release}.{self.arch}"

    @property
    def spec(self) -> str:
        return f"{self.name}-{self.evr}.{self.arch}"

    def compare(self, other: "Nevra") -> int:
        if self.epoch != other.epoch:
            return 1 if self.epoch > other.epoch else -1
        return rpmvercmp(self.version, other.version) or rpmvercmp(self.release, other.release)


def parse_nevra_line(line: str) -> Nevra | None:
    """`name epoch version release arch [repo [location]]`, or None."""
    fields = line.split()
    if len(fields) not in (5, 6, 7):
        return None
    name, epoch, version, release, arch, *rest = fields
    repo = rest[0] if rest else ""
    location = rest[1] if len(rest) > 1 else ""
    return Nevra(name, int(epoch) if epoch.isdigit() else 0, version, release, arch, repo, location)


def newest(candidates: list[Nevra]) -> Nevra | None:
    best: Nevra | None = None
    for candidate in candidates:
        if best is None or candidate.compare(best) > 0:
            best = candidate
    return best


def _major_minor(version: str) -> tuple[str, ...]:
    return tuple(version.split(".")[:2])


def choose_candidate(rule: str, base_version: str | None, candidates: list[Nevra]) -> Nevra | None:
    """The build of one component for this base, or None to wait."""
    if rule == "newest":
        return newest(candidates)
    if base_version is None:
        return None
    exact = [c for c in candidates if c.version == base_version]
    if exact:
        return newest(exact)
    if rule == "exact":
        return None
    if rule != "compatible":
        raise ValueError(f"unknown rule {rule}")
    older = [
        c for c in candidates
        if _major_minor(c.version) == _major_minor(base_version)
        and rpmvercmp(c.version, base_version) < 0
    ]
    return newest(older)


# ---------------------------------------------------------------------------
# Signatures
# ---------------------------------------------------------------------------

def openpgp_fingerprint(armored: str) -> str:
    """The v4 fingerprint of the primary key in an ASCII-armoured public key."""
    body = []
    inside = False
    for line in armored.splitlines():
        line = line.strip()
        if line.startswith("-----BEGIN PGP PUBLIC KEY BLOCK"):
            inside = True
            continue
        if line.startswith("-----END PGP PUBLIC KEY BLOCK"):
            break
        if not inside or not line or ":" in line and not body:
            continue
        if line.startswith("="):
            break
        body.append(line)
    data = base64.b64decode("".join(body))
    if not data:
        raise ResolveError("empty key")
    tag_byte = data[0]
    if not tag_byte & 0x80:
        raise ResolveError("not an OpenPGP packet")
    if tag_byte & 0x40:  # new format
        tag = tag_byte & 0x3F
        first = data[1]
        if first < 192:
            length, offset = first, 2
        elif first < 224:
            length, offset = ((first - 192) << 8) + data[2] + 192, 3
        elif first == 255:
            length, offset = int.from_bytes(data[2:6], "big"), 6
        else:
            raise ResolveError("partial-length key packet")
    else:  # old format
        tag = (tag_byte >> 2) & 0x0F
        size = {0: 1, 1: 2, 2: 4}.get(tag_byte & 0x03)
        if size is None:
            raise ResolveError("indeterminate-length key packet")
        length, offset = int.from_bytes(data[1:1 + size], "big"), 1 + size
    if tag != 6:
        raise ResolveError("first packet is not a public key")
    packet = data[offset:offset + length]
    if len(packet) != length or not packet or packet[0] != 4:
        raise ResolveError("only v4 public keys are supported")
    return hashlib.sha1(b"\x99" + length.to_bytes(2, "big") + packet).hexdigest()


def parse_checksig(output: str) -> tuple[bool, set[str]]:
    """(all checks OK and signed, signing fingerprints) from `rpmkeys -Kv` output."""
    ok = True
    fingerprints = set()
    saw_signature = False
    for line in output.splitlines()[1:]:
        line = line.strip()
        if not line:
            continue
        if not line.endswith(": OK"):
            ok = False
        match = re.search(r"signature, key fingerprint: ([0-9a-f]{40}): OK$", line)
        if match:
            saw_signature = True
            fingerprints.add(match.group(1))
    return ok and saw_signature, fingerprints


# ---------------------------------------------------------------------------
# Recipe
# ---------------------------------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Component:
    name: str
    part: str
    rule: str
    follows: str | None = None
    arches: tuple[str, ...] = ()

    def applies_to(self, arch: str) -> bool:
        return not self.arches or arch in self.arches


@dataclasses.dataclass(frozen=True)
class Part:
    name: str
    waiting: str
    unsupported: str


@dataclasses.dataclass(frozen=True)
class Key:
    name: str
    file: str
    fingerprint: str | dict[str, str]

    def path(self, recipe_dir: Path, version_id: str) -> Path:
        path = Path(self.file.format(version_id=version_id))
        return path if path.is_absolute() else recipe_dir / path

    def pinned(self, version_id: str) -> str | None:
        if isinstance(self.fingerprint, str):
            return self.fingerprint
        return self.fingerprint.get(version_id)


@dataclasses.dataclass(frozen=True)
class Repo:
    name: str
    key: str
    publisher: str
    baseurl: str
    also: tuple[str, ...] = ()

    def base(self, releasever: str, basearch: str) -> str:
        url = self.baseurl.replace("$releasever", releasever).replace("$basearch", basearch)
        return url if url.endswith("/") else url + "/"


@dataclasses.dataclass(frozen=True)
class Recipe:
    directory: Path
    id: str
    os_id: str
    repo_file: Path
    parts: tuple[Part, ...]
    components: tuple[Component, ...]
    keys: dict[str, Key]
    repos: dict[str, Repo]

    def fingerprints_for(self, repo: str, version_id: str) -> set[str]:
        """The key fingerprints that may sign a package from `repo`."""
        entry = self.repos.get(repo)
        if entry is None:
            return set()
        pinned = self.keys[entry.key].pinned(version_id)
        return {pinned} if pinned else set()


def repo_ids(text: str) -> list[str]:
    return re.findall(r"^\[([A-Za-z0-9._-]+)\]\s*$", text, re.MULTILINE)


def repo_baseurls(text: str) -> dict[str, str]:
    """Each repository's single baseurl; a metalink or mirrorlist is refused."""
    parser = configparser.ConfigParser(interpolation=None)
    parser.read_string(text)
    found = {}
    for section in parser.sections():
        options = parser[section]
        if "metalink" in options or "mirrorlist" in options:
            raise ResolveError(f"[{section}]: only a canonical baseurl is allowed")
        baseurl = options.get("baseurl", "").strip()
        if not baseurl.startswith("https://") or len(baseurl.split()) != 1:
            raise ResolveError(f"[{section}]: needs exactly one https baseurl")
        found[section] = baseurl
    return found


def load_recipe(path: Path) -> Recipe:
    data = tomllib.loads(path.read_text())
    directory = path.parent
    if data.get("schema") != 1:
        raise ResolveError(f"{path}: unsupported recipe schema")
    ident = data.get("id", "")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", ident):
        raise ResolveError(f"{path}: invalid extension id {ident!r}")
    parts = tuple(Part(p["name"], p["waiting"], p["unsupported"]) for p in data["parts"])
    part_names = [p.name for p in parts]
    if len(set(part_names)) != len(part_names):
        raise ResolveError(f"{path}: duplicate part")
    components = tuple(
        Component(c["name"], c["part"], c["rule"], c.get("follows"), tuple(c.get("arches", ())))
        for c in data["components"]
    )
    for component in components:
        if component.part not in part_names:
            raise ResolveError(f"{component.name}: unknown part {component.part}")
        if component.rule not in RULES:
            raise ResolveError(f"{component.name}: unknown rule {component.rule}")
        if component.rule != "newest" and not component.follows:
            raise ResolveError(f"{component.name}: rule {component.rule} needs `follows`")
        if not set(component.arches) <= set(ARCHITECTURES):
            raise ResolveError(f"{component.name}: unknown architecture in {component.arches}")
    keys = {name: Key(name, k["file"], k["fingerprint"]) for name, k in data["keys"].items()}
    for key in keys.values():
        values = [key.fingerprint] if isinstance(key.fingerprint, str) else list(key.fingerprint.values())
        if not values or not all(re.fullmatch(r"[0-9a-f]{40}", v) for v in values):
            raise ResolveError(f"key {key.name}: fingerprints must be 40 lowercase hex digits")
    repo_file = directory / data["repo_file"]
    baseurls = repo_baseurls(repo_file.read_text())
    if sorted(baseurls) != sorted(data["repos"]):
        raise ResolveError(f"{repo_file.name} defines {sorted(baseurls)}, the recipe describes {sorted(data['repos'])}")
    repos = {name: Repo(name, r["key"], r["publisher"], baseurls[name], tuple(r.get("also", ())))
             for name, r in data["repos"].items()}
    for repo in repos.values():
        if repo.key not in keys:
            raise ResolveError(f"repo {repo.name}: unknown key {repo.key}")
        if not all(u.startswith("https://") for u in repo.also):
            raise ResolveError(f"repo {repo.name}: every URL template must be https")
    return Recipe(directory, ident, data["os_id"], repo_file, parts, components, keys, repos)


# ---------------------------------------------------------------------------
# Decisions
# ---------------------------------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Base:
    os_id: str
    version_id: str
    arch: str
    installed: dict[str, Nevra]
    packages_sha256: str
    image: str

    def version_of(self, name: str | None) -> str | None:
        if name is None or name not in self.installed:
            return None
        return self.installed[name].version


@dataclasses.dataclass(frozen=True)
class PartDecision:
    state: str
    chosen: tuple[Nevra, ...]
    message: str
    requires_base: dict[str, str]


def decide_part(part: Part, components: Iterable[Component], base: Base,
                candidates: dict[str, list[Nevra]]) -> PartDecision:
    """Which builds a part gets on this base, or why it waits or never applies.

    A component the base already ships needs nothing. A component following a
    base package the base does not have never applies to it.
    """
    components = list(components)
    applicable = [c for c in components if c.part == part.name and c.applies_to(base.arch)
                  and c.name not in base.installed]
    supported = [c for c in applicable if c.follows is None or c.follows in base.installed]
    if not supported:
        return PartDecision("unsupported", (), part.unsupported, {})
    chosen, waiting, requires = [], [], {}
    for component in supported:
        offered = candidates.get(component.name, [])
        choice = choose_candidate(component.rule, base.version_of(component.follows), offered)
        if choice is None:
            waiting.append((component, offered))
            continue
        chosen.append(choice)
        if component.follows:
            requires[component.follows] = base.installed[component.follows].epochnum_evr
    details = []
    for component, offered in waiting:
        versions = ", ".join(sorted({c.version for c in offered}, key=_version_key)) or "none"
        if component.follows:
            details.append(f"{component.name} needs {component.follows} "
                           f"{base.version_of(component.follows)}; published: {versions}.")
        else:
            details.append(f"{component.name}: no build is published.")
    if not chosen:
        return PartDecision("waiting", (), " ".join([part.waiting, *details]), {})
    names = {candidate.name for candidate in chosen}
    if ("gstreamer1-plugin-openh264" in names
            and "openh264" not in names and "openh264" not in base.installed):
        return PartDecision("waiting", (),
                            "OpenH264 is waiting for Cisco's codec library; the compatibility stub cannot encode or decode video.", {})
    # A component already supplied by the base is still a runtime dependency.
    # Pin it so retaining this part across an OS update cannot silently remove it.
    for component in components:
        if component.part == part.name and component.applies_to(base.arch) and component.name in base.installed:
            requires[component.name] = base.installed[component.name].epochnum_evr
    message = " ".join(["Not yet available for this release:", *details]) if details else ""
    return PartDecision("available", tuple(sorted(chosen, key=lambda n: n.name)), message,
                        dict(sorted(requires.items())))


_version_key = functools.cmp_to_key(rpmvercmp)


# ---------------------------------------------------------------------------
# Manifest
# ---------------------------------------------------------------------------

def artifact_urls(recipe: Recipe, offers: list[Nevra], releasever: str, basearch: str) -> list[str]:
    """Canonical upstream URLs for one package from every repository offering it.

    dnf reports a package's location as its repository's baseurl plus the
    location_href of the metadata; the baseurl is the publisher's canonical
    one, so that URL is canonical too.
    """
    urls = []
    for offer in offers:
        repo = recipe.repos.get(offer.repo)
        if repo is None:
            continue
        base = repo.base(releasever, basearch)
        if not offer.location.startswith(base):
            raise ResolveError(f"{offer.nvra}: location {offer.location!r} is not below {base}")
        href = offer.location[len(base):]
        if (not href or href.startswith("/") or ".." in href.split("/")
                or not re.fullmatch(r"[A-Za-z0-9._+~^/-]+", href)):
            raise ResolveError(f"{offer.nvra}: unsafe location {href!r} in {offer.repo}")
        urls.append(base + href)
        for template in repo.also:
            urls.append(template.format(releasever=releasever, basearch=basearch, location=href,
                                        filename=href.rsplit("/", 1)[-1]))
    return list(dict.fromkeys(urls))


def manifest_document(recipe: Recipe, base: Base, parts: list[dict], when: datetime.datetime) -> dict:
    when = when.astimezone(datetime.timezone.utc).replace(microsecond=0)
    return {
        "type": MANIFEST_TYPE,
        "schema": MANIFEST_SCHEMA,
        "id": recipe.id,
        "revision": int(when.strftime("%Y%m%d%H%M%S")),
        "created": when.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "base": {
            "os_id": base.os_id,
            "version_id": base.version_id,
            "architecture": base.arch,
            "packages_sha256": base.packages_sha256,
            "image": base.image,
        },
        "parts": sorted(parts, key=lambda p: p["name"]),
    }


def part_document(name: str, decision: PartDecision, artifacts: list[dict]) -> dict:
    return {
        "name": name,
        "format": FORMAT,
        "state": decision.state,
        "message": decision.message,
        "requires_base": dict(sorted(decision.requires_base.items())),
        "artifacts": sorted(artifacts, key=lambda a: a["package"]),
    }


def render(manifest: dict) -> bytes:
    return (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode()


def publication_content(manifest: dict) -> dict:
    """What a publication pins; revision, creation time and the image name do not count."""
    content = json.loads(json.dumps(manifest))
    content.pop("revision", None)
    content.pop("created", None)
    content.get("base", {}).pop("image", None)
    return content


def same_publication(old: dict, new: dict) -> bool:
    return publication_content(old) == publication_content(new)


def qualification_digest(manifest: dict) -> str:
    """Bind reviewed artifacts, constraints and image; ignore only clock fields."""
    content = json.loads(json.dumps(manifest))
    content.pop("revision", None)
    content.pop("created", None)
    encoded = json.dumps(content, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    return hashlib.sha256(encoded).hexdigest()


def check_manifest(manifest: dict) -> list[str]:
    """Shape problems that must stop a publication (empty when it is sound)."""
    problems = []
    if manifest.get("type") != MANIFEST_TYPE or manifest.get("schema") != MANIFEST_SCHEMA:
        problems.append("wrong type or schema")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", str(manifest.get("id", ""))):
        problems.append("invalid id")
    revision = manifest.get("revision")
    if not isinstance(revision, int) or not 20000101000000 <= revision <= 99991231235959:
        problems.append("revision must be a UTC YYYYMMDDHHMMSS integer")
    base = manifest.get("base") or {}
    if base.get("architecture") not in ARCHITECTURES:
        problems.append("unsupported architecture")
    if not HEX64.fullmatch(str(base.get("packages_sha256", ""))):
        problems.append("packages_sha256 must be 64 hex digits")
    if not IMAGE_REF.fullmatch(str(base.get("image", ""))):
        problems.append("base.image must be ref@sha256:<64 hex>")
    for key in ("os_id", "version_id"):
        if not re.fullmatch(r"[a-z0-9._-]+", str(base.get(key, ""))):
            problems.append(f"invalid base.{key}")
    parts = manifest.get("parts")
    if not isinstance(parts, list) or not parts:
        problems.append("no parts")
        parts = []
    names = [p.get("name") for p in parts]
    if names != sorted(set(names)):
        problems.append("parts must be unique and sorted")
    for part in parts:
        name = part.get("name")
        if part.get("format") not in (FORMAT, RAW_FORMAT) or part.get("state") not in PART_STATES:
            problems.append(f"{name}: wrong format or state")
        artifacts = part.get("artifacts", [])
        if part.get("format") == RAW_FORMAT and len(artifacts) > 1:
            problems.append(f"{name}: a sysext image part is exactly one artifact")
        if (part.get("state") == "available") != bool(artifacts):
            problems.append(f"{name}: only an available part has artifacts")
        if part.get("state") != "available" and not part.get("message"):
            problems.append(f"{name}: a waiting or unsupported part needs a message")
        for artifact in artifacts:
            package = artifact.get("package", "")
            if not NVRA_SAFE.fullmatch(package):
                problems.append(f"{name}: invalid package {package!r}")
            urls = artifact.get("urls", [])
            if not urls or not all(isinstance(u, str) and u.startswith("https://") for u in urls):
                problems.append(f"{name}: {package} needs https URLs")
            if not isinstance(artifact.get("size"), int) or artifact["size"] <= 0:
                problems.append(f"{name}: {package} has no size")
            if not HEX64.fullmatch(str(artifact.get("sha256", ""))):
                problems.append(f"{name}: {package} has no sha256")
    return problems


# ---------------------------------------------------------------------------
# The image
# ---------------------------------------------------------------------------

def run(argv: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(argv, capture_output=True, text=True, check=False)
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).strip().splitlines()[-8:]
        raise ResolveError(f"{argv[0]} failed: {' '.join(detail)[:800]}")
    return result


def parse_os_release(text: str) -> dict[str, str]:
    values = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        values[key.strip()] = value.strip().strip("\"'")
    return values


def probe_base(image: str, os_id: str) -> Base:
    release = parse_os_release(Path("/usr/lib/os-release").read_text())
    found_id, version_id = release.get("ID", ""), release.get("VERSION_ID", "")
    if found_id != os_id or not version_id.isdigit():
        raise ResolveError(f"the recipe needs {os_id}, the image is {found_id} {version_id}")
    arch = os.uname().machine
    if arch not in ARCHITECTURES:
        raise ResolveError(f"unsupported architecture {arch}")
    lines = base_package_lines(run(["rpm", "-qa", "--qf", RPM_FINGERPRINT_QUERY]).stdout)
    if not lines or any(not re.fullmatch(r"\S+ [0-9]+:\S+\n", line) for line in lines):
        raise ResolveError("rpm returned an empty or malformed base package fingerprint")
    fingerprint = base_packages_sha256(lines)
    output = run(["rpm", "-qa", "--qf", "%{NAME} %{EPOCHNUM} %{VERSION} %{RELEASE} %{ARCH} base\n"]).stdout
    installed = {}
    for line in output.splitlines():
        nevra = parse_nevra_line(line)
        if nevra is not None and nevra.name in FINGERPRINT_EXCLUDED:
            continue
        if nevra is None or not line.split()[1].isdigit():
            raise ResolveError(f"rpm returned a malformed installed package: {line!r}")
        installed[nevra.name] = nevra
    if not installed:
        raise ResolveError("rpm returned no installed base packages")
    if {line.split()[0] for line in lines} != set(installed):
        raise ResolveError("rpm base package queries disagree")
    return Base(os_id, version_id, arch, installed, fingerprint, image)


class Dnf:
    """dnf5 against the recipe's own repository definitions, never the image's."""

    QUERY = "%{name} %{epoch} %{version} %{release} %{arch} %{repoid} %{location}\n"

    def __init__(self, recipe: Recipe, base: Base, workdir: Path):
        self.base = base
        self.recipe = recipe
        reposdir = workdir / "repos.d"
        reposdir.mkdir(parents=True, exist_ok=True)
        text = recipe.repo_file.read_text().replace("@KEYS@", str((recipe.directory / "keys").resolve()))
        (reposdir / recipe.repo_file.name).write_text(text)
        self.repos = repo_ids(text)
        cache = workdir / "dnf"
        self.prefix = [
            "dnf5", "--assumeyes", "--quiet", "--no-plugins",
            f"--releasever={base.version_id}",
            f"--setopt=reposdir={reposdir}",
            f"--setopt=cachedir={cache / 'cache'}",
            f"--setopt=system_cachedir={cache / 'cache'}",
            f"--setopt=logdir={cache / 'log'}",
            f"--setopt=system_state_dir={cache / 'state'}",
            "--setopt=install_weak_deps=False",
            "--setopt=gpgcheck=True",
        ]

    def base_excludes(self) -> list[str]:
        """Nothing the base installed may be replaced, only depended on."""
        names = ",".join(sorted(self.base.installed))
        return [f"--setopt={repo}.excludepkgs={names}" for repo in self.repos] if names else []

    def candidates(self, names: list[str]) -> dict[str, list[Nevra]]:
        found: dict[str, list[Nevra]] = {name: [] for name in names}
        if not names:
            return found
        output = run(self.prefix + ["repoquery", "--available", "--queryformat", self.QUERY, *names]).stdout
        for line in output.splitlines():
            nevra = parse_nevra_line(line)
            if nevra and nevra.name in found and nevra.arch in (self.base.arch, "noarch"):
                found[nevra.name].append(nevra)
        return found

    def download(self, specs: list[str], destdir: Path) -> list[Path]:
        destdir.mkdir(parents=True, exist_ok=True)
        run(self.prefix + self.base_excludes()
            + ["download", "--resolve", f"--arch={self.base.arch}", "--arch=noarch",
               f"--destdir={destdir}", *specs])
        return sorted(destdir.glob("*.rpm"))


def rpm_header(path: Path) -> dict[str, str]:
    output = run(["rpm", "-qp", "--nosignature", "--nodigest", "--qf",
                  "%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{LICENSE}\n", str(path)]).stdout
    name, epoch, version, release, arch, licence = output.rstrip("\n").split("\t")
    return {"name": name, "epoch": epoch, "version": version, "release": release, "arch": arch,
            "license": licence, "nvra": f"{name}-{version}-{release}.{arch}"}


class Keyring:
    """An isolated rpm keyring holding only the recipe's pinned keys for this release."""

    def __init__(self, recipe: Recipe, base: Base, workdir: Path):
        self.recipe = recipe
        self.version_id = base.version_id
        self.defines = [
            "--define", "_keyring fs",
            "--define", f"_keyringpath {workdir / 'keys'}",
            "--define", f"_dbpath {workdir / 'db'}",
            "--define", "_pkgverify_level signature",
        ]
        used = {repo.key for repo in recipe.repos.values()}
        for name in sorted(used):
            key = recipe.keys[name]
            pinned = key.pinned(base.version_id)
            if pinned is None:
                raise ResolveError(f"no pinned {name} key for {base.os_id} {base.version_id}")
            path = key.path(recipe.directory, base.version_id)
            actual = openpgp_fingerprint(path.read_text())
            if actual != pinned:
                raise ResolveError(f"{path} has fingerprint {actual}, expected {pinned}")
            run(["rpmkeys", *self.defines, "--import", str(path)])

    def verify(self, path: Path, repos: Iterable[str]) -> str:
        result = run(["rpmkeys", *self.defines, "-Kv", str(path)], check=False)
        ok, fingerprints = parse_checksig(result.stdout)
        if result.returncode != 0 or not ok:
            raise ResolveError(f"{path.name}: signature check failed")
        for repo in repos:
            allowed = self.recipe.fingerprints_for(repo, self.version_id)
            if not fingerprints or not fingerprints <= allowed:
                raise ResolveError(f"{path.name} from {repo or 'an unknown repository'} "
                                   "is not signed by that repository's key")
        return sorted(fingerprints)[0]


class HttpsOnly(urllib.request.HTTPRedirectHandler):
    """Follow redirects only to HTTPS: the device must never need plain HTTP."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise urllib.error.HTTPError(newurl, code, f"redirect to non-HTTPS {newurl}", headers, fp)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def fetch_digest(url: str) -> tuple[int, str, str]:
    """(size, sha256, final URL) of what `url` serves over HTTPS."""
    opener = urllib.request.build_opener(HttpsOnly())
    request = urllib.request.Request(url, headers={"User-Agent": "tilo-extension-resolve/1"})
    digest = hashlib.sha256()
    size = 0
    with opener.open(request, timeout=120) as response:
        final = response.geturl()
        while chunk := response.read(1 << 20):
            digest.update(chunk)
            size += len(chunk)
    return size, digest.hexdigest(), final


def live_urls(urls: list[str], size: int, sha256: str,
              fetch: Callable[[str], tuple[int, str, str]] = fetch_digest, log=print) -> list[str]:
    """The URLs that serve exactly these bytes over HTTPS, each tried up to three times."""
    good = []
    for url in urls:
        reason = ""
        for _attempt in range(3):
            try:
                got_size, got_sha, final = fetch(url)
            except (urllib.error.URLError, OSError, ValueError, http.client.HTTPException) as error:
                reason = str(error)
                continue
            if (got_size, got_sha) == (size, sha256):
                good.append(url)
                if final != url:
                    log(f"  {url} -> {final}")
                reason = ""
                break
            reason = f"served {got_size} bytes with sha256 {got_sha}"
        if reason:
            log(f"  dropping {url}: {reason}")
    return good


def resolve_part_artifacts(recipe: Recipe, base: Base, dnf, keyring, specs: list[str], workdir: Path,
                           fetch: Callable[[str], tuple[int, str, str]] = fetch_digest, log=print) -> list[dict]:
    """Download, verify and pin one part's packages and the dependencies the base lacks."""
    rpms = dnf.download(specs, workdir)
    if not rpms:
        raise ResolveError(f"downloading {' '.join(specs)} produced nothing")
    headers = {path: rpm_header(path) for path in rpms}
    downloaded = {
        Nevra(h["name"], int(h["epoch"]), h["version"], h["release"], h["arch"]).spec
        for h in headers.values()
    }
    missing = set(specs) - downloaded
    if missing:
        raise ResolveError(f"download omitted selected packages: {', '.join(sorted(missing))}")
    offered = dnf.candidates(sorted({h["name"] for h in headers.values()}))
    artifacts = []
    for path, header in headers.items():
        if header["name"] in base.installed:
            raise ResolveError(f"{header['nvra']} would replace a base package")
        offers = [n for n in offered.get(header["name"], []) if n.nvra == header["nvra"] and n.epoch == int(header["epoch"])]
        repos = sorted({n.repo for n in offers})
        if not repos or not set(repos) <= set(recipe.repos):
            raise ResolveError(f"{header['nvra']} comes from no repository of the recipe ({repos})")
        keyring.verify(path, repos)
        data = path.read_bytes()
        size, sha256 = len(data), hashlib.sha256(data).hexdigest()
        candidates = artifact_urls(recipe, offers, base.version_id, base.arch)
        log(f"{header['nvra']} ({size} bytes, {sha256}) from {', '.join(repos)}")
        urls = live_urls(candidates, size, sha256, fetch, log)
        if not urls:
            raise ResolveError(f"{header['nvra']}: no upstream URL serves the verified bytes")
        artifacts.append({
            "package": header["nvra"],
            "urls": urls,
            "size": size,
            "sha256": sha256,
            "publisher": recipe.repos[repos[0]].publisher,
            "license": header["license"],
        })
    return artifacts


def resolve(recipe: Recipe, base: Base, dnf, keyring, workdir: Path, when: datetime.datetime,
            fetch: Callable[[str], tuple[int, str, str]] = fetch_digest, log=print) -> dict:
    candidates = dnf.candidates(sorted({c.name for c in recipe.components if c.applies_to(base.arch)}))
    parts = []
    for part in recipe.parts:
        decision = decide_part(part, recipe.components, base, candidates)
        log(f"{part.name}: {decision.state} {' '.join(n.spec for n in decision.chosen)} {decision.message}".rstrip())
        artifacts = []
        if decision.state == "available":
            artifacts = resolve_part_artifacts(recipe, base, dnf, keyring, [n.spec for n in decision.chosen],
                                               workdir / "rpms" / part.name, fetch, log)
        parts.append(part_document(part.name, decision, artifacts))
    manifest = manifest_document(recipe, base, parts, when)
    problems = check_manifest(manifest)
    if problems:
        raise ResolveError("; ".join(problems))
    return manifest


# ---------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------

def command_resolve(args) -> int:
    if not IMAGE_REF.fullmatch(args.image):
        raise ResolveError("--image must be a ref@sha256:<64 hex> digest reference")
    recipe = load_recipe(Path(args.recipe))
    base = probe_base(args.image, recipe.os_id)
    print(f"base {base.os_id} {base.version_id} {base.arch} {base.packages_sha256} ({base.image})")
    workdir = Path(tempfile.mkdtemp(prefix="tilo-extension-resolve."))
    try:
        dnf = Dnf(recipe, base, workdir)
        keyring = Keyring(recipe, base, workdir / "keyring")
        manifest = resolve(recipe, base, dnf, keyring, workdir, datetime.datetime.now(datetime.timezone.utc))
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(render(manifest))
    print(f"wrote {out}: revision {manifest['revision']}")
    return 0


def command_fingerprint(_args) -> int:
    print(base_packages_sha256(base_package_lines(run(["rpm", "-qa", "--qf", RPM_FINGERPRINT_QUERY]).stdout)))
    return 0


def command_check(args) -> int:
    manifest = json.loads(Path(args.manifest).read_bytes())
    problems = check_manifest(manifest)
    if problems:
        for problem in problems:
            print(f"{args.manifest}: {problem}", file=sys.stderr)
        return 1
    expected = getattr(args, "expected_content_sha256", None)
    if expected and (not HEX64.fullmatch(expected) or qualification_digest(manifest) != expected):
        print("resolved manifest differs from the reviewed qualification content; refusing to sign or publish",
              file=sys.stderr)
        return 1
    base = manifest["base"]
    print(manifest["id"], base["architecture"], base["packages_sha256"], manifest["revision"])
    return 0


def command_same(args) -> int:
    old = json.loads(Path(args.old).read_bytes())
    new = json.loads(Path(args.new).read_bytes())
    return 0 if same_publication(old, new) else 1


def command_content_sha256(args) -> int:
    manifest = json.loads(Path(args.manifest).read_bytes())
    if check_manifest(manifest):
        raise ResolveError("cannot hash an invalid manifest for qualification")
    print(qualification_digest(manifest))
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    resolve_cmd = commands.add_parser("resolve")
    resolve_cmd.add_argument("--recipe", required=True)
    resolve_cmd.add_argument("--image", required=True)
    resolve_cmd.add_argument("--out", required=True)
    resolve_cmd.set_defaults(handler=command_resolve)
    commands.add_parser("fingerprint").set_defaults(handler=command_fingerprint)
    check_cmd = commands.add_parser("check")
    check_cmd.add_argument("manifest")
    check_cmd.add_argument("--expected-content-sha256")
    check_cmd.set_defaults(handler=command_check)
    same_cmd = commands.add_parser("same")
    same_cmd.add_argument("old")
    same_cmd.add_argument("new")
    same_cmd.set_defaults(handler=command_same)
    digest_cmd = commands.add_parser("content-sha256")
    digest_cmd.add_argument("manifest")
    digest_cmd.set_defaults(handler=command_content_sha256)
    args = parser.parse_args(argv)
    try:
        return args.handler(args)
    except (ResolveError, OSError, ValueError, KeyError) as error:
        print(f"resolve.py: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
