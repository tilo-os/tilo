import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_record_reads.py"
SPEC = importlib.util.spec_from_file_location("check_app_record_reads", SCRIPT)
record_reads = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = record_reads
SPEC.loader.exec_module(record_reads)


READS = '''
#[resource(
    kind = "notes.notebook",
    label = "Notebook",
    search = Notebook::search,
    read_content = Notebook::read_note
)]
impl Notebook {}
'''

READS_DIRECT_HELPER = '''
#[resource(
    kind = "calculator.document",
    label = "Calculator",
    read_content = tilo_resource::read::fields
)]
impl Calculator {}
'''

READS_UI_HELPER = '''
#[resource(
    kind = "messages.thread",
    read_content = ui::tilo_resource::read::fields
)]
impl Providers {}
'''

READS_IN_DOCUMENT = '''
#[resource(kind = "code_editor.project", label = "Project", read_content_in_doc = Project::content)]
impl Project {}
'''

SILENT = '''
#[resource(kind = "calculator.document", label = "Calculator")]
impl Calculator {}
'''

DECLARED_UNREADABLE = '''
#[resource(kind = "osk.learned", read = false)]
impl Learned {}
'''


class RecordReadsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_a_record_that_reads_is_fine(self):
        root = self.fixture({"apps/settings/src/notebook.rs": READS})
        self.assertEqual(record_reads.check(root), [])

    def test_a_record_that_names_the_direct_helper_is_fine(self):
        root = self.fixture({
            "apps/calculator/src/resource.rs": READS_DIRECT_HELPER,
            "crates/desktop/settings-sections/src/providers.rs": READS_UI_HELPER,
        })
        self.assertEqual(record_reads.check(root), [])

    def test_a_read_that_looks_at_the_document_counts(self):
        root = self.fixture({"apps/code-editor/src/project.rs": READS_IN_DOCUMENT})
        self.assertEqual(record_reads.check(root), [])

    def test_a_record_without_a_read_fails_by_name(self):
        root = self.fixture({"apps/calculator/src/resource.rs": SILENT})
        errors = record_reads.check(root)
        self.assertEqual(len(errors), 1)
        self.assertIn("apps/calculator/src/resource.rs:2", errors[0])
        self.assertIn("calculator.document", errors[0])
        self.assertIn("read_content = tilo_resource::read::fields", errors[0])
        self.assertIn("read = false", errors[0])

    def test_a_record_that_declares_it_answers_no_read_is_fine(self):
        root = self.fixture({"apps/osk/src/learned.rs": DECLARED_UNREADABLE})
        self.assertEqual(record_reads.check(root), [])

    def test_a_doc_comment_naming_the_macro_is_not_a_declaration(self):
        code = "/// The facts (`#[resource(facts = path)]`).\npub fn facts() {}\n"
        root = self.fixture({"apps/slides/src/outline.rs": code})
        self.assertEqual(record_reads.check(root), [])


if __name__ == "__main__":
    unittest.main()
