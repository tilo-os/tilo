import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_document_engine_boundary.py"
SPEC = importlib.util.spec_from_file_location("check_document_engine_boundary", SCRIPT)
boundary = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = boundary
SPEC.loader.exec_module(boundary)


class DocumentEngineBoundaryTests(unittest.TestCase):
    def fixture(self, files: dict[str, str], baseline: str = "") -> tuple[Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        baseline_path = root / "baseline.toml"
        baseline_path.write_text(baseline, encoding="utf-8")
        return root, baseline_path

    def test_apps_using_the_document_api_pass(self):
        root, baseline = self.fixture(
            {
                "apps/editor/src/lib.rs": (
                    "fn bold(dm: &mut tilo_doc::DocMut) {\n"
                    "    dm.add_mark(\"b\", 0, 1, tilo_doc::schema::MARK_BOLD, true);\n"
                    "}\n"
                ),
            }
        )
        self.assertEqual(boundary.audit(root, baseline), [])

    def test_rich_text_primitives_outside_the_engine_fail(self):
        root, baseline = self.fixture(
            {
                "apps/editor/src/lib.rs": (
                    "use automerge::marks::{ExpandMark, Mark};\n"
                    "fn f(doc: &mut automerge::AutoCommit) {}\n"
                ),
            }
        )
        violations = boundary.audit(root, baseline)
        self.assertEqual(len(violations), 1)
        self.assertIn("rich_text_engine", violations[0].reason)

    def test_reaching_into_the_document_schema_fails(self):
        root, baseline = self.fixture(
            {
                "apps/editor/src/lib.rs": (
                    "fn f(doc: &automerge::AutoCommit, block: &automerge::ObjId) {\n"
                    "    let _ = doc.get(block, tilo_doc::schema::BLOCK_TEXT);\n"
                    "}\n"
                ),
                "apps/other/src/lib.rs": "use tilo_doc::schema::{KEY_HEADER, MARK_BOLD};\n",
            }
        )
        reasons = sorted(v.path for v in boundary.audit(root, baseline))
        self.assertEqual(reasons, ["apps/editor/src/lib.rs", "apps/other/src/lib.rs"])

    def test_engine_owners_tests_and_comments_are_exempt(self):
        root, baseline = self.fixture(
            {
                "crates/doc/tilo-doc/src/model.rs": "use automerge::marks::ExpandMark;\n",
                "crates/sdk/ui-core/src/text.rs": "use automerge::marks::Mark;\n",
                "apps/editor/tests/corrupt.rs": "const K: &str = tilo_doc::schema::BLOCK_ORDER;\n",
                "apps/editor/src/lib.rs": (
                    "// tilo-doc uses automerge::marks internally\n"
                    "fn f() {}\n"
                    "#[cfg(test)]\n"
                    "mod tests { const K: &str = tilo_doc::schema::BLOCK_TEXT; }\n"
                ),
            }
        )
        self.assertEqual(boundary.audit(root, baseline), [])

    def test_the_baseline_is_a_ratchet(self):
        files = {"apps/notes/src/doc.rs": "use automerge::marks::{ExpandMark, Mark};\n"}
        allowed = '[ratchets.rich_text_engine]\n"apps/notes/src/doc.rs" = 2\n'
        root, baseline = self.fixture(files, allowed)
        self.assertEqual(boundary.audit(root, baseline), [])
        tighter = '[ratchets.rich_text_engine]\n"apps/notes/src/doc.rs" = 1\n'
        root, baseline = self.fixture(files, tighter)
        self.assertIn("baseline allows 1", boundary.audit(root, baseline)[0].reason)
        looser = '[ratchets.rich_text_engine]\n"apps/notes/src/doc.rs" = 3\n'
        root, baseline = self.fixture(files, looser)
        self.assertIn("lower the entry", boundary.audit(root, baseline)[0].reason)


if __name__ == "__main__":
    unittest.main()
