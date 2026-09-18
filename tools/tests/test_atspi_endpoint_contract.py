import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_atspi_endpoint_contract.py"
SPEC = importlib.util.spec_from_file_location("check_atspi_endpoint_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(contract)


class AtspiEndpointContractTests(unittest.TestCase):
    def audit_update(self, update):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            files = {
                "ui/Cargo.toml": 'accesskit_winit = "0.34.0"',
                "ui/src/app.rs": "\n".join([
                    "accesskit_winit::Adapter::with_event_loop_proxy",
                    "self.app.dispatch_a11y_action(req.target_node, &action)",
                    "self.a11y_adapter = None;",
                    update,
                ]),
                "ui/src/accesskit_bridge.rs": "\n".join([
                    "ak.set_live(match politeness",
                    "ak.set_live_atomic()",
                    "if !access.state.protected",
                ]),
                "shell/src/compositor/accessibility.rs": "\n".join([
                    "accesskit_unix::Adapter",
                    "self.rx.try_iter()",
                    "self.adapter.update_if_active",
                ]),
            }
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

    def test_disconnected_tree_build_does_not_satisfy_update_contract(self):
        self.assertIn("ui/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
            "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
            "adapter.update_if_active(|| unrelated_tree);"
        ))

    def test_dropping_built_tree_does_not_satisfy_update_contract(self):
        self.assertIn("ui/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
            "adapter.update_if_active(|| {\n"
            "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
            "unrelated_tree\n});"
        ))


if __name__ == "__main__":
    unittest.main()
