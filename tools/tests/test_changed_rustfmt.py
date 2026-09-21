from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "ci/check-changed-rustfmt.sh"


class ChangedRustfmtTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "ci@example.invalid"],
            cwd=self.root,
            check=True,
        )
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        (self.root / "base.rs").write_text("fn base() {}\n", encoding="utf-8")
        (self.root / "notes.txt").write_text("base\n", encoding="utf-8")
        self.commit("base")
        self.base = self.git("rev-parse", "HEAD")
        self.arguments = self.root / "rustfmt-arguments"
        fake = self.bin / "rustfmt"
        fake.write_text(
            "#!/usr/bin/env bash\n"
            "printf '<%s>\\n' \"$@\" > \"$RUSTFMT_ARGUMENTS\"\n",
            encoding="utf-8",
        )
        fake.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def git(self, *args: str) -> str:
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def commit(self, message: str) -> None:
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", message], cwd=self.root, check=True)

    def run_check(self, base: str = "", before: str = "") -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{self.bin}:{environment['PATH']}",
                "TILO_REPO_ROOT": str(self.root),
                "RUSTFMT_ARGUMENTS": str(self.arguments),
            }
        )
        return subprocess.run(
            ["bash", str(SCRIPT), base, before],
            text=True,
            capture_output=True,
            check=True,
            env=environment,
        )

    def test_only_changed_rust_files_are_passed_with_safe_argument_boundary(self) -> None:
        (self.root / "base.rs").write_text("fn changed() {}\n", encoding="utf-8")
        (self.root / "space name.rs").write_text("fn spaced() {}\n", encoding="utf-8")
        (self.root / "notes.txt").write_text("changed\n", encoding="utf-8")
        self.commit("change")

        result = self.run_check(self.base)

        self.assertIn("changed Rust formatting OK (2 files)", result.stdout)
        self.assertEqual(
            self.arguments.read_text(encoding="utf-8").splitlines(),
            ["<--edition>", "<2024>", "<--check>", "<-->", "<base.rs>", "<space name.rs>"],
        )

    def test_upstream_vendor_is_excluded_but_first_party_nested_vendor_is_checked(self) -> None:
        for name in ("vendor/codec/src/lib.rs", "crates/demo/vendor/local.rs"):
            path = self.root / name
            path.parent.mkdir(parents=True)
            path.write_text("#[allow(dead_code)]\nfn f() {}\n", encoding="utf-8")
        self.commit("import")

        self.run_check(self.base)
        arguments = self.arguments.read_text(encoding="utf-8")
        self.assertNotIn("<vendor/codec/src/lib.rs>", arguments)
        self.assertIn("<crates/demo/vendor/local.rs>", arguments)
        result = subprocess.run(
            ["python3", str(SCRIPT.with_name("check_changed_rust_allows.py")),
             "--root", str(self.root), self.base], text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("crates/demo/vendor/local.rs:1:", result.stderr)
        self.assertNotIn("vendor/codec/src/lib.rs", result.stderr)

    def test_checked_in_capnp_bindings_are_excluded(self) -> None:
        generated = self.root / "crates/runtime/demo/src/generated/demo_capnp.rs"
        generated.parent.mkdir(parents=True)
        generated.write_text("// generated, intentionally not rustfmt input\n", encoding="utf-8")
        ordinary = self.root / "crates/runtime/demo/src/lib.rs"
        ordinary.write_text("fn checked() {}\n", encoding="utf-8")
        self.commit("generated bindings")

        self.run_check(self.base)
        arguments = self.arguments.read_text(encoding="utf-8")
        self.assertNotIn("<crates/runtime/demo/src/generated/demo_capnp.rs>", arguments)
        self.assertIn("<crates/runtime/demo/src/lib.rs>", arguments)

    def test_push_zero_base_falls_back_to_parent(self) -> None:
        (self.root / "base.rs").write_text("fn changed() {}\n", encoding="utf-8")
        self.commit("change")

        self.run_check("", "0" * 40)

        self.assertIn("<base.rs>", self.arguments.read_text(encoding="utf-8"))

    def test_non_rust_change_skips_rustfmt(self) -> None:
        (self.root / "notes.txt").write_text("changed\n", encoding="utf-8")
        self.commit("docs")

        result = self.run_check(self.base)

        self.assertIn("no Rust files", result.stdout)
        self.assertFalse(self.arguments.exists())


if __name__ == "__main__":
    unittest.main()
