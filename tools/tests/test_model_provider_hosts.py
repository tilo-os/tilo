"""Guard the portable provider control panel's registration and host boundary."""
from pathlib import Path
import json
import re
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/web"))
import generate_web_desktop as registry


class ModelProviderHosts(unittest.TestCase):
    def test_registered_asset_is_portable_and_settings_remains_os_only(self):
        apps = {app["id"]: app for app in registry.all_apps(ROOT)}
        providers = apps["so.tilo.model-providers"]
        self.assertEqual(set(providers["platforms"]), {"tilo-os", "linux", "macos", "web"})
        self.assertEqual(apps["so.tilo.settings"]["platforms"], ["tilo-os"])
        manifest = tomllib.loads((ROOT / "apps/model-providers/tilo-app.toml").read_text())
        self.assertEqual(manifest["sandbox"]["services"], {"model-gateway": "optional@2"})
        self.assertEqual(manifest["sandbox"]["net"]["mode"], "off")
        self.assertTrue(providers["keywords"])
        published = json.loads((ROOT / "crates/browser/tilo-web-desktop/apps.json").read_text())
        self.assertIn("so.tilo.model-providers", json.dumps(published))
        for path, registration in [
            ("crates/browser/tilo-web-desktop/src/apps.rs", 'registry::defer("so.tilo.model-providers"'),
            ("crates/agent/agent-participant/src/web_apps.rs", "model_providers::definition()"),
        ]:
            self.assertIn(registration, (ROOT / path).read_text())

    def test_shared_panel_is_an_sdk_surface_with_native_only_workspace_hack(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
        panel = "crates/sdk/provider-settings-ui"
        self.assertIn(panel, workspace["workspace"]["members"])
        self.assertNotIn("crates/agent/provider-settings-ui", workspace["workspace"]["members"])
        for app in ("settings", "model-providers"):
            manifest = tomllib.loads((ROOT / f"apps/{app}/Cargo.toml").read_text())
            dependency = manifest["dependencies"]["provider-settings-ui"]
            self.assertEqual((ROOT / f"apps/{app}" / dependency["path"]).resolve(), ROOT / panel)
        manifest = tomllib.loads((ROOT / panel / "Cargo.toml").read_text())
        self.assertEqual(manifest["package"]["name"], "provider-settings-ui")
        self.assertNotIn("workspace-hack", manifest["dependencies"])
        self.assertIn("workspace-hack", manifest["target"]['cfg(not(target_arch = "wasm32"))']["dependencies"])
        app = tomllib.loads((ROOT / "apps/model-providers/Cargo.toml").read_text())
        self.assertEqual(app["features"]["thin"], ["dep:runtime-dylib"])
        self.assertTrue(app["dependencies"]["runtime-dylib"]["optional"])

    def test_browser_administration_uses_exact_registered_identity(self):
        base = ROOT / "crates/browser/tilo-browser-runtime/src"
        services = (base / "host_services.rs").read_text()
        helper = services.split("fn provider_admin_app(", 1)[1].split("\n}", 1)[0]
        self.assertEqual(re.findall(r'"(so\.tilo\.[^"]+)"', helper),
                         ["so.tilo.settings", "so.tilo.model-providers"])
        self.assertIn("handle.is_available() && !provider_admin_app(context.app_id())", services)
        desktop = (base / "desktop.rs").read_text()
        self.assertIn("if crate::host_services::provider_admin_app(definition.app_id)", desktop)
        self.assertIn("self.inputs.provider_admin.clone()", desktop)
        browser = (base / "browser.rs").read_text()
        self.assertIn("provide_provider_admin(&mut context, config.provider_admin()?)", browser)
        config = browser.split("fn provider_admin(", 1)[1].split("\n    fn models(", 1)[0]
        self.assertIn("self.provider_management", config)
        self.assertNotIn("self.models", config)
        self.assertNotIn("session_token", helper)
        self.assertNotIn("starts_with", helper)

    def test_gateway_maps_only_exact_control_panel_ids(self):
        source = (ROOT / "crates/agent/model-gateway/src/caller.rs").read_text()
        helper = source.split("fn provider_admin_app(", 1)[1].split("\n}", 1)[0]
        self.assertEqual(re.findall(r'"(so\.tilo\.[^"]+)"', helper),
                         ["so.tilo.settings", "so.tilo.model-providers"])
        self.assertNotIn("starts_with", helper)


if __name__ == "__main__":
    unittest.main()
