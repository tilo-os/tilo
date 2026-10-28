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
    def test_shared_factory_retains_both_adapters_before_new_windows_are_shown(self):
        source = (SCRIPT.parents[2] / "crates/sdk/ui-core/src/app/native_runner.rs").read_text()
        self.assertEqual(contract.adapter_registration_errors(source), [])
        registration = "create_accessibility_adapter(event_loop, &window, self.a11y_proxy.clone());"
        self.assertEqual(source.count(registration), 2)
        for occurrence in (0, 1):
            with self.subTest(view=occurrence):
                parts = source.split(registration)
                mutated = registration.join(parts[:occurrence + 1]) + "None;" + registration.join(parts[occurrence + 1:])
                self.assertTrue(contract.adapter_registration_errors(mutated))
        with self.subTest(phase="unconnected constructor is not a provider"):
            self.assertTrue(contract.adapter_registration_errors(source.replace(
                "Some(PrimaryA11yAdapter::with_event_loop_proxy(",
                "drop(PrimaryA11yAdapter::with_event_loop_proxy(",
            )))
        for name in ("resumed", "create_native_floating_window"):
            with self.subTest(phase="registration after visibility", view=name):
                body = contract.function_body(source, name)
                self.assertIsNotNone(body)
                delayed = body.replace(registration, "None;", 1)
                creation = delayed.find(".create_window(")
                show = delayed.find("window.set_visible(true);", creation)
                self.assertGreater(show, creation)
                delayed = delayed[:show] + delayed[show:].replace(
                    "window.set_visible(true);",
                    "window.set_visible(true);\n" + ("self.a11y_adapter = " if name == "resumed" else "let a11y_adapter = ") + registration,
                    1,
                )
                self.assertTrue(contract.adapter_registration_errors(source.replace(body, delayed)))

    def test_the_client_reaches_the_guest_through_the_harness_and_never_ships(self):
        repository = SCRIPT.parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("tools/atspi_external_client.py", "os/dev/vm.sh", "os/Containerfile"):
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text((repository / name).read_text(encoding="utf-8"), encoding="utf-8")
            self.assertEqual(contract.external_client_errors(root), [])
            containerfile = root / "os/Containerfile"
            original = containerfile.read_text(encoding="utf-8")
            containerfile.write_text(
                original + "COPY tools/atspi_external_client.py /usr/libexec/tilo/atspi-external-client.py\n",
                encoding="utf-8")
            self.assertEqual(contract.external_client_errors(root), [
                "os/Containerfile: the image ships the external AT-SPI client, which is test tooling"])
            containerfile.write_text(original, encoding="utf-8")
            harness = root / "os/dev/vm.sh"
            harness.write_text(harness.read_text(encoding="utf-8").replace(
                '"tools/atspi_external_client.py ', '"tools/other.py '), encoding="utf-8")
            self.assertEqual(contract.external_client_errors(root), [
                "os/dev/vm.sh: the VM harness does not deliver the external AT-SPI client into the guest"])

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
                "os/dev/vm.sh",
                "os/Containerfile",
            ):
                files[name] = (repository / name).read_text(encoding="utf-8")
            for name, source in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")
            return contract.audit(root)

    def test_atspi_endpoint_contract_requires_a_default_connected_native_update_path(self):
        with self.subTest(phase='native update reaches adapter with both rustfmt shapes'):
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
        with self.subTest(phase='native host extraction preserves endpoint contract'):
            self.assertEqual(self.audit_update(
                "adapter.update_if_active(|| {\n"
                "crate::accesskit_bridge::native_tree_update(\n"
                "    app_ref.build_a11y_tree_update(scale),\n"
                ")\n"
                "});",
                source="crates/sdk/ui-core/src/app/host.rs",
            ), [])
        with self.subTest(phase='default native runner feature preserves endpoint contract'):
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
        with self.subTest(phase='optional adapter outside default features is rejected'):
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
        with self.subTest(phase="a window of its own's tree carries the app's shared tree"):
            self.assertEqual(self.audit_update(
                "adapter.update_if_active(|| slot_bar::window_tree_update(app_ref, bar, scale));\n"
                + self.WINDOW_TREE
            ), [])
        with self.subTest(phase="a window tree without the app's shared tree is rejected"):
            self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
                "adapter.update_if_active(|| slot_bar::window_tree_update(app_ref, bar, scale));\n"
                + self.WINDOW_TREE.replace("app.build_a11y_tree_update(scale)", "accesskit::TreeUpdate::default()")
            ))
        with self.subTest(phase="a window tree that skips the native conversion is rejected"):
            self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
                "adapter.update_if_active(|| slot_bar::window_tree_update(app_ref, bar, scale));\n"
                + self.WINDOW_TREE.replace("crate::accesskit_bridge::native_tree_update(update)", "update")
            ))
        with self.subTest(phase="a window tree that never reaches the adapter is rejected"):
            self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
                "adapter.update_if_active(|| unrelated_tree);\n" + self.WINDOW_TREE
            ))
        with self.subTest(phase='disconnected tree build does not satisfy update contract'):
            self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
                "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
                "adapter.update_if_active(|| unrelated_tree);"
            ))
        with self.subTest(phase='dropping built tree does not satisfy update contract'):
            self.assertIn("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates", self.audit_update(
                "adapter.update_if_active(|| {\n"
                "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale));\n"
                "unrelated_tree\n});"
            ))

    WINDOW_TREE = (
        "pub(super) fn window_tree_update(\n"
        "    app: &mut App,\n"
        "    bar: &SlotBar,\n"
        "    scale: f64,\n"
        ") -> accesskit::TreeUpdate {\n"
        "    let mut update = app.build_a11y_tree_update(scale);\n"
        "    if let Some(row) = bar.accessibility(app) {\n"
        "        crate::accesskit_bridge::publish_host_part(&mut update, &row, scale);\n"
        "    }\n"
        "    crate::accesskit_bridge::native_tree_update(update)\n"
        "}\n"
    )

    NATIVE_UPDATE = (
        "adapter.update_if_active(|| {\n"
        "crate::accesskit_bridge::native_tree_update(app_ref.build_a11y_tree_update(scale))\n"
        "});"
    )


if __name__ == "__main__":
    unittest.main()
