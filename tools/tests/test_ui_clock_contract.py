import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_ui_clock_contract.py"
SPEC = importlib.util.spec_from_file_location("check_ui_clock_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)

ROOT_CLIPPY = "allow-unwrap-in-tests = true\n"
CRATE_CLIPPY = ROOT_CLIPPY + 'disallowed-methods = ["std::time::Instant::now"]\n'


class UiClockContractTests(unittest.TestCase):
    def fixture(self, files: dict[str, str], crate_clippy: str | None = CRATE_CLIPPY) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "clippy.toml").write_text(ROOT_CLIPPY, encoding="utf-8")
        crate = root / "crates/sdk/ui-core"
        (crate / "src").mkdir(parents=True)
        if crate_clippy is not None:
            (crate / "clippy.toml").write_text(crate_clippy, encoding="utf-8")
        for relative, text in files.items():
            path = crate / "src" / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_crate_clock_module_passes(self):
        root = self.fixture(
            {
                "time.rs": "pub use std::time::{Instant, SystemTime};\n",
                "lib.rs": "use crate::time::Instant;\nuse std::time::Duration;\n",
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_std_clock_paths_fail(self):
        root = self.fixture(
            {
                "a.rs": "use std::time::Instant;\n",
                "b.rs": "use std::time::{Duration, SystemTime};\n",
                "c.rs": "use std::{\n    path::Path,\n    time::{Duration, Instant},\n};\n",
                "d.rs": "fn f() -> std::time::SystemTime { todo!() }\n",
            }
        )
        paths = sorted(Path(v.path).name for v in contract.audit(root))
        self.assertEqual(paths, ["a.rs", "b.rs", "c.rs", "d.rs"])

    def test_tests_and_comments_are_exempt(self):
        root = self.fixture(
            {
                "a.rs": "// std::time::Instant\nfn f() {}\n#[cfg(test)]\nmod tests { use std::time::Instant; }\n",
                "w/tests.rs": "use std::time::Instant;\n",
                "w/tests/x.rs": "use std::time::Instant;\n",
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_crate_clippy_config_must_repeat_root_keys(self):
        root = self.fixture({}, crate_clippy='disallowed-methods = []\n')
        reasons = [v.reason for v in contract.audit(root)]
        self.assertEqual(len(reasons), 1)
        self.assertIn("allow-unwrap-in-tests", reasons[0])

    def test_crate_clippy_config_must_match_root_values(self):
        root = self.fixture({}, crate_clippy="allow-unwrap-in-tests = false\n")
        reasons = [v.reason for v in contract.audit(root)]
        self.assertEqual(len(reasons), 1)
        self.assertIn("differently", reasons[0])


if __name__ == "__main__":
    unittest.main()
