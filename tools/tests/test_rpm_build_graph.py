"""Verify the real RPM driver preserves delivered targets and sandbox features."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SUPPORT = {
    "tilo-fs",
    "launcher",
    "tilo-unlock-helper",
    "tilo-app-package-cli",
    "install",
    "runtime-bundle",
    "cli-shim",
    "app-host",
    "app-cli",
    "ush",
    "local-model-service",
    "tilo-model-gateway",
}


class RpmBuildGraphTest(unittest.TestCase):
    def test_actual_support_targets_and_isolated_compositor(self):
        with tempfile.TemporaryDirectory(
            prefix=".rpm-build-graph-", dir=ROOT
        ) as temporary:
            root = Path(temporary)
            (root / "dist/rpm").mkdir(parents=True)
            (root / "bin").mkdir()
            shutil.copyfile(
                ROOT / "dist/rpm/build-rpm.sh", root / "dist/rpm/build-rpm.sh"
            )
            shutil.copyfile(ROOT / "dist/VERSION", root / "dist/VERSION")
            (root / "dist/rpm/test-rpmlint-config.py").write_text("")
            shim = root / "dist/rpm/rpmbuild.sh"
            shim.write_text("#!/bin/sh\nexit 0\n")
            shim.chmod(0o755)
            cargo = root / "bin/cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\nimport json,os,sys\np=os.environ['CALLS']\nn=sum(1 for _ in open(p)) if os.path.exists(p) else 0\nwith open(p,'a') as f:f.write(json.dumps(sys.argv[1:])+'\\n')\nsys.exit(47 if n else 0)\n"
            )
            cargo.chmod(0o755)
            log = root / "calls"
            result = subprocess.run(
                ["bash", str(root / "dist/rpm/build-rpm.sh")],
                env=os.environ
                | {"PATH": f"{root / 'bin'}:{os.environ['PATH']}", "CALLS": str(log)},
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 47, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        support, compositor = calls
        packages = {
            support[i + 1] for i, value in enumerate(support[:-1]) if value == "-p"
        }
        bins = {
            support[i + 1] for i, value in enumerate(support[:-1]) if value == "--bin"
        }
        expected = {
            "imager",
            "fs",
            "tilo-relay-admin",
            "tilo-enroll",
            "tilo-relay-environment",
            "tilo-relay-offers",
            "tilo-legacy",
        }
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        seen = set()
        for pattern in workspace["members"]:
            for directory in ROOT.glob(pattern):
                manifest = tomllib.loads((directory / "Cargo.toml").read_text())
                package = manifest.get("package", {})
                if package.get("name") not in SUPPORT:
                    continue
                seen.add(package["name"])
                features = manifest.get("features", {})
                active = set(features.get("default", []))
                pending = list(active)
                while pending:
                    for feature in features.get(pending.pop(), []):
                        if feature not in active:
                            active.add(feature)
                            pending.append(feature)
                for target in manifest.get("bin", []):
                    if set(target.get("required-features", [])).issubset(active):
                        expected.add(target["name"])
                if package.get("autobins", True):
                    explicit = {target["name"] for target in manifest.get("bin", [])}
                    paths = {
                        str(directory / target["path"])
                        for target in manifest.get("bin", [])
                        if "path" in target
                    }
                    if (
                        (directory / "src/main.rs").is_file()
                        and package["name"] not in explicit
                        and str(directory / "src/main.rs") not in paths
                    ):
                        expected.add(package["name"])
                    for source in (directory / "src/bin").glob("*.rs"):
                        if source.stem not in explicit and str(source) not in paths:
                            expected.add(source.stem)
                    for source in (directory / "src/bin").glob("*/main.rs"):
                        if (
                            source.parent.name not in explicit
                            and str(source) not in paths
                        ):
                            expected.add(source.parent.name)
        self.assertEqual(seen, SUPPORT)
        self.assertEqual(packages, SUPPORT | {"imager", "tilo-relay", "legacy-vm"})
        self.assertEqual(bins, expected)
        self.assertEqual(support[:4], ["build", "--release", "--locked", "--lib"])
        self.assertEqual(
            compositor,
            [
                "build",
                "--release",
                "--locked",
                "-p",
                "shell",
                "--bin",
                "shell",
                "--bin",
                "tilo-runtime-mcp",
                "--no-default-features",
            ],
        )


if __name__ == "__main__":
    unittest.main()
