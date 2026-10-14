#!/usr/bin/env python3
"""Validate continuous Installer publication without inventing stable versions."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import tomllib
from urllib.parse import urlsplit
from urllib.request import urlopen


def protection(repository, branch, environ):
    """Check Forgejo metadata; this is preflight, never a signing credential."""
    repository = json.loads(Path(repository).read_text())
    branch = json.loads(Path(branch).read_text())
    if not isinstance(repository, dict) or not isinstance(branch, dict):
        raise ValueError("protection metadata must be JSON objects")
    expected_name = environ.get("GITHUB_REPOSITORY", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", expected_name):
        raise ValueError("missing source repository name")
    expected_id = environ.get("GITHUB_REPOSITORY_ID", "")
    if not re.fullmatch(r"[1-9][0-9]*", expected_id):
        raise ValueError("missing immutable source repository identity")
    if (type(repository.get("id")) is not int or repository["id"] != int(expected_id)
            or repository.get("full_name") != expected_name
            or repository.get("default_branch") != "main"):
        raise ValueError("protection metadata belongs to a different source repository")
    if branch.get("name") != "main" or branch.get("protected") is not True:
        raise ValueError("continuous publication requires protected main")
    return "TILO_REF_PROTECTED=true"


def identity(environ, manifest):
    if environ.get("GITHUB_EVENT_NAME") != "push" or environ.get("GITHUB_REF") != "refs/heads/main":
        raise ValueError("continuous publication requires a main push")
    if environ.get("TILO_REF_PROTECTED") != "true":
        raise ValueError("continuous publication requires protected main")
    source = environ.get("GITHUB_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("continuous publication requires the exact source revision")
    version = tomllib.loads(Path(manifest).read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("native package version must be the source's numeric version")
    return version, f"imager-main-{source}"


def assets(root):
    root = Path(root)
    names = []
    for arch in ("x86_64", "aarch64"):
        names.extend(f"tilo-imager-linux-{arch}.{suffix}" for suffix in ("AppImage", "tar.gz", "flatpak"))
        names.extend(f"tilo-imager-windows-{arch}.{suffix}" for suffix in ("exe", "zip"))
        names.append(f"tilo-imager-setup-windows-{arch}.exe")
        names.extend(f"tilo-imager-macos-{arch}.{suffix}" for suffix in ("zip", "dmg"))
    for name in names:
        matches = list(root.rglob(name))
        if len(matches) != 1 or matches[0].is_symlink() or not matches[0].is_file() or matches[0].stat().st_size == 0:
            raise ValueError(f"missing, empty or duplicate native package: {name}")
        path = matches[0]
        checksum = path.with_name(name + ".sha256")
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if checksum.is_symlink() or not checksum.is_file() or checksum.read_text().strip() not in (f"{digest}  {name}", f"{digest} *{name}"):
            raise ValueError(f"invalid package digest: {name}")
        if "-linux-" in name:
            signature = path.with_name(name + ".asc")
            if signature.is_symlink() or not signature.is_file() or signature.stat().st_size == 0:
                raise ValueError(f"missing Linux signature: {name}")
    return names


def catalog(source, output):
    """Reuse published media, checking the reviewed catalog and exact bytes."""
    source = Path(source)
    document = json.loads(source.read_text())
    tag = document.get("tag", "")
    if not re.fullmatch(r"imager-v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("continuous publication requires a reviewed published installer catalog")
    repository = os.environ.get("TILO_RELEASE_REPOSITORY", "tilo-os/releases")
    server = os.environ.get("TILO_RELEASE_SERVER", "https://github.com")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid release repository")
    origin = urlsplit(server)
    if origin.scheme != "https" or not origin.hostname or origin.username or origin.password or origin.path not in ("", "/") or origin.query or origin.fragment:
        raise ValueError("invalid public release origin")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    schema = document.get("schema")
    if type(schema) is not int or schema not in (1, 2):
        raise ValueError("unsupported installer catalog schema")
    name = f"tilo-installer-catalog-v{schema}.json"
    base = f"{server.rstrip('/')}/{repository}/releases/download/{tag}/"
    # Construct destinations ourselves, never follow a caller-supplied media URL.
    # The production verifier below then checks every reviewed URL and digest.
    names = [name]
    for entry in document.get("releases", []):
        if entry.get("purpose") == "tilo-internal-installer":
            continue
        arch = entry.get("arch")
        profile = entry.get("hardware_profile", "generic")
        if arch not in ("x86_64", "aarch64") or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", profile):
            raise ValueError("invalid installer target")
        suffix = "" if profile == "generic" else f"-{profile}"
        names.append(f"tilo-installer-linux-{arch}{suffix}.img.zst")
    for asset in names:
        for suffix in ("", ".sha256", ".asc"):
            with urlopen(base + asset + suffix, timeout=120) as remote, (output / (asset + suffix)).open("wb") as destination:
                shutil.copyfileobj(remote, destination)
    if (output / name).read_bytes() != source.read_bytes():
        raise ValueError("published installer catalog differs from the reviewed source catalog")
    spec = importlib.util.spec_from_file_location("installer_catalog", Path(__file__).with_name("release-catalog.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    _, selected = module.select(output)
    module.media(selected, output)
    # The existing Linux verifier authenticates this catalog and its media with
    # the independently pinned OpenPGP identity before any public promotion.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("identity", "assets", "catalog", "protection"))
    parser.add_argument("path")
    parser.add_argument("--output")
    parser.add_argument("--branch-metadata")
    args = parser.parse_args()
    if args.operation == "protection":
        if not args.branch_metadata:
            parser.error("protection requires --branch-metadata")
        print(protection(args.path, args.branch_metadata, os.environ))
    elif args.operation == "identity":
        version, tag = identity(os.environ, args.path)
        print(f"version={version}\nrelease-tag={tag}")
    elif args.operation == "catalog":
        if not args.output:
            parser.error("catalog requires --output")
        catalog(args.path, args.output)
    else:
        print(json.dumps(assets(args.path)))


if __name__ == "__main__":
    main()
