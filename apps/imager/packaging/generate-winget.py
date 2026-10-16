#!/usr/bin/env python3
"""Generate one WinGet manifest set from both validated Windows installers."""
import argparse
import hashlib
import os
from pathlib import Path
import re
from urllib.parse import urlparse


def manifests(root, version, base_url):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("WinGet requires a stable semantic version")
    repository = os.environ.get("TILO_RELEASE_REPOSITORY", "tilo-os/releases")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("release repository must be OWNER/REPOSITORY")
    server = urlparse(os.environ.get("TILO_RELEASE_SERVER", "https://github.com"))
    if server.scheme != "https" or not server.netloc or server.path not in ("", "/") or server.query or server.fragment:
        raise ValueError("release server must be an HTTPS origin")
    parsed = urlparse(base_url)
    if (parsed.scheme != "https" or parsed.netloc != server.netloc
            or parsed.path != f"/{repository}/releases/download/imager-v{version}"
            or parsed.query or parsed.fragment):
        raise ValueError("release URL does not match the selected version")
    entries = []
    for arch, winget_arch in (("x86_64", "x64"), ("aarch64", "arm64")):
        name = f"tilo-imager-setup-windows-{arch}.exe"
        candidates = list(root.rglob(name))
        if len(candidates) != 1 or not candidates[0].is_file() or candidates[0].is_symlink():
            raise ValueError(f"expected exactly one regular {arch} setup executable")
        installer = candidates[0]
        if installer.stat().st_size == 0:
            raise ValueError(f"empty {arch} setup executable")
        with installer.open("rb") as stream:
            checksum = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                checksum.update(chunk)
            digest = checksum.hexdigest()
        sidecar = installer.with_suffix(".exe.sha256")
        if sidecar.is_symlink() or sidecar.read_text().split() != [digest, name]:
            raise ValueError(f"invalid {arch} setup digest")
        entries.append(f"  - Architecture: {winget_arch}\n"
                       f"    InstallerUrl: {base_url}/{name}\n"
                       f"    InstallerSha256: {digest.upper()}\n")
    prefix = f"PackageIdentifier: Tilo.Imager\nPackageVersion: {version}\n"
    return {
        "Tilo.Imager.installer.yaml": prefix + "InstallerType: inno\nInstallers:\n"
        + "".join(entries) + "ManifestType: installer\nManifestVersion: 1.9.0\n",
        "Tilo.Imager.yaml": prefix + "DefaultLocale: en-US\nManifestType: version\nManifestVersion: 1.9.0\n",
        "Tilo.Imager.locale.en-US.yaml": prefix + "PackageLocale: en-US\nPublisher: Tilo Project\n"
        "PackageName: Tilo Installer\nLicense: AGPL-3.0-only\n"
        "ShortDescription: Create and verify customised Tilo installation media.\n"
        f"PackageUrl: {server.scheme}://{server.netloc}/{repository}\n"
        "ManifestType: defaultLocale\nManifestVersion: 1.9.0\n",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--base-url", required=True)
    args = parser.parse_args()
    try:
        documents = manifests(args.root, args.version, args.base_url)
        output = args.root / "winget"
        output.mkdir(exist_ok=True)
        for name, content in documents.items():
            (output / name).write_text(content, encoding="utf-8")
    except (OSError, ValueError) as error:
        parser.exit(1, f"WinGet manifest generation failed: {error}\n")


if __name__ == "__main__":
    main()
