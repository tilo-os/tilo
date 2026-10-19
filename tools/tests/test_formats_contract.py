import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_formats.py"
SPEC = importlib.util.spec_from_file_location("check_formats", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


def reasons(text: str) -> list[str]:
    return [v.reason for v in contract.audit_text("apps/x/src/lib.rs", text)]


class FormatsContractTests(unittest.TestCase):
    def test_hand_built_dates_are_flagged_and_machine_forms_pass(self):
        with self.subTest(phase="a worded strftime pattern"):
            self.assertEqual(len(reasons('let s = at.format("%b %-d, %Y").to_string();\n')), 1)
        with self.subTest(phase="a 12-hour pattern"):
            self.assertEqual(len(reasons('let s = at.format("%I:%M %p").to_string();\n')), 1)
        with self.subTest(phase="ISO 8601 machine forms"):
            for pattern in ("%Y-%m-%d", "%Y-%m-%dT%H:%M:%S%.3fZ", "%Y%m%dT%H%M%SZ", "%Y-%m"):
                self.assertEqual(reasons(f'let s = at.format("{pattern}");\n'), [], pattern)
        with self.subTest(phase="a display time without seconds"):
            self.assertEqual(len(reasons('let s = at.format("%Y-%m-%d %H:%M");\n')), 1)

    def test_clock_parts_sizes_and_name_tables_are_flagged(self):
        with self.subTest(phase="clock from parts"):
            self.assertEqual(len(reasons('let s = format!("{h:02}:{m:02}");\n')), 1)
            self.assertEqual(len(reasons('let s = format!("{:02}:{:02}", h, m);\n')), 1)
        with self.subTest(phase="a computed size with a unit"):
            self.assertEqual(len(reasons('let s = format!("{} MB", n / 1_000_000);\n')), 1)
            self.assertEqual(len(reasons('let s = format!("{mib:.1} MiB");\n')), 1)
        with self.subTest(phase="fixed copy naming a limit is not formatting"):
            self.assertEqual(reasons('let s = "files up to 32 MiB can be shared";\n'), [])
        with self.subTest(phase="unit and name tables"):
            self.assertEqual(len(reasons('const U: [&str; 3] = ["B", "KiB", "MiB"];\n')), 1)
            self.assertEqual(len(reasons('const M: [&str; 2] = ["Jan", "Feb"];\n')), 1)

    def test_marks_comments_and_tests_are_exempt(self):
        with self.subTest(phase="a machine mark on the line or above it"):
            self.assertEqual(reasons('let s = format!("{h:02}:{m:02}"); // format: machine (SMPTE)\n'), [])
            self.assertEqual(
                reasons('// format: machine (a file name)\nlet s = at.format("%d.%m.%Y");\n'), []
            )
        with self.subTest(phase="a mark further away does not count"):
            text = "// format: machine\n" + "\n" * 6 + 'let s = format!("{h:02}:{m:02}");\n'
            self.assertEqual(len(reasons(text)), 1)
        with self.subTest(phase="comments"):
            self.assertEqual(reasons('// at.format("%b %d")\n'), [])
        with self.subTest(phase="test modules"):
            text = '#[cfg(test)]\nmod tests {\n    fn t() { let s = format!("{h:02}:{m:02}"); }\n}\n'
            self.assertEqual(reasons(text), [])

    def test_the_formatter_and_test_files_are_not_audited(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        offending = 'pub fn f() -> String { format!("{h:02}:{m:02}") }\n'
        for relative in (
            "crates/sdk/ui-core/src/format.rs",
            "crates/sdk/ui-core/src/format/dates.rs",
            "apps/x/src/tests.rs",
            "apps/x/src/view.rs",
        ):
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(offending, encoding="utf-8")
        flagged = [v.path for v in contract.audit(root)]
        self.assertEqual(flagged, ["apps/x/src/view.rs"])


if __name__ == "__main__":
    unittest.main()
