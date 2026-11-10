import importlib.util
import sys
import tomllib
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "ci/wasm_clippy_conf.py"
SPEC = importlib.util.spec_from_file_location("wasm_clippy_conf", SCRIPT)
conf = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = conf
SPEC.loader.exec_module(conf)


class WasmClippyConfTests(unittest.TestCase):
    def test_the_repository_configuration_gains_the_clock_reads(self) -> None:
        root = (conf.ROOT / "clippy.toml").read_text(encoding="utf-8")
        written = tomllib.loads(conf.wasm_config(root))
        kept = tomllib.loads(root)
        banned = [entry["path"] for entry in written["disallowed-methods"]]
        self.assertEqual(banned[: len(kept["disallowed-methods"])], [e["path"] for e in kept["disallowed-methods"]])
        for path in conf.CLOCK_READS:
            self.assertIn(path, banned)
        for key, value in kept.items():
            if key != "disallowed-methods":
                self.assertEqual(written[key], value)

    def test_a_configuration_without_banned_methods_is_refused(self) -> None:
        with self.assertRaises(SystemExit):
            conf.wasm_config("allow-unwrap-in-tests = true\n")


if __name__ == "__main__":
    unittest.main()
