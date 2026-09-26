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
    def fixture(
        self,
        files: dict[str, str],
        crate_clippy: str | None = CRATE_CLIPPY,
        crate_path: str = "crates/sdk/ui-core",
    ) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "clippy.toml").write_text(ROOT_CLIPPY, encoding="utf-8")
        crate = root / crate_path
        (crate / "src").mkdir(parents=True)
        if crate_clippy is not None:
            (crate / "clippy.toml").write_text(crate_clippy, encoding="utf-8")
        for relative, text in files.items():
            path = crate / "src" / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_ui_code_must_use_the_crate_clock_outside_tests_and_comments(self):
        with self.subTest(phase='crate clock module passes'):
            root = self.fixture(
                {
                    "time.rs": "pub use std::time::{Instant, SystemTime};\n",
                    "lib.rs": "use crate::time::Instant;\nuse std::time::Duration;\n",
                },
                crate_path="crates/sdk/tilo-host",
            )
            self.assertEqual(contract.audit(root), [])
        with self.subTest(phase='the clock module lives in the host crate only'):
            root = self.fixture({"time.rs": "pub use std::time::Instant;\n"})
            paths = [Path(v.path).as_posix() for v in contract.audit(root)]
            self.assertEqual(paths, ["crates/sdk/ui-core/src/time.rs"])
        with self.subTest(phase='every toolkit crate is audited'):
            root = self.fixture({"a.rs": "use std::time::Instant;\n"})
            host = root / "crates/sdk/tilo-host"
            (host / "src").mkdir(parents=True)
            (host / "src/b.rs").write_text("use std::time::SystemTime;\n", encoding="utf-8")
            audited = sorted(Path(v.path).as_posix() for v in contract.audit(root))
            self.assertEqual(
                audited,
                [
                    "crates/sdk/tilo-host/clippy.toml",
                    "crates/sdk/tilo-host/src/b.rs",
                    "crates/sdk/ui-core/src/a.rs",
                ],
            )
        with self.subTest(phase='std clock paths fail'):
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
        with self.subTest(phase='tests and comments are exempt'):
            root = self.fixture(
                {
                    "a.rs": "// std::time::Instant\nfn f() {}\n#[cfg(test)]\nmod tests { use std::time::Instant; }\n",
                    "w/tests.rs": "use std::time::Instant;\n",
                    "w/tests/x.rs": "use std::time::Instant;\n",
                }
            )
            self.assertEqual(contract.audit(root), [])

    def test_crate_clippy_config_repeats_and_may_only_extend_root_policy(self):
        with self.subTest(phase='crate clippy config must repeat root keys'):
            root = self.fixture({}, crate_clippy='disallowed-methods = []\n')
            reasons = [v.reason for v in contract.audit(root)]
            self.assertEqual(len(reasons), 1)
            self.assertIn("allow-unwrap-in-tests", reasons[0])
        with self.subTest(phase='crate clippy config must match root values'):
            root = self.fixture({}, crate_clippy="allow-unwrap-in-tests = false\n")
            reasons = [v.reason for v in contract.audit(root)]
            self.assertEqual(len(reasons), 1)
            self.assertIn("differently", reasons[0])
        with self.subTest(phase='crate clippy list may extend the root list'):
            root = self.fixture({}, crate_clippy=None)
            (root / "clippy.toml").write_text(
                ROOT_CLIPPY + 'disallowed-methods = ["reqwest::get"]\n', encoding="utf-8"
            )
            crate = root / "crates/sdk/ui-core/clippy.toml"
            crate.write_text(
                ROOT_CLIPPY + 'disallowed-methods = ["reqwest::get", "std::time::Instant::now"]\n',
                encoding="utf-8",
            )
            self.assertEqual(contract.audit(root), [])
            crate.write_text(
                ROOT_CLIPPY + 'disallowed-methods = ["std::time::Instant::now"]\n', encoding="utf-8"
            )
            reasons = [v.reason for v in contract.audit(root)]
            self.assertEqual(len(reasons), 1)
            self.assertIn("differently", reasons[0])


if __name__ == "__main__":
    unittest.main()
