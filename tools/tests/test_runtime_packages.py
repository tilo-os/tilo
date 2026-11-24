#!/usr/bin/env python3
"""The language runtime packages' pinned releases, and the tokens apps name.

tools/apps/runtime_packages.sh builds `so.tilo.runtime.bun` and
`so.tilo.runtime.python` from upstream releases it downloads and verifies.
Its table, the pins in os/external-assets/github-assets.tsv and every app
manifest that names one of those runtimes have to agree, or a release
downloads nothing it can verify or ships a package no app can install.
"""
from __future__ import annotations

from pathlib import Path
import re
import subprocess
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/apps/runtime_packages.sh"
ASSETS = ROOT / "os/external-assets/github-assets.tsv"
SEMVER = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")


def call(*words: str) -> str:
    """What the script's function `words` answers, sourcing the script as its callers do."""
    script = ". tools/apps/runtime_packages.sh\n" + "\n".join(words)
    done = subprocess.run(
        ["bash", "-c", script], cwd=ROOT, text=True, capture_output=True, check=False
    )
    if done.returncode != 0:
        raise AssertionError(f"{words}: {done.stderr}")
    return done.stdout


def pinned_assets() -> dict[str, tuple[str, str]]:
    assets = {}
    for line in ASSETS.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        digest, name, url = line.split("\t")
        assets[name] = (digest, url)
    return assets


def naming_runtimes() -> list[tuple[Path, list[str]]]:
    """Every app manifest and the runtimes its `[package]` names."""
    found = []
    for manifest in sorted(list((ROOT / "apps").glob("*/tilo-app.toml"))
                           + list((ROOT / "apps/tl").glob("*/app.toml"))):
        package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        named = list(package.get("optional-runtimes", []))
        runtime = manifest.parent / "app.toml"
        if runtime != manifest and runtime.is_file():
            named += tomllib.loads(runtime.read_text(encoding="utf-8")).get("package", {}).get(
                "optional-runtimes", []
            )
        if named:
            found.append((manifest, named))
    return found


class RuntimePackageTests(unittest.TestCase):
    def test_every_runtime_is_pinned_for_every_architecture_its_package_ships(self):
        assets = pinned_assets()
        for name in call("runtime_package_versions").split():
            with self.subTest(runtime=name):
                release = call(f"runtime_package_release {name}").strip()
                self.assertRegex(release, SEMVER)
                for arch in ("x86_64", "aarch64"):
                    asset = call(f"runtime_package_asset {name} {arch}").strip()
                    self.assertIn(asset, assets, f"{name} {arch} names an unpinned asset")
                    digest, url = assets[asset]
                    self.assertRegex(digest, r"^[0-9a-f]{64}$")
                    self.assertIn(release, url, "the asset is not this release's")

    def test_provides_names_the_runtime_its_release_and_a_semver_an_app_may_name(self):
        for name in call("runtime_package_versions").split():
            with self.subTest(runtime=name):
                release = call(f"runtime_package_release {name}").strip()
                provides = call(f"runtime_package_provides {name}").strip()
                self.assertEqual(provides, f"{name}/{release}+{name}")
                version = provides.split("/", 1)[1].split("+", 1)[0]
                self.assertRegex(version, SEMVER)

    def test_every_app_naming_one_of_these_runtimes_names_what_is_built(self):
        built = {
            name: call(f"runtime_package_provides {name}").strip()
            for name in call("runtime_package_versions").split()
        }
        for manifest, named in naming_runtimes():
            for runtime in named:
                name = runtime.split("/", 1)[0]
                if name not in built:
                    continue
                with self.subTest(manifest=str(manifest.relative_to(ROOT)), runtime=runtime):
                    self.assertEqual(
                        runtime,
                        built[name],
                        "the app names a release no runtime package provides",
                    )


if __name__ == "__main__":
    unittest.main()
