import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_text_contract.py"
SPEC = importlib.util.spec_from_file_location("check_app_text_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class AppTextContractTests(unittest.TestCase):
    def fixture(self, cargo: str, source: str) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        app = root / "apps" / "sample"
        (app / "src").mkdir(parents=True)
        (app / "Cargo.toml").write_text(cargo, encoding="utf-8")
        (app / "src" / "lib.rs").write_text(source, encoding="utf-8")
        return root

    def test_ui_text_surface_is_public_and_free_of_private_shaping_and_fonts(self):
        with self.subTest(phase='public ui text surface passes'):
            root = self.fixture(
                '[package]\nname="sample"\nversion="0.1.0"\n'
                '[dependencies]\nui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n',
                "use ui::text_render::TextRenderer;\n",
            )
            violations, apps, users = contract.audit(root)
            self.assertEqual(violations, [])
            self.assertEqual((apps, users), (1, 1))
        with self.subTest(phase='private shaper dependency and glyph paint fail'):
            root = self.fixture(
                '[package]\nname="sample"\nversion="0.1.0"\n'
                '[dependencies]\nui={package="tilo-app",path="../../crates/sdk/tilo-app"}\ncosmic-text="0.19"\n',
                "fn paint(scene: &mut vello::Scene) { scene.draw_glyphs(todo!()); }\n",
            )
            violations, _, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertIn("private text crate", reasons)
            self.assertIn("raw Vello glyph emission", reasons)
        with self.subTest(phase='product font literal fails'):
            root = self.fixture(
                '[package]\nname="sample"\nversion="0.1.0"\n'
                '[dependencies]\nui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n',
                'const FONT: &str = "Noto Serif";\n',
            )
            violations, _, _ = contract.audit(root)
            self.assertEqual(len(violations), 1)
            self.assertIn("font family literal", violations[0].reason)

    def test_only_a_windowless_package_may_go_without_ui(self):
        compute = (
            '[package]\nid="so.tilo.sample"\nkind="service"\n'
            '[entrypoint]\nbin="bin/sample-batch"\ntype="cli"\n'
            '[cli]\ncommands=["sample-batch"]\n'
        )
        cargo = '[package]\nname="sample"\nversion="0.1.0"\n[dependencies]\n'
        with self.subTest(phase='a compute package needs no ui'):
            root = self.fixture(cargo, "fn main() {}\n")
            (root / "apps" / "sample" / "tilo-app.toml").write_text(compute, encoding="utf-8")
            violations, _, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='a windowed app without ui fails'):
            root = self.fixture(cargo, "fn main() {}\n")
            (root / "apps" / "sample" / "tilo-app.toml").write_text(
                compute.replace('type="cli"', 'type="gui"'), encoding="utf-8"
            )
            violations, _, _ = contract.audit(root)
            self.assertIn("shared `ui` crate", "\n".join(item.reason for item in violations))


if __name__ == "__main__":
    unittest.main()
