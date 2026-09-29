import importlib.util
import shutil
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


POLICY = Path(__file__).resolve().parents[1] / "policy"
sys.path.insert(0, str(POLICY))
SPEC = importlib.util.spec_from_file_location(
    "check_app_capabilities", POLICY / "check_app_capabilities.py"
)
capabilities = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = capabilities
SPEC.loader.exec_module(capabilities)

REPO = Path(__file__).resolve().parents[2]
UI = 'ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }\n'


class AppCapabilitiesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write("Cargo.toml", '[workspace]\nmembers = ["apps/*"]\n')
        for relative in (capabilities.TABLE, capabilities.RULES, capabilities.TILO_CAPS):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / relative, target)

    def write(self, relative: str, text: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text), encoding="utf-8")

    def app(self, name: str, source: str, *, dependencies: str = UI, features: str = "",
            declared: str = "") -> None:
        self.write(
            f"apps/{name}/Cargo.toml",
            f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n{features}'
            f"[dependencies]\n{dependencies}",
        )
        self.write(
            f"apps/{name}/tilo-app.toml",
            f'[package]\nid = "so.tilo.{name}"\nversion = "0.1.0"\nkind = "app"\n\n{declared}',
        )
        self.write(f"apps/{name}/src/lib.rs", source)

    def audit(self):
        return {app.name: app for app in capabilities.audit(self.root)}

    def problems(self):
        table = capabilities.tomllib.loads((self.root / capabilities.TABLE).read_text())
        return capabilities.problems(
            capabilities.audit(self.root), capabilities.vocabulary(self.root), table
        )

    def test_dependencies_features_paths_and_methods_count_as_use(self):
        self.app(
            "maps",
            """\
            use ui::{location, time};
            pub fn open(files: &dyn Picker) { let _ = files.open_contents(); }
            pub fn share() { ui::portal::send_open_uri("https://x"); }
            pub fn save() { let _ = ui::portal::PickRequest::save("a", "b", "c"); }
            """,
            dependencies=UI + 'tilo-secrets = { path = "../../crates/platform/tilo-secrets" }\n'
            'tilo-net = { path = "../../crates/platform/tilo-net", features = ["http"] }\n',
            features='[features]\nsession = ["ui/mpris"]\n\n',
        )
        uses = set(self.audit()["maps"].uses)
        self.assertEqual(
            uses,
            {"location", "files.open", "open-uri", "files.save", "secrets", "network.http",
             "media.session"},
        )

    def test_test_code_and_dev_dependencies_do_not_count(self):
        self.app(
            "notes",
            """\
            pub fn run() {}
            #[cfg(test)]
            mod tests { fn t() { ui::location::current(); std::thread::spawn(|| {}); } }
            """,
            dependencies=UI,
        )
        self.write(
            "apps/notes/Cargo.toml",
            (self.root / "apps/notes/Cargo.toml").read_text()
            + '\n[dev-dependencies]\ntilo-capture = { path = "x" }\n',
        )
        self.assertEqual(self.audit()["notes"].uses, {})

    def test_an_undeclared_capability_fails_with_the_line_to_add(self):
        self.app("camera", "pub fn go() { let _ = tilo_capture::devices(); }\n")
        found = self.problems()
        self.assertEqual(len(found), 1)
        self.assertIn("uses `camera` without declaring it", found[0])
        self.assertIn("apps/camera/src/lib.rs:1", found[0])
        self.assertIn('add "camera" to [capabilities] optional', found[0])

    def test_declared_capabilities_pass_and_unknown_names_fail(self):
        self.app(
            "camera",
            "pub fn go() { let _ = tilo_capture::devices(); }\n",
            declared='[capabilities]\noptional = ["camera", "network.htp"]\n',
        )
        self.assertEqual(
            self.problems(), ["apps/camera/tilo-app.toml: `network.htp` is not a Tilo capability"]
        )

    def test_the_table_names_only_tilo_caps_capabilities(self):
        table = capabilities.tomllib.loads((REPO / capabilities.TABLE).read_text())
        self.assertLessEqual(set(table["uses"]), capabilities.vocabulary(REPO))


if __name__ == "__main__":
    unittest.main()
