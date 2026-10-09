import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_settings_writes.py"
SPEC = importlib.util.spec_from_file_location("check_settings_writes", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class SettingsWriteContractTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def violations(self, files: dict[str, str]) -> list[tuple[str, int, str]]:
        return [(v.path, v.line, v.call) for v in contract.audit(self.fixture(files))]

    def test_string_literal_sections_are_refused_in_production_code(self):
        source = (
            "fn a() {\n"
            '    settings_store::update_section("input", values);\n'
            "}\n"
            "fn b(source: &dyn SettingsSource) {\n"
            "    source\n"
            "        .update_section_report(\n"
            '            "display_transaction",\n'
            "            values,\n"
            "        );\n"
            "}\n"
            "fn c() {\n"
            "    settings_store::update_section_at(\n"
            '        file.ok_or_else(|| Error::other("no, (storage)"))?,\n'
            '        "display_tools",\n'
            "        values,\n"
            "    );\n"
            "}\n"
        )
        self.assertEqual(
            self.violations({"crates/x/src/lib.rs": source}),
            [
                ("crates/x/src/lib.rs", 2, "update_section"),
                ("crates/x/src/lib.rs", 6, "update_section_report"),
                ("crates/x/src/lib.rs", 12, "update_section_at"),
            ],
        )

    def test_constants_keys_and_paths_with_literals_pass(self):
        source = (
            "const SECTION: &str = \"input\";\n"
            "fn a() {\n"
            "    settings_store::update_section(SECTION, values);\n"
            "    settings_store::update_section_at(Path::new(\"/x\"), Self::SECTION, values);\n"
            "    settings_store::hardware::TOUCHPAD_ENABLED.set(false);\n"
            "}\n"
            "pub fn update_section(name: &str, values: Map) {}\n"
            "// settings_store::update_section(\"input\", values);\n"
        )
        self.assertEqual(self.violations({"apps/y/src/main.rs": source}), [])

    def test_tests_and_the_settings_store_crate_are_exempt(self):
        literal = 'fn t() { settings_store::update_section("input", values); }\n'
        files = {
            "crates/system/settings-store/src/lib.rs": literal,
            "crates/x/tests/it.rs": literal,
            "crates/x/src/thing_tests.rs": literal,
            "crates/x/src/inline.rs": (
                "fn production() {}\n"
                "#[cfg(test)]\n"
                "mod tests {\n"
                "    #[test]\n"
                "    fn seeds() {\n"
                '        settings_store::update_section("input", values);\n'
                "    }\n"
                "}\n"
                "fn after() {\n"
                '    settings_store::update_section("display", values);\n'
                "}\n"
            ),
            "crates/x/src/test_fn.rs": (
                "#[test]\n"
                "fn seeds() {\n"
                '    settings_store::update_section("input", values);\n'
                "}\n"
            ),
        }
        self.assertEqual(
            self.violations(files),
            [("crates/x/src/inline.rs", 10, "update_section")],
            "only production code after the test module is audited",
        )


if __name__ == "__main__":
    unittest.main()
