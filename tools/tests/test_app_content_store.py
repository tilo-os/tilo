import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_content_store.py"
SPEC = importlib.util.spec_from_file_location("check_app_content_store", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class AppContentStoreTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "apps").mkdir()
        (root / "crates").mkdir()
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_only_the_hosts_that_lend_the_store_open_it(self):
        call = "fn f() { let _ = tilo_content_device::for_app(); }\n"
        local = "fn f() { let _ = tilo_content::ContentStore::local_for_app(); }\n"
        root = self.fixture(
            {
                "apps/paint/src/lib.rs": call,
                "apps/paint/src/doc.rs": "// tilo_content_device::for_app() is not used\n",
                "apps/paint/src/local.rs": local,
                "apps/paint/src/state.rs": "fn g() {}\n#[cfg(test)]\nmod tests {\n" + call + "}\n",
                "apps/paint/tests/app.rs": call,
                "crates/system/content/src/lib.rs": local,
                "crates/system/content-device/src/lib.rs": call,
                "crates/sdk/tilo-app/src/framework/app.rs": call,
                "crates/system/fs/src/main.rs": call,
                "crates/sdk/ui-core/src/lib.rs": "\n" + call,
            }
        )
        found = sorted((Path(v.path).as_posix(), v.line) for v in contract.audit(root))
        self.assertEqual(
            found,
            [
                ("apps/paint/src/lib.rs", 1),
                ("apps/paint/src/local.rs", 1),
                ("crates/sdk/ui-core/src/lib.rs", 2),
            ],
        )


    def test_no_app_keeps_a_store_for_the_whole_process(self):
        root = self.fixture(
            {
                "apps/paint/src/content.rs": (
                    "use std::sync::OnceLock;\n"
                    "static STORE: OnceLock<tilo_content::ContentStore> = OnceLock::new();\n"
                ),
                "apps/paint/src/notes.rs": (
                    "thread_local! {\n"
                    "    static SLOT: RefCell<Option<ContentStore>> = RefCell::new(None);\n"
                    "}\n"
                ),
                "apps/paint/src/ok.rs": "fn f(store: &ContentStore) { let _ = store; }\n",
            }
        )
        found = sorted((Path(v.path).as_posix(), v.line) for v in contract.audit(root))
        self.assertEqual(found, [("apps/paint/src/content.rs", 2), ("apps/paint/src/notes.rs", 2)])


if __name__ == "__main__":
    unittest.main()
