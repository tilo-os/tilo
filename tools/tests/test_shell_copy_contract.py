from pathlib import Path
import importlib.util
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_shell_copy_contract.py"
SPEC = importlib.util.spec_from_file_location("check_shell_copy_contract", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ShellCopyContractTests(unittest.TestCase):
    def test_public_shell_copy_uses_the_product_vocabulary(self) -> None:
        self.assertEqual(MODULE.audit(), [])


if __name__ == "__main__":
    unittest.main()
