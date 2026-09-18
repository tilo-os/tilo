import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("guest_ci", Path(__file__).resolve().parents[1] / "dev/run-development-windows-guest.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
ROOT = "code.podesta.ai/qos/wine-tools@sha256:" + "a" * 64
PAIR = "code.podesta.ai/qos/wine-paired@sha256:" + "b" * 64
SIGNED = "code.podesta.ai/qos/tilo-development-wine@sha256:" + "c" * 64
SNAPSHOT = "code.podesta.ai/qos/tilo-development-windows-guest-rpms@sha256:" + "d" * 64


class Inputs(unittest.TestCase):
    def test_initial_pair(self):
        result = MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "WINE_PAIR_REF": PAIR})
        self.assertEqual(result["WINE_PAIR_REF"], PAIR)

    def test_snapshot_reuse_binds_signed_identity(self):
        env = {"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "DEVELOPMENT_WINE_REF": SIGNED,
               "DEVELOPMENT_WINE_KEY_SHA256": "e" * 64, "WINDOWS_GUEST_RPMS_REF": SNAPSHOT}
        self.assertEqual(MODULE.inputs(env)["WINDOWS_GUEST_RPMS_REF"], SNAPSHOT)
        del env["DEVELOPMENT_WINE_KEY_SHA256"]
        with self.assertRaises(ValueError):
            MODULE.inputs(env)

    def test_rejects_resigning_for_old_snapshot(self):
        with self.assertRaises(ValueError):
            MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT,
                           "WINE_PAIR_REF": PAIR, "WINDOWS_GUEST_RPMS_REF": SNAPSHOT})

    def test_rejects_conflicting_wine_sources(self):
        with self.assertRaises(ValueError):
            MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "WINE_PAIR_REF": PAIR,
                           "DEVELOPMENT_WINE_REF": SIGNED, "DEVELOPMENT_WINE_KEY_SHA256": "e" * 64})

    def test_mutable_or_external_sources_rejected(self):
        for ref in ("code.podesta.ai/qos/wine-tools:latest", ROOT.replace("code.podesta.ai", "elsewhere.test")):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ref, "WINE_PAIR_REF": PAIR})


if __name__ == "__main__":
    unittest.main()
