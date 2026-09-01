from pathlib import Path
import importlib.util


SCRIPT = Path(__file__).resolve().parents[1] / "check_shell_copy_contract.py"
SPEC = importlib.util.spec_from_file_location("check_shell_copy_contract", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_public_shell_copy_uses_the_product_vocabulary() -> None:
    assert MODULE.audit() == []
