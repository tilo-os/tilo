#!/usr/bin/env python3
"""The SDKs' publish script, run as a release tag's run would run it.

tools/ci/publish/sdks.sh builds its two packages with bun and uv, and publishes them to npm and
PyPI. This runs the script itself, with stand-ins for the two tools in front of it: they build and
leave the artifacts the script reads back, and record what a publish would have sent instead of
sending anything. A check that reaches no registry is what this covers: the version a tag writes
into both manifests, the artifacts and the module required of them, and how a publish refuses
anything but a release tag or a run without the registries' tokens.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/ci/publish/sdks.sh"

# Stands in for bun: installs nothing, builds the package as tsc would, and packs and publishes
# from the module's version, as bun does. Every call is recorded in FAKE_LOG.
FAKE_BUN = r'''#!/usr/bin/env python3
import json, os, pathlib, sys

log = pathlib.Path(os.environ["FAKE_LOG"])
args = [a for a in sys.argv[1:] if not a.startswith("--")]
metadata = json.loads(pathlib.Path("package.json").read_text())
name, version = metadata["name"], metadata["version"]
with log.open("a") as f:
    f.write("bun " + " ".join(sys.argv[1:]) + "\n")
if args[0] == "install":
    pathlib.Path("node_modules").mkdir(exist_ok=True)
elif args[:2] == ["run", "build"]:
    for entry in ("dist/index.js", "dist/index.d.ts", "dist/types/index.js"):
        if os.environ.get("FAKE_TS_DIST") == "missing" and entry == "dist/types/index.js":
            continue
        path = pathlib.Path(entry)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"export const version = {version!r};\n")
elif args[0] == "pm" and args[1] == "pack":
    print(f"{name}-{version}.tgz")
elif args[0] == "publish":
    if not os.environ.get("NPM_CONFIG_TOKEN"):
        sys.exit("bun publish: no token")
    with log.open("a") as f:
        f.write(f"published npm {name}@{version} {sys.argv[sys.argv.index('--tag') + 1]}\n")
'''

# Stands in for uv: builds the sdist and a wheel carrying the metadata PyPI indexes and the
# module the package installs, and publishes nothing. FAKE_WHEEL_* break the wheel on purpose.
FAKE_UV = r'''#!/usr/bin/env python3
import os, pathlib, sys, tarfile, zipfile

log = pathlib.Path(os.environ["FAKE_LOG"])
args = [a for a in sys.argv[1:] if not a.startswith("--")]
with log.open("a") as f:
    f.write("uv " + " ".join(sys.argv[1:]) + "\n")
if args[0] == "build":
    version = next(line.split("=")[1].strip().strip('"')
                   for line in pathlib.Path("pyproject.toml").read_text().splitlines()
                   if line.startswith("version = "))
    name = "tilo-sdk"
    dist = pathlib.Path("dist")
    dist.mkdir(exist_ok=True)
    plain = version.replace("-", "").replace(".", "")
    wheel = dist / f"tilo_sdk-{version}-py3-none-any.whl"
    metadata = os.environ.get("FAKE_WHEEL_VERSION", version)
    module = os.environ.get("FAKE_WHEEL_MODULE", "tilo/__init__.py")
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(f"tilo_sdk-{version}.dist-info/METADATA",
                         f"Name: {name}\nVersion: {metadata}\n")
        if module:
            archive.writestr(module, "")
    with tarfile.open(dist / f"tilo_sdk-{version}.tar.gz", "w:gz"):
        pass
elif args[0] == "publish":
    if "--dry-run" not in sys.argv and not os.environ.get("UV_PUBLISH_TOKEN"):
        sys.exit("uv publish: no token")
    if "--dry-run" in sys.argv:
        sys.exit(0)
    with log.open("a") as f:
        f.write("published pypi " + " ".join(sorted(p.name for p in pathlib.Path("dist").iterdir())) + "\n")
'''

# The three calls a publish makes, by the version each was given: what a run sent, or would have.
SENT = ("published npm", "published pypi")


class PublishSdksTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="tilo-publish-sdks-")
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.log = self.dir / "calls"
        self.log.write_text("")
        self.bin = self.dir / "bin"
        self.bin.mkdir()
        for name, body in (("bun", FAKE_BUN), ("uv", FAKE_UV)):
            path = self.bin / name
            path.write_text(body)
            path.chmod(0o755)

    def run_script(self, mode: str, ref: str | None, env: dict[str, str] | None = None, **knobs):
        """The script run as a job would run it: `ref` is T_REF, unset for a working tree."""
        environment = {
            key: value for key, value in os.environ.items()
            if key not in ("T_REF", "NPM_TOKEN", "PYPI_TOKEN", "UV_PUBLISH_TOKEN", "NPM_CONFIG_TOKEN")
        }
        environment.update({
            "PATH": f"{self.bin}:{environment['PATH']}",
            "TMPDIR": str(self.dir / "scratch"),
            "FAKE_LOG": str(self.log),
        })
        (self.dir / "scratch").mkdir(exist_ok=True)
        if ref:
            environment["T_REF"] = ref
        environment.update(env or {})
        environment.update({key: str(value) for key, value in knobs.items()})
        return subprocess.run(
            ["bash", str(SCRIPT), mode], cwd=ROOT, env=environment, capture_output=True, text=True,
        )

    def calls(self) -> list[str]:
        return [line for line in self.log.read_text().splitlines() if line]

    def sent(self) -> list[str]:
        return [line for line in self.calls() if line.startswith(SENT)]

    def test_a_tag_builds_both_packages_at_its_version_and_publishes_nothing(self):
        result = self.run_script("check", "tag:v0.4.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tilo-sdk 0.4.0 is ready to publish", result.stdout)
        # Both manifests carried the tag's version when the build read them, and the publish
        # after it was the dry run's (uv's, which reaches no registry here).
        self.assertIn("bun run build", self.calls())
        self.assertTrue(
            any(call.startswith("uv publish --dry-run --trusted-publishing never dist/tilo_sdk-0.4.0-")
                for call in self.calls()),
            self.calls(),
        )
        self.assertEqual(self.sent(), [])

    def test_a_working_tree_checks_a_stand_in_version(self):
        # No T_REF: main's run, which must exercise the same build without standing for a release.
        result = self.run_script("check", None)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tilo-sdk 0.0.0 is ready to publish", result.stdout)

    def test_a_pre_release_is_checked_as_a_pre_release(self):
        result = self.run_script("check", "tag:v0.4.0-rc.1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tilo-sdk 0.4.0-rc.1 is ready to publish", result.stdout)

    def test_a_wheel_that_names_another_version_fails_the_check(self):
        result = self.run_script("check", "tag:v0.4.0", FAKE_WHEEL_VERSION="0.3.9")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not tilo-sdk@0.4.0", result.stderr)

    def test_a_wheel_without_the_module_fails_the_check(self):
        result = self.run_script("check", "tag:v0.4.0", FAKE_WHEEL_MODULE="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("carries no tilo module", result.stderr)

    def test_a_typescript_build_without_its_entry_points_fails_the_check(self):
        result = self.run_script("check", "tag:v0.4.0", FAKE_TS_DIST="missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the build left no", result.stderr)

    def test_a_publish_refuses_a_ref_that_is_no_release_tag(self):
        for ref in ("main", "feature"):
            result = self.run_script("publish", ref, NPM_TOKEN="npm_test", PYPI_TOKEN="pypi-test")
            self.assertNotEqual(result.returncode, 0, ref)
            self.assertIn("released from a release tag", result.stderr)
        self.assertEqual(self.sent(), [])

    def test_a_publish_refuses_a_run_without_the_registries_tokens(self):
        for env in ({}, {"NPM_TOKEN": "npm_test"}, {"PYPI_TOKEN": "pypi-test"}):
            result = self.run_script("publish", "tag:v0.4.0", env)
            self.assertNotEqual(result.returncode, 0, env)
            self.assertIn("registry tokens", result.stderr)
        self.assertEqual(self.sent(), [])


if __name__ == "__main__":
    unittest.main()
