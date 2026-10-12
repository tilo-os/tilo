"""The unchanged acceptance producer must reject missing native facts."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "layer_artifacts", ROOT / "scripts/vm-acceptance/layer_artifacts.py")
artifacts = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(artifacts)


class NativeFactsGuard(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / "target/native-layer-facts-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.stream = Path(self.temp.name) / "apps-runtime.stream"
        self.packed = dict(component="apps+runtime", target="x86_64-unknown-linux-gnu",
                           toolchain="rustc fixture", source_digest="a" * 64,
                           layer_hash="b" * 64, reused=False, binary=False)

    def write(self, values):
        self.stream.write_text("tilo-vm-layer-begin " + json.dumps(values) + "\n"
                               "tilo-vm-layer-data YQ==\n"
                               "tilo-vm-layer-end " + hashlib.sha256(b"a").hexdigest() + " 1\n")

    def test_complete_observed_native_facts_are_accepted(self):
        self.write(self.packed)
        self.assertEqual(artifacts.header(self.stream), self.packed)

    def test_c_only_missing_or_wrong_architecture_facts_remain_rejected(self):
        for changed in ({"toolchain": None}, {"toolchain": ""},
                        {"target": "aarch64-unknown-linux-gnu"}, {"target": None}):
            self.write(dict(self.packed, **changed))
            with self.subTest(changed=changed), self.assertRaisesRegex(
                    ValueError, "missing or mismatched native build facts"):
                artifacts.header(self.stream)


if __name__ == "__main__":
    unittest.main()
