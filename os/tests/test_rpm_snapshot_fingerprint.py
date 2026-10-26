#!/usr/bin/env python3
"""Fingerprint resolution replaces the hand-kept RPM snapshot pin."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
import t_matrix  # noqa: E402
spec = importlib.util.spec_from_file_location(
    "fingerprint", ROOT / "os/dev/rpm-snapshot-fingerprint.py"
)
fingerprint = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fingerprint)

DIGEST = re.compile(r"registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}")
RESOLVE = ROOT / "os/dev/resolve-rpm-snapshot.sh"


class Fingerprint(unittest.TestCase):
    def _clone(self, temporary: str) -> Path:
        # The fingerprint and its resolver read only os/; copying the whole
        # checkout made each test scale with whatever else it holds.
        repo = Path(temporary) / "repo"
        shutil.copytree(
            ROOT / "os",
            repo / "os",
            ignore=shutil.ignore_patterns("output", "__pycache__"),
            symlinks=True,
        )
        return repo

    @staticmethod
    def _source_files():
        """Regular files of the checkout, never descending into build output."""
        skipped = {ROOT / ".git", ROOT / "target", ROOT / "os/output"}
        for directory, subdirectories, files in os.walk(ROOT):
            base = Path(directory)
            subdirectories[:] = [
                name for name in subdirectories
                if base / name not in skipped and name != "node_modules"
            ]
            for name in files:
                path = base / name
                if path.is_file() and not path.is_symlink():
                    yield path

    def test_package_set_change_changes_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo = self._clone(temporary)
            before = fingerprint.fingerprint(repo)
            consumer = repo / "os/Containerfile"
            consumer.write_text(
                consumer.read_text(encoding="utf-8")
                + "\nRUN dnf install -y tilo-fingerprint-probe-package\n",
                encoding="utf-8",
            )
            after = fingerprint.fingerprint(repo)
            self.assertNotEqual(before, after)
            self.assertRegex(before, r"^[0-9a-f]{64}$")
            self.assertRegex(after, r"^[0-9a-f]{64}$")

    def test_shape_file_change_changes_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo = self._clone(temporary)
            before = fingerprint.fingerprint(repo)
            shape = repo / "os/Containerfile.rpms"
            shape.write_text(
                shape.read_text(encoding="utf-8") + "\n# fingerprint probe\n",
                encoding="utf-8",
            )
            self.assertNotEqual(before, fingerprint.fingerprint(repo))

    def test_no_committed_file_names_an_rpms_digest(self) -> None:
        pinned = []
        for path in self._source_files():
            relative = path.relative_to(ROOT).as_posix()
            if relative.endswith((
                ".png", ".jpg", ".webp", ".ico", ".woff2", ".ttf",
                ".bin", ".tar", ".gz", ".xz", ".rpm", ".so",
            )):
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            for match in DIGEST.finditer(text):
                pinned.append(f"{relative}: {match.group(0)}")
        self.assertEqual(
            pinned,
            [],
            "committed files must not name an rpms digest; resolve by fingerprint",
        )

    def test_resolution_does_not_fall_back_to_hand_kept_pin(self) -> None:
        source = RESOLVE.read_text(encoding="utf-8")
        self.assertNotIn("rpms-snapshot.env", source)
        self.assertIn("set-$fingerprint", source)
        # Only base-snapshot.env may be sourced; never a pin file.
        sourced = [line for line in source.splitlines() if line.lstrip().startswith(". ")]
        self.assertEqual(sourced, ['. "$repo/os/base-snapshot.env"'])
        with tempfile.TemporaryDirectory() as temporary:
            repo = self._clone(temporary)
            pin = repo / "os/rpms-snapshot.env"
            pin.write_text(
                "RPMS_REF=registry.tilo.so/tilo/rpms@sha256:"
                + ("a" * 64)
                + "\nRPMS_SOURCE_REVISION="
                + ("b" * 40)
                + "\n",
                encoding="utf-8",
            )
            script = (repo / "os/dev/resolve-rpm-snapshot.sh").read_text(encoding="utf-8")
            self.assertNotIn("rpms-snapshot.env", script)
            bindir = repo / "bin"
            bindir.mkdir()
            (bindir / "skopeo").write_text(
                "#!/bin/bash\n"
                "printf 'skopeo-was-invoked-for:%s\\n' \"$*\" >&2\n"
                "exit 1\n",
                encoding="utf-8",
            )
            (bindir / "skopeo").chmod(0o755)
            env = {
                **os.environ,
                "PATH": str(bindir) + ":" + os.environ.get("PATH", ""),
            }
            result = subprocess.run(
                ["bash", str(repo / "os/dev/resolve-rpm-snapshot.sh")],
                cwd=repo,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            combined = result.stdout + result.stderr
            self.assertNotIn("sha256:" + ("a" * 64), combined)
            self.assertIn("set-", combined)
            self.assertNotIn("rpms-snapshot.env", combined)

    def test_image_builds_resolve_standalone_signed_snapshot(self) -> None:
        build = (ROOT / ".forgejo/workflows/build-image.yml").read_text(encoding="utf-8")
        self.assertIn("Resolve the exact signed RPM snapshot fingerprint", build)
        self.assertIn("./os/dev/resolve-rpm-snapshot.sh", build)
        self.assertIn("rpms_ref: ${{ steps.resolve.outputs.rpms_ref }}", build)
        self.assertNotIn("os/rpms-snapshot.env", build)
        self.assertIn("needs.ensure-rpms.result == 'success'", build)
        # The relay and the env-runtime install from the snapshot t's rpm-snapshot check
        # resolves and verifies, never from a pin file.
        checks = {check["name"]: check for check in t_matrix.checks(ROOT)}
        self.assertEqual(checks["rpm-snapshot"]["run"],
                         "bash tools/ci/publish/rpm-snapshot-ref.sh /target/publish/rpm-snapshot")
        resolver = (ROOT / "tools/ci/publish/rpm-snapshot-ref.sh").read_text(encoding="utf-8")
        self.assertIn('ref=$(bash "$repo_root/os/dev/resolve-rpm-snapshot.sh")', resolver)
        self.assertNotIn("rpms-snapshot.env", resolver)
        for consumer in ("image-relay", "image-env-runtime-tree-x86_64", "image-env-runtime-tree-aarch64"):
            with self.subTest(consumer=consumer):
                self.assertIn("rpm-snapshot", checks[consumer]["needs"])
                self.assertIn("/target/publish/rpm-snapshot", checks[consumer]["run"])
        # Stable publishes a new package set before resolving it, in the same run; elsewhere the
        # builds fall back to the run's own image-rpms of their architecture.
        self.assertEqual(checks["rpm-snapshot"].get("optional_needs"), ["publish-rpms"])
        self.assertEqual(checks["publish-rpms"]["on"], ["stable"])
        self.assertIn("image-rpms-x86_64", checks["image-relay"]["needs"])
        for arch in ("x86_64", "aarch64"):
            tree = checks[f"image-env-runtime-tree-{arch}"]
            self.assertIn(f"image-rpms-{arch}", tree["needs"])
            self.assertIn(f"/target/publish/rpms/{arch}", tree["run"])
        self.assertIn("/target/publish/relay/rpms-source", checks["image-relay"]["outputs"])
        # The snapshot is published once per package-set fingerprint, never proposed as a pin.
        self.assertIn("'once:set-{fingerprint}'", checks["publish-rpms"]["run"])
        for name in ("image-rpms-x86_64", "provenance-rpms", "sign-rpms", "publish-rpms"):
            self.assertNotIn("open-snapshot-pr.py", checks[name]["run"])
        for script in ("rpms-build.sh", "rpms-provenance.sh"):
            text = (ROOT / "tools/ci/publish" / script).read_text(encoding="utf-8")
            self.assertNotIn("open-snapshot-pr.py", text)
            self.assertNotIn("reconcile-snapshot-pins.py", text)

    def test_only_a_build_of_the_published_snapshot_is_signed(self) -> None:
        # lib.sh require_published_snapshot, which oci-sign.sh and a publisher's env-runtime
        # seal run on what the build recorded (snapshot_for_build).
        published = "registry.tilo.so/tilo/rpms@sha256:" + "b" * 64
        cases = {published: 0, "unsigned image-rpms-x86_64 sha256:" + "c" * 64: 1}
        for source, code in cases.items():
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmp:
                recorded = Path(tmp) / "rpms-source"
                recorded.write_text(source + "\n", encoding="utf-8")
                result = subprocess.run(
                    ["bash", "-c", '. "$1"; require_published_snapshot "$2"', "bash",
                     str(ROOT / "tools/ci/publish/lib.sh"), str(recorded)],
                    capture_output=True, text=True, env={**os.environ, "TMPDIR": tmp})
                self.assertEqual(result.returncode != 0, bool(code), result.stderr)
                if code:
                    self.assertIn("refusing to sign or publish", result.stderr)
        sign = (ROOT / "tools/ci/publish/oci-sign.sh").read_text(encoding="utf-8")
        self.assertIn('require_published_snapshot "$out/rpms-source"', sign)
        runtime = (ROOT / "tools/ci/publish/env-runtime.sh").read_text(encoding="utf-8")
        self.assertIn('require_published_snapshot "$trees/env-runtime-rpms-$arch.txt"', runtime)


if __name__ == "__main__":
    unittest.main()
