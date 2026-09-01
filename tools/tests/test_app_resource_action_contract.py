import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_app_resource_action_contract.py"
SPEC = importlib.util.spec_from_file_location("check_app_resource_action_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class AppResourceActionContractTests(unittest.TestCase):
    def fixture(self, source: str, *, dependencies: str = 'qos-actions="1"\n') -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        app = root / "apps" / "sample"
        (app / "src").mkdir(parents=True)
        (app / "Cargo.toml").write_text(
            '[package]\nname="sample"\nversion="0.1.0"\n[dependencies]\n'
            + dependencies,
            encoding="utf-8",
        )
        (app / "src" / "lib.rs").write_text(source, encoding="utf-8")
        return root

    def test_resource_action_app_passes(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> { vec![] } }\n"
        )
        violations, apps = contract.audit(root)
        self.assertEqual(violations, [])
        self.assertEqual(apps, 1)

    def test_missing_protocol_and_surface_fail(self):
        root = self.fixture("struct App;\n", dependencies="")
        violations, _ = contract.audit(root)
        reasons = "\n".join(item.reason for item in violations)
        self.assertIn("does not depend", reasons)
        self.assertIn("Widget::resources", reasons)
        self.assertIn("ActionSpec", reasons)

    def test_legacy_room_entry_point_fails(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "fn resources(&self) {}\nfn apply_room_action(&mut self) {}\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("legacy room-action" in item.reason for item in violations))


if __name__ == "__main__":
    unittest.main()
