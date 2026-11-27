import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_control_binding.py"
SPEC = importlib.util.spec_from_file_location("check_control_binding", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)

APP_BINDING_ITSELF = """
use tilo_ui::widget::BoundAction;

fn send() -> BoundAction<String> {
    BoundAction::from_tilo_ui_binding(address(), None, |text| Ok(invocation(text)))
}
"""


class ControlBindingContractTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def violations(self, files: dict[str, str]) -> list[tuple[str, int]]:
        return [(v.path, v.line) for v in contract.audit(self.fixture(files))]

    def test_an_app_or_sdk_crate_cannot_build_a_bound_action(self):
        self.assertEqual(
            self.violations(
                {
                    "apps/notes/src/send.rs": APP_BINDING_ITSELF,
                    "crates/sdk/tilo-app/tests/send.rs": APP_BINDING_ITSELF,
                }
            ),
            [("crates/sdk/tilo-app/tests/send.rs", 5), ("apps/notes/src/send.rs", 5)],
        )

    def test_tilo_ui_binds_and_the_base_defines(self):
        self.assertEqual(
            self.violations(
                {
                    "crates/ui/ui-core/src/bind.rs": APP_BINDING_ITSELF,
                    "crates/ui/ui-base/src/widget/action_emitter.rs": "pub fn from_tilo_ui_binding() {}",
                    "apps/notes/src/lib.rs": "let bound = tilo_ui::Bind::bound(binding);",
                }
            ),
            [],
        )

    def test_the_tree_holds_the_contract(self):
        self.assertEqual(contract.audit(), [])


if __name__ == "__main__":
    unittest.main()
