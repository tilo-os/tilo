import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("toolkit_closure", ROOT / "tools/sdk/check_toolkit_closure.py")
toolkit_closure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(toolkit_closure)


class ToolkitClosure(unittest.TestCase):
    def test_only_path_packages_count_and_the_root_is_kept_out_by_the_caller(self):
        output = "\n".join([
            "tilo-ui v0.1.0 (/checkout/crates/ui/ui-core)",
            "serde v1.0.0",
            "tilo-ui-paint v0.1.0 (/checkout/crates/ui/ui-paint)",
            "tilo-ui-paint v0.1.0 (/checkout/crates/ui/ui-paint) (*)",
        ])
        self.assertEqual(toolkit_closure.local_crates(output), {"tilo-ui", "tilo-ui-paint"})

    def test_a_crate_the_list_does_not_name_fails(self):
        problems = toolkit_closure.findings({"tilo-ui-paint", "tilo-resource"}, set(), {"tilo-ui-paint"}, set())
        self.assertEqual(len(problems), 1)
        self.assertIn("tilo-resource", problems[0])

    def test_a_listed_crate_no_longer_compiled_fails_so_the_list_shrinks(self):
        problems = toolkit_closure.findings({"tilo-ui-paint"}, set(), {"tilo-ui-paint", "tilo-mime"}, set())
        self.assertEqual(len(problems), 1)
        self.assertIn("tilo-mime", problems[0])

    def test_the_model_in_the_default_build_fails(self):
        problems = toolkit_closure.findings(
            {"tilo-ui-paint"}, {"tilo-ui-paint", "tilo-collaboration"}, {"tilo-ui-paint"}, {"tilo-collaboration"}
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("tilo-collaboration", problems[0])

    def test_the_exact_list_passes(self):
        self.assertEqual(
            toolkit_closure.findings({"tilo-ui-paint"}, {"tilo-ui-paint"}, {"tilo-ui-paint"}, {"tilo-resource"}),
            [],
        )


if __name__ == "__main__":
    unittest.main()
