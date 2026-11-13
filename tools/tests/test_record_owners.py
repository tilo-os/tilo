import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_record_owners.py"
SPEC = importlib.util.spec_from_file_location("check_record_owners", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class RecordOwnersTests(unittest.TestCase):
    """The session opens one ``RecordOwners``, in ``owners.rs``; any other construction fails."""

    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_the_one_construction_the_session_may_make(self):
        root = self.fixture(
            {
                "crates/session/session/src/owners.rs": (
                    "fn one(root: PathBuf) -> Self {\n"
                    "    Self { owners: RecordOwners::new(root) }\n"
                    "}\n"
                ),
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_a_second_construction_in_a_part_is_refused(self):
        root = self.fixture(
            {
                "crates/session/session/src/agents.rs": (
                    "fn two(root: PathBuf) {\n"
                    "    let owners = RecordOwners::new(root);\n"
                    "}\n"
                ),
                "crates/session/session/src/conversations.rs": (
                    "fn three() {\n"
                    "    let store = Rc::new(MemoryStore::new());\n"
                    "    let owners = RecordOwners::with_store(store);\n"
                    "}\n"
                ),
            }
        )
        found = sorted((v.path, v.line, v.name) for v in contract.audit(root))
        self.assertEqual(
            found,
            [
                ("crates/session/session/src/agents.rs", 2, "new"),
                ("crates/session/session/src/conversations.rs", 3, "with_store"),
            ],
        )

    def test_a_test_module_may_open_its_own_store(self):
        root = self.fixture(
            {
                "crates/session/session/src/conversations.rs": (
                    "fn part(owners: RecordOwners) {}\n"
                    "#[cfg(test)]\n"
                    "mod tests {\n"
                    "    fn fixture() { let owners = RecordOwners::new(root()); }\n"
                    "}\n"
                ),
                "crates/session/session/tests/canvas_ask.rs": (
                    "fn fixture() { let owners = RecordOwners::new(root()); }\n"
                ),
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_a_clone_of_the_session_owners_is_not_a_construction(self):
        root = self.fixture(
            {
                "crates/session/session/src/agents.rs": (
                    "fn part(owners: SessionOwners) {\n"
                    "    let owners = owners.owners();\n"
                    "}\n"
                ),
            }
        )
        self.assertEqual(contract.audit(root), [])


if __name__ == "__main__":
    unittest.main()
