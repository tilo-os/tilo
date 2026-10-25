from __future__ import annotations

import subprocess
import tempfile
import unittest
from fnmatch import fnmatchcase
from pathlib import Path

from tools.policy.check_workflow_package_paths import (
    audit,
    dependency_directories,
    pattern_covers,
    t_check_blocks,
    unmatched_patterns,
    workflow_path_blocks,
)


ROOT = Path(__file__).resolve().parents[2]


def metadata(root: Path) -> dict:
    def package(name: str, directory: str) -> dict:
        return {
            "id": name,
            "name": name,
            "manifest_path": str(root / directory / "Cargo.toml"),
        }

    return {
        "packages": [
            package("app", "apps/app"),
            package("shared", "crates/shared"),
            {
                "id": "external",
                "name": "external",
                "manifest_path": "/registry/external/Cargo.toml",
            },
        ],
        "resolve": {
            "nodes": [
                {"id": "app", "dependencies": ["shared", "external"]},
                {"id": "shared", "dependencies": []},
                {"id": "external", "dependencies": []},
            ]
        },
    }


class WorkflowPackagePathTests(unittest.TestCase):
    def test_a_t_check_naming_the_package_covers_its_closure(self) -> None:
        text = """
[[check]]
name = "image"
run = "build"
cargo_packages = ["app", "tool-*"]
inputs = ["tools/build.sh"]

[[check]]
name = "packages-only"
run = "build"
cargo_packages = ["other"]

[[check]]
name = "globs"
run = "build"
inputs = ["apps/app/**"]

[[check]]
name = "per-arch-{arch}"
run = "build {arch}"
inputs = ["tools/{arch}.sh"]

[check.matrix]
arch = ["x86_64", "aarch64"]
"""
        self.assertEqual(t_check_blocks(text, "image", "app"), [])
        self.assertEqual(t_check_blocks(text, "per-arch-aarch64", "app"), [{"tools/aarch64.sh"}])
        self.assertEqual(t_check_blocks(text, "image", "tool-x"), [])
        self.assertEqual(t_check_blocks(text, "image", "shared"), [{"tools/build.sh"}])
        self.assertEqual(t_check_blocks(text, "packages-only", "app"), [set()])
        self.assertEqual(t_check_blocks(text, "globs", "app"), [{"apps/app/**"}])
        with self.assertRaises(ValueError):
            t_check_blocks(text, "missing", "app")

    def test_workflow_path_filters_cover_every_package_closure(self) -> None:
        with self.subTest(phase='extracts each paths block and ignores other lists'):
            text = """on:
  push:
    paths:
      - "apps/app/**"
      - crates/shared/** # local dependency
  pull_request:
    paths:
      - 'apps/app/**'
jobs:
  check:
    steps:
      - run: cargo test
"""
            self.assertEqual(
                workflow_path_blocks(text),
                [
                    {"apps/app/**", "crates/shared/**"},
                    {"apps/app/**"},
                ],
            )
        with self.subTest(phase='dependency closure excludes registry packages'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.assertEqual(
                    dependency_directories(metadata(root), root, "app"),
                    {"apps/app", "crates/shared"},
                )
        with self.subTest(phase='directory and broad globs cover a package'):
            self.assertTrue(pattern_covers("crates/shared", "crates/shared/**"))
            self.assertTrue(pattern_covers("crates/shared", "crates/**"))
            self.assertFalse(pattern_covers("crates/shared", "apps/**"))
        with self.subTest(phase='every trigger block must cover the closure'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                violations = audit(
                    metadata(root),
                    root,
                    "app",
                    """on:
  push:
    paths:
      - apps/app/**
      - crates/shared/**
  pull_request:
    paths:
      - apps/app/**
""",
                )
            self.assertEqual(
                [str(item) for item in violations],
                ['paths block 2 misses - "crates/shared/**"'],
            )
        with self.subTest(phase='a pattern matching no tracked file is reported'):
            text = "on:\n  push:\n    paths:\n      - apps/app/**\n      - apps/gone.rs\n      - '!apps/x'\n"
            self.assertEqual(unmatched_patterns(text, ["apps/app/src/lib.rs"]), ["apps/gone.rs"])
        with self.subTest(phase='every repository workflow trigger names a tracked file'):
            files = subprocess.run(["git", "ls-files"], cwd=ROOT, text=True,
                                   capture_output=True, check=True).stdout.split()
            stale = [
                f"{workflow.name}: {pattern}"
                for workflow in sorted((ROOT / ".forgejo/workflows").glob("*.yml"))
                for pattern in unmatched_patterns(workflow.read_text(encoding="utf-8"), files)
            ]
            self.assertEqual(stale, [])
        with self.subTest(phase='repository targeted workflows follow package closures'):
            from tools.policy.check_workflow_package_paths import load_metadata

            repository_metadata = load_metadata(ROOT)
            t_text = (ROOT / "t.toml").read_text(encoding="utf-8")
            relay_inputs = {
                ".cargo/**",
                "Cargo.toml",
                "Cargo.lock",
                "keys/cosign.pub",
                "os/base-snapshot.env",
                "os/dev/rpm-snapshot-fingerprint.py",
                "os/dev/verify-rpm-snapshot-package-set.sh",
            }
            relay_blocks = t_check_blocks(t_text, "image-relay", "tilo-relay")
            self.assertEqual(len(relay_blocks), 1)
            self.assertTrue(
                relay_inputs <= relay_blocks[0],
                f"image-relay misses direct build/signing inputs: {sorted(relay_inputs - relay_blocks[0])}",
            )
            # The kernel recipe builds tilo-crypto without default features
            # (os/kernel/Containerfile, os/kernel/project-crate.py).
            kernel_signer = ("--no-default-features",)
            pairs = (
                ("tilo-relay", "image-relay", ()),
                ("tilo-fs", "image-relay", ()),
                ("tilo-crypto", "image-kernel-x86_64", kernel_signer),
                ("tilo-crypto", "image-kernel-aarch64", kernel_signer),
            )
            violations: list[str] = []
            for package, check, cargo_args in pairs:
                blocks = t_check_blocks(t_text, check, package)
                self.assertTrue(blocks, f"{check} names {package} in cargo_packages; audit its inputs instead")
                violations.extend(
                    f"t.toml#{check}: {item}"
                    for item in audit(repository_metadata, ROOT, package, "", blocks, cargo_args)
                )
            self.assertEqual(violations, [])
        with self.subTest(phase='relay image inputs exclude test-only workspace edges'):
            # The image Containerfile runs cargo build -p tilo-relay -p fs --bins.
            # Dev-harness crates must not schedule that rebuild.
            (block,) = t_check_blocks((ROOT / "t.toml").read_text(encoding="utf-8"),
                                      "image-relay", "tilo-relay")
            for path in (
                "apps/notes/**",
                "apps/chat/**",
                "crates/sdk/ui-core/**",
                "crates/browser/tilo-browser-runtime/**",
                "vendor/vello/**",
            ):
                self.assertNotIn(path, block, path)
            for path in ("crates/hosting/tilo-relay/src/main.rs", "crates/system/fs/src/lib.rs"):
                self.assertTrue(any(fnmatchcase(path, pattern) for pattern in block), path)
            self.assertIn("crates/hosting/tilo-relay/**", block)
            self.assertIn("crates/system/fs/**", block)
            self.assertIn("vendor/genawaiter-proc-macro/**", block)

if __name__ == "__main__":
    unittest.main()
