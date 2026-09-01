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
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
            " fn local_resource_operation_handlers(&self) -> &'static [&'static str] { &[\"app.open\"] }\n"
            " fn invoke_resource_operation(&mut self, type_id: &str) { match type_id { \"app.open\" => open(), _ => {} } } }\n"
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

    def test_effect_action_without_handler_registry_fails(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
            " fn invoke_resource_operation(&mut self) {} }\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("handler registry" in item.reason for item in violations))

    def test_legacy_room_entry_point_fails(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "fn resources(&self) {}\nfn apply_room_action(&mut self) {}\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("legacy room-action" in item.reason for item in violations))

    def test_self_attested_handler_registry_fails(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
            " fn local_resource_operation_handlers(&self) -> &'static [&'static str] { &[\"app.open\"] }\n"
            " fn invoke_resource_operation(&mut self) { Ok(false) } }\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("not reachable" in item.reason for item in violations))

    def test_comments_and_empty_provider_do_not_satisfy_contract(self):
        root = self.fixture(
            "// ActionSpec::operation Resource::new\n"
            "impl Widget for App { fn resources(&self) -> Vec<Resource> { vec![] } }\n"
        )
        violations, _ = contract.audit(root)
        reasons = "\n".join(item.reason for item in violations)
        self.assertIn("ActionSpec", reasons)
        self.assertIn("empty provider", reasons)


if __name__ == "__main__":
    unittest.main()
