import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_too_many_arguments.py"
SPEC = importlib.util.spec_from_file_location("check_too_many_arguments", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


class TooManyArgumentsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.git(root, "init", "-q")
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def git(self, root: Path, *args: str) -> None:
        subprocess.run(
            ["git", "-C", str(root), *args],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

    def test_ignored_image_registry_cache_is_not_repository_source(self):
        cache = "os/output/.tmp/buildah-cache/cargo/registry/src/index/dep"
        root = self.fixture(
            {
                ".gitignore": "os/output/\n",
                f"{cache}/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "allow"\n',
                f"{cache}/src/lib.rs": "#![allow(clippy::too_many_arguments)]\n",
                "crates/own/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "deny"\n',
                "crates/own/src/lib.rs": "#![deny(clippy::too_many_arguments)]\n",
            }
        )
        self.git(root, "add", ".")
        self.assertEqual(check.audit(root), [])

    def test_new_staged_crates_and_own_sources_cannot_opt_out(self):
        source = "#![allow(clippy::too_many_arguments)]\n"
        manifest = '[lints.clippy]\ntoo_many_arguments = "allow"\n'
        paths = {
            "apps/new/Cargo.toml": manifest,
            "apps/new/src/lib.rs": source,
            "crates/new/Cargo.toml": manifest,
            "crates/new/src/vendor/own.rs": source,
            "tools/new/Cargo.toml": manifest,
            "tools/new/src/target/own.rs": source,
            "elsewhere/new/Cargo.toml": manifest,
            "elsewhere/new/src/node_modules/own.rs": source,
            "os/output/own/Cargo.toml": manifest,
            "os/output/own/src/lib.rs": source,
        }
        root = self.fixture({".gitignore": "os/output/\n", **paths})
        self.git(root, "add", "-f", *paths)
        # Even ignored paths must be checked when they belong to the index.
        self.assertEqual({v.split(":")[0] for v in check.audit(root)}, set(paths))
        # Exact staged blobs remain forbidden if disk copies remove the lint.
        for relative in paths:
            (root / relative).write_text("", encoding="utf-8")
        self.assertEqual({v.split(":")[0] for v in check.audit(root)}, set(paths))
        # A missing working-tree copy must not hide an indexed violation either.
        for relative in paths:
            (root / relative).unlink()
        self.assertEqual({v.split(":")[0] for v in check.audit(root)}, set(paths))

    def test_untracked_sources_outside_conventional_crate_roots_are_checked(self):
        root = self.fixture(
            {
                "new workspace/crate/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "warn"\n',
                "new workspace/crate/src/own\nsource.rs": "#![expect(clippy::too_many_arguments)]\n",
            }
        )
        self.assertEqual(len(check.audit(root)), 2)

    def test_every_escape_hatch_is_rejected(self):
        cases = {
            "outer allow": '#[allow(clippy::too_many_arguments, reason = "x")]\nfn f() {}\n',
            "inner allow": "#![allow(clippy::too_many_arguments)]\n",
            "expect across lines": (
                "#[expect(\n    clippy::too_many_lines,\n    clippy::too_many_arguments,\n)]\nfn f() {}\n"
            ),
            "cfg_attr": "#[cfg_attr(test, allow(clippy::too_many_arguments))]\nfn f() {}\n",
        }
        for name, source in cases.items():
            with self.subTest(name):
                root = self.fixture({"crates/a/src/lib.rs": source})
                violations = check.audit(root)
                self.assertEqual(len(violations), 1, violations)
                self.assertIn("crates/a/src/lib.rs:1:", violations[0])

    def test_a_manifest_cannot_lower_the_lint(self):
        root = self.fixture(
            {
                "crates/a/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "warn"\n',
                "crates/b/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = { level = "allow" }\n',
                "Cargo.toml": '[workspace.lints.clippy]\ntoo_many_arguments = "deny"\n',
            }
        )
        violations = check.audit(root)
        self.assertEqual(
            sorted(v.split(":")[0] for v in violations),
            ["crates/a/Cargo.toml", "crates/b/Cargo.toml"],
        )

    def test_workspace_manifest_cannot_lower_the_lint(self):
        root = self.fixture(
            {"Cargo.toml": '[workspace.lints.clippy]\ntoo_many_arguments = "allow"\n'}
        )
        self.git(root, "add", "Cargo.toml")
        (root / "Cargo.toml").write_text("", encoding="utf-8")
        self.assertEqual(
            check.audit(root),
            ["Cargo.toml: sets clippy::too_many_arguments to 'allow'; it must stay denied"],
        )
    def test_malformed_manifests_cannot_pass_the_audit(self):
        for text in ('[lints.clippy]\ntoo_many_arguments = "allow"\nbroken = [',
                     '[package\nname = "broken"\n'):
            with self.subTest(text=text):
                root = self.fixture({"crates/a/Cargo.toml": text})
                violations = check.audit(root)
                self.assertEqual(len(violations), 1, violations)
                self.assertIn("crates/a/Cargo.toml: cannot audit malformed Cargo manifest:",
                              violations[0])
                self.assertRegex(violations[0], r"\(at (?:line|end of document)")

    def test_denying_mentioning_and_vendored_code_pass(self):
        root = self.fixture(
            {
                "crates/a/src/lib.rs": (
                    "#![allow(clippy::complexity)]\n#![deny(clippy::too_many_arguments)]\n"
                    "// the too_many_arguments lint keeps signatures short\n"
                ),
                "vendor/dep/src/lib.rs": "#[allow(clippy::too_many_arguments)]\nfn f() {}\n",
                "vendor/dep/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "allow"\n',
            }
        )
        self.git(root, "add", ".")
        self.assertEqual(check.audit(root), [])


if __name__ == "__main__":
    unittest.main()
