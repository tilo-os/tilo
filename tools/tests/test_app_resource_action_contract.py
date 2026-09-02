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
    def fixture(
        self,
        source: str,
        *,
        dependencies: str = 'ui={package="qos-app",path="../../crates/app/qos-app"}\n',
        app_name: str = "sample",
    ) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        app = root / "apps" / app_name
        (app / "src").mkdir(parents=True)
        (app / "Cargo.toml").write_text(
            f'[package]\nname="{app_name}"\nversion="0.1.0"\n[dependencies]\n'
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
        self.assertIn("qos-app", reasons)
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

    def test_agent_must_use_generic_broker_catalogue_and_receipts(self):
        root = self.fixture(
            "fn resources() {}\nstruct ActionDescriptor;\n",
            app_name="agent",
        )
        violations, _ = contract.audit(root)
        reasons = "\n".join(item.reason for item in violations)
        self.assertIn("generic action catalogue", reasons)
        self.assertIn("invoke and observe", reasons)

    def test_generic_broker_agent_passes(self):
        root = self.fixture(
            "fn resources() {}\n"
            "struct ActionDescriptor; struct BrokerActionSurface;\n"
            "enum CollabRequest { InvokePublishedAction, PublishedInvocationStatus }\n",
            app_name="agent",
        )
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

    def test_direct_ui_core_dependency_fails_even_when_renamed(self):
        root = self.fixture(
            "fn resources() {}\nstruct ActionDescriptor; struct BrokerActionSurface;\n"
            "enum CollabRequest { InvokePublishedAction, PublishedInvocationStatus }\n",
            dependencies=(
                'ui={package="qos-app",path="../../crates/app/qos-app"}\n'
                'ui_core={package="ui",path="../../ui"}\n'
            ),
            app_name="agent",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))

    def test_direct_ui_core_path_fails_without_explicit_package_name(self):
        root = self.fixture(
            "fn resources() {}\nstruct ActionDescriptor; struct BrokerActionSurface;\n"
            "enum CollabRequest { InvokePublishedAction, PublishedInvocationStatus }\n",
            dependencies=(
                'ui={package="qos-app",path="../../crates/app/qos-app"}\n'
                'rendering={path="../../ui"}\n'
            ),
            app_name="agent",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))

    def test_target_specific_facades_are_checked(self):
        root = self.fixture(
            "fn resources() {}\nstruct ActionDescriptor; struct BrokerActionSurface;\n"
            "enum CollabRequest { InvokePublishedAction, PublishedInvocationStatus }\n",
            dependencies="",
            app_name="agent",
        )
        manifest = root / "apps" / "agent" / "Cargo.toml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "\n[target.'cfg(unix)'.dependencies]\n"
            + 'ui={package="qos-app",path="../../crates/app/qos-app"}\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "\n[target.'cfg(windows)'.dependencies]\n"
            + 'ui_core={package="ui",path="../../ui"}\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))

    def test_toolkit_resource_action_host_needs_no_copied_widget_boilerplate(self):
        root = self.fixture(
            "pub fn app_root() {\n"
            " let resource = ui::widget::resource_actions::typed_resource();\n"
            " ui::widget::resource_actions::immediate(root, resource, |_| {});\n"
            "}\n"
        )
        violations, apps = contract.audit(root)
        self.assertEqual(violations, [])
        self.assertEqual(apps, 1)

    def test_toolkit_host_without_resource_descriptor_fails(self):
        root = self.fixture(
            "pub fn app_root() {\n"
            " ui::widget::resource_actions::immediate(root, resource, |_| {});\n"
            "}\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("no toolkit resource descriptor" in item.reason for item in violations))

    def test_new_raw_callback_exceeds_zero_default_ratchet(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            "fn view() { Button::new().on_click(|| mutate_domain()); }\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("raw on_click closure" in item.reason for item in violations))

    def test_audited_callback_ceiling_is_reduce_only(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            "fn view() { Button::new().on_click(|| mutate_domain()); }\n"
        )
        tools = root / "tools"
        tools.mkdir()
        (tools / contract.BASELINE).write_text(
            '[ratchets.raw_on_click_closure]\n"apps/sample/src/lib.rs"=1\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

        app = root / "apps" / "sample" / "src" / "lib.rs"
        app.write_text(
            app.read_text(encoding="utf-8")
            + "fn second() { Button::new().on_click(move || mutate_more()); }\n",
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("audited ceiling 1" in item.reason for item in violations))

    def test_reducing_debt_requires_lowering_the_audited_ceiling(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
            " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
            " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
        )
        tools = root / "tools"
        tools.mkdir()
        (tools / contract.BASELINE).write_text(
            '[ratchets.raw_on_click_closure]\n"apps/sample/src/lib.rs"=1\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("stale raw_on_click_closure ceiling" in item.reason for item in violations))

    def test_new_bespoke_automation_entry_point_fails(self):
        root = self.fixture(
            "use qos_actions::ActionSpec;\n"
            "fn resources(&self) {}\n"
            "fn automation_invoke() {}\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any("bespoke automation" in item.reason for item in violations))


if __name__ == "__main__":
    unittest.main()
