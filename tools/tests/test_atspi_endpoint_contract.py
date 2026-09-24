import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_atspi_endpoint_contract.py"
SPEC = importlib.util.spec_from_file_location("check_atspi_endpoint_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(contract)


class AtspiEndpointContractTests(unittest.TestCase):
    def audit_update(self, update, *, source="crates/sdk/ui-core/src/app.rs", cargo='accesskit_winit = "0.34.0"'):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            files = {
                "crates/sdk/ui-core/Cargo.toml": cargo,
                source: "\n".join([
                    "accesskit_winit::Adapter::with_event_loop_proxy",
                    "self.app.dispatch_a11y_action(req.target_node, &action)",
                    "self.a11y_adapter = None;",
                    update,
                ]),
                "crates/sdk/ui-core/src/accesskit_bridge.rs": "\n".join([
                    "ak.set_live(match politeness",
                    "ak.set_live_atomic()",
                    "if !access.state.protected",
                ]),
                "crates/session/shell/src/compositor/accessibility.rs": "\n".join([
                    "accesskit_unix::Adapter",
                    "self.rx.try_iter()",
                    "wake.ping()",
                    "self.adapter.update_if_active",
                ]),
            }
            # Keep the unrelated native lifecycle and acceptance contracts valid
            # while each test varies only the shared tree-update expression.
            files["crates/session/shell/src/compositor/native.rs"] = "\n".join([
                "drop(native_accessibility);",
                "drop(display);",
            ])
            repository = SCRIPT.parents[2]
            for name in (
                "apps/file-explorer/tilo-app.toml",
                "apps/photos/tilo-app.toml",
                "apps/settings/tilo-app.toml",
                "tools/atspi_external_client.py",
            ):
                files[name] = (repository / name).read_text(encoding="utf-8")
            for name, source in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")
            return contract.audit(root)

    def test_native_update_reaches_adapter_with_both_rustfmt_shapes(self):
        for argument in (
            "app_ref.build_a11y_tree_update(scale)",
            "\n    app_ref.build_a11y_tree_update(scale),\n",
        ):
            with self.subTest(argument=argument):
                self.assertEqual(self.audit_update(
                    "adapter.update_if_active(|| {\n"
                    f"crate::accesskit_bridge::native_tree_update({argument})\n"
                    "});"
                ), [])

    def test_native_host_extraction_preserves_endpoint_contract(self):
        self.assertEqual(self.audit_update(
            "adapter.update_if_active(|| {\n"
            "crate::accesskit_bridge::native_tree_update(\n"
            "    app_ref.build_a11y_tree_update(scale),\n"
            ")\n"
            "});",
            source="crates/sdk/ui-core/src/app/host.rs",
        ), [])

    NATIVE_UPDATE = (
        "adapter.update_if_active(|| {\n"
        "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale))\n"
        "});"
    )

    def test_default_native_runner_feature_preserves_endpoint_contract(self):
        self.assertEqual(self.audit_update(
            self.NATIVE_UPDATE,
            source="crates/sdk/ui-core/src/app/native_runner.rs",
            cargo="\n".join([
                "[features]",
                'default = ["native"]',
                'native = ["dep:accesskit_winit"]',
                "[dependencies]",
                'accesskit_winit = { version = "0.34.0", optional = true }',
            ]),
        ), [])

    def test_optional_adapter_outside_default_features_is_rejected(self):
        self.assertIn(
            "crates/sdk/ui-core/Cargo.toml: AccessKit winit adapter is not enabled by default",
            self.audit_update(
                self.NATIVE_UPDATE,
                cargo="\n".join([
                    "[features]",
                    "default = []",
                    'native = ["dep:accesskit_winit"]',
                    "[dependencies]",
                    'accesskit_winit = { version = "0.34.0", optional = true }',
                ]),
            ),
        )

    def test_disconnected_tree_build_does_not_satisfy_update_contract(self):
        self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
            "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
            "adapter.update_if_active(|| unrelated_tree);"
        ))

    def test_dropping_built_tree_does_not_satisfy_update_contract(self):
        self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
            "adapter.update_if_active(|| {\n"
            "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
            "unrelated_tree\n});"
        ))


if __name__ == "__main__":
    unittest.main()
