"""tools/policy/check_compiled_surfaces.py: built code embeds compiled surfaces only."""
from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/policy"))
import check_compiled_surfaces as check  # noqa: E402


class CompiledSurfacesTests(unittest.TestCase):
    def audit(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for path, text in files.items():
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).write_text(text, encoding="utf-8")
            original = check.ROOT
            check.ROOT = root
            try:
                violations, scanned = check.audit(root)
            finally:
                check.ROOT = original
            self.assertEqual(scanned, len([p for p in files if "/tests/" not in p]))
            return violations

    def test_an_authored_surface_embedded_in_a_crate_is_refused(self):
        violations = self.audit({
            "crates/desktop/session/src/state.rs":
                'pub const S: &str = include_str!("../../window-menu-surface.toml");\n',
        })
        self.assertEqual(
            violations,
            ["crates/desktop/session/src/state.rs: embeds the authored surface "
             "../../window-menu-surface.toml"],
        )

    def test_the_compiled_surface_and_the_templates_pass(self):
        self.assertEqual(self.audit({
            "crates/desktop/session/src/state.rs":
                'pub const S: &str = include_str!("../../window-menu-surface.json");\n',
            "crates/lang/tilo-app-builder/src/lib.rs":
                'const T: &str = include_str!("../templates/starter.surface.toml");\n',
        }), [])

    def test_a_test_may_embed_the_authored_surface_it_compiles(self):
        self.assertEqual(self.audit({
            "crates/core/x/src/lib.rs": "pub fn f() {}\n",
            "crates/core/x/tests/compile.rs":
                'const S: &str = include_str!("../surface.toml");\n',
        }), [])


if __name__ == "__main__":
    unittest.main()
