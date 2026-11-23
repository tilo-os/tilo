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
        crate_path: str = "crates/ui/ui-core",
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
                crate_path="crates/sdk/tilo-process-services",
            )
            self.assertEqual(contract.audit(root), [])
        with self.subTest(phase='the clock module lives in the host crate only'):
            root = self.fixture({"time.rs": "pub use std::time::Instant;\n"})
            paths = [Path(v.path).as_posix() for v in contract.audit(root)]
            self.assertEqual(paths, ["crates/ui/ui-core/src/time.rs"])
        with self.subTest(phase='every toolkit crate is audited'):
            root = self.fixture({"a.rs": "use std::time::Instant;\n"})
            host = root / "crates/sdk/tilo-process-services"
            (host / "src").mkdir(parents=True)
            (host / "src/b.rs").write_text("use std::time::SystemTime;\n", encoding="utf-8")
            audited = sorted(Path(v.path).as_posix() for v in contract.audit(root))
            self.assertEqual(
                audited,
                [
                    "crates/sdk/tilo-process-services/clippy.toml",
                    "crates/sdk/tilo-process-services/src/b.rs",
                    "crates/ui/ui-core/src/a.rs",
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

    def tree(self, files: dict[str, str]) -> Path:
        """A root holding the tree's sources at their own paths."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "clippy.toml").write_text(ROOT_CLIPPY, encoding="utf-8")
        toolkit = root / "crates/ui/ui-core"
        (toolkit / "src").mkdir(parents=True)
        (toolkit / "clippy.toml").write_text(CRATE_CLIPPY, encoding="utf-8")
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def found(self, root: Path) -> list[str]:
        return [f"{violation.path}:{violation.line}" for violation in contract.audit(root)]

    def test_a_clock_type_named_through_the_clock_is_not_a_reading_through_it(self):
        with self.subTest(phase='the module path is read directly'):
            root = self.tree(
                {
                    "apps/board/src/faces.rs": (
                        "use ui::time::Instant;\n"
                        "pub fn a() -> Instant { ui::time::Instant::now() }\n"
                        "pub fn b() -> ui::time::SystemTime { ui::time::SystemTime::now() }\n"
                    ),
                }
            )
            self.assertEqual(
                self.found(root),
                ["apps/board/src/faces.rs:2", "apps/board/src/faces.rs:3"],
            )
        with self.subTest(phase='a bare name imported from the clock module is read directly'):
            root = self.tree(
                {
                    "apps/board/src/roster.rs": (
                        "use ui::time::{Duration, Instant};\n"
                        "fn held() -> Duration { Instant::now().elapsed() }\n"
                    ),
                    "apps/board/src/stamps.rs": (
                        "use ui::time::SystemTime;\n"
                        "fn stamp() -> SystemTime { SystemTime::now() }\n"
                    ),
                }
            )
            self.assertEqual(
                self.found(root),
                ["apps/board/src/roster.rs:2", "apps/board/src/stamps.rs:2"],
            )
        with self.subTest(phase='a glob import brings both of the clock types in'):
            root = self.tree(
                {"apps/board/src/glob.rs": "use ui::time::*;\nfn f() { let _ = Instant::now(); }\n"}
            )
            self.assertEqual(self.found(root), ["apps/board/src/glob.rs:2"])
        with self.subTest(phase='the toolkit reads its own clock module the same way'):
            root = self.tree(
                {
                    "crates/ui/ui-core/src/schedule.rs": (
                        "pub fn due() -> crate::time::Instant { crate::time::Instant::now() }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), ["crates/ui/ui-core/src/schedule.rs:1"])
        with self.subTest(phase='the toolkit named by its crate name'):
            root = self.tree(
                {
                    "crates/sdk/tilo-app/src/saves.rs": (
                        "use ui_core::time::Instant;\n"
                        "pub fn at() -> Instant { ui_core::time::Instant::now() }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), ["crates/sdk/tilo-app/src/saves.rs:2"])

    def test_a_reading_through_the_clock_passes_and_another_time_module_is_not_it(self):
        with self.subTest(phase='the clock module and its types are read through it'):
            root = self.tree(
                {
                    "apps/board/src/clock.rs": (
                        "use ui::time::{Duration, Instant};\n"
                        "pub fn t(now: Instant) -> Duration { ui::time::now() - now }\n"
                        "pub fn w() -> ui::time::SystemTime { ui::time::wall() }\n"
                    ),
                    "crates/ui/ui-core/src/clock.rs": (
                        "use crate::time::Instant;\n"
                        "pub fn t(now: Instant) -> crate::time::Instant { crate::time::now() }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), [])
        with self.subTest(phase='a renamed import binds another name'):
            root = self.tree(
                {"apps/board/src/renamed.rs": "use ui::time::Instant as Clock;\nfn f() -> Clock { Clock::now() }\n"}
            )
            self.assertEqual(self.found(root), [])
        with self.subTest(phase='a nearer std import shadows the clock module'):
            root = self.tree(
                {
                    "apps/board/src/shadow.rs": (
                        "use ui::time::Instant;\n"
                        "fn f() { use std::time::Instant; let _ = Instant::now(); }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), [])
        with self.subTest(phase='another crate names its own time module'):
            root = self.tree(
                {
                    "apps/board/src/own_clock.rs": (
                        "use crate::time::Instant;\nfn f() -> Instant { Instant::now() }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), [])

    def test_a_renamed_import_does_not_bind_or_shadow_the_bare_clock_name(self):
        with self.subTest(phase='a renamed clock-module import does not bind the bare name'):
            root = self.tree(
                {
                    "apps/board/src/instant.rs": (
                        "use std::time::Instant;\n"
                        "use ui::time::Instant as Clock;\n"
                        "fn f() -> Instant { Instant::now() }\n"
                    ),
                    "apps/board/src/wall.rs": (
                        "use std::time::SystemTime;\n"
                        "use ui::time::SystemTime as Wall;\n"
                        "fn f() -> SystemTime { SystemTime::now() }\n"
                    ),
                    "apps/board/src/braced.rs": (
                        "use std::time::Instant;\n"
                        "use ui::time::{Duration, Instant as Clock};\n"
                        "fn f() -> Instant { Instant::now() }\n"
                    ),
                }
            )
            self.assertEqual(self.found(root), [])
        with self.subTest(phase='a renamed std import does not shadow an earlier clock-module import'):
            root = self.tree(
                {
                    "apps/board/src/instant.rs": (
                        "use ui::time::Instant;\n"
                        "use std::time::Instant as StdInstant;\n"
                        "fn f() -> Instant { Instant::now() }\n"
                    ),
                    "apps/board/src/wall.rs": (
                        "use ui::time::SystemTime;\n"
                        "use std::time::SystemTime as StdSystemTime;\n"
                        "fn f() -> SystemTime { SystemTime::now() }\n"
                    ),
                    "apps/board/src/braced.rs": (
                        "use ui::time::{Instant as Clock, SystemTime};\n"
                        "fn f() -> SystemTime { SystemTime::now() }\n"
                    ),
                }
            )
            self.assertEqual(
                self.found(root),
                [
                    "apps/board/src/braced.rs:2",
                    "apps/board/src/instant.rs:3",
                    "apps/board/src/wall.rs:3",
                ],
            )

    def test_a_read_past_the_clock_in_tests_or_comments_is_exempt(self):
        root = self.tree(
            {
                "apps/board/tests/probe.rs": "fn f() { let _ = ui::time::Instant::now(); }\n",
                "apps/board/src/module.rs": (
                    "#[cfg(test)]\nmod tests { fn f() { let _ = ui::time::Instant::now(); } }\n"
                ),
                "apps/board/src/commented.rs": (
                    "fn f() { let _ = 1; } // ui::time::Instant::now()\n"
                    'const NAMED: &str = "ui::time::SystemTime::now()";\n'
                ),
            }
        )
        self.assertEqual(self.found(root), [])

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
            crate = root / "crates/ui/ui-core/clippy.toml"
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
