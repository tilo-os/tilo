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
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "fingerprint", ROOT / "os/dev/rpm-snapshot-fingerprint.py"
)
fingerprint = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fingerprint)

DIGEST = re.compile(r"registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}")
RESOLVE = ROOT / "os/dev/resolve-rpm-snapshot.sh"


class Fingerprint(unittest.TestCase):
    def _clone(self, temporary: str) -> Path:
        repo = Path(temporary) / "repo"
        shutil.copytree(
            ROOT,
            repo,
            ignore=shutil.ignore_patterns(
                ".git", "target", "os/output", "node_modules", "__pycache__"
            ),
            symlinks=True,
        )
        return repo

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
        for path in ROOT.rglob("*"):
            if not path.is_file() or path.is_symlink():
                continue
            relative = path.relative_to(ROOT).as_posix()
            if relative.startswith(("target/", ".git/", "os/output/")):
                continue
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
        relay = (ROOT / ".forgejo/workflows/release-relay.yml").read_text(encoding="utf-8")
        snapshot = (ROOT / ".forgejo/workflows/build-rpms.yml").read_text(encoding="utf-8")
        for consumer in (build, relay):
            self.assertIn("Resolve the exact signed RPM snapshot fingerprint", consumer)
            self.assertIn("./os/dev/resolve-rpm-snapshot.sh", consumer)
            self.assertIn("rpms_ref: ${{ steps.resolve.outputs.rpms_ref }}", consumer)
            self.assertNotIn("uses: ./.forgejo/workflows/build-rpms.yml", consumer)
            self.assertNotIn("os/rpms-snapshot.env", consumer)
        self.assertIn("needs.ensure-rpms.result == 'success'", build)
        self.assertIn("--tag \"$FINGERPRINT_TAG\"", snapshot)
        self.assertNotIn("open-snapshot-pr.py", snapshot)
        self.assertNotIn("reconcile-snapshot-pins.py", snapshot)


if __name__ == "__main__":
    unittest.main()
