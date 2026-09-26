import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("guest_ci", Path(__file__).resolve().parents[1] / "dev/run-development-windows-guest.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
ROOT = "registry.tilo.so/tilo/wine-tools@sha256:" + "a" * 64
PAIR = "registry.tilo.so/tilo/wine-paired@sha256:" + "b" * 64
SIGNED = "registry.tilo.so/tilo/tilo-development-wine@sha256:" + "c" * 64
SNAPSHOT = "registry.tilo.so/tilo/tilo-development-windows-guest-rpms@sha256:" + "d" * 64


class Inputs(unittest.TestCase):
    def test_snapshot_pairing(self):
        with self.subTest(phase="initial_pair"):
            result = MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "WINE_PAIR_REF": PAIR})
            self.assertEqual(result["WINE_PAIR_REF"], PAIR)
        with self.subTest(phase="snapshot_reuse_binds_signed_identity"):
            env = {"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "DEVELOPMENT_WINE_REF": SIGNED,
                   "DEVELOPMENT_WINE_KEY_SHA256": "e" * 64, "WINDOWS_GUEST_RPMS_REF": SNAPSHOT}
            self.assertEqual(MODULE.inputs(env)["WINDOWS_GUEST_RPMS_REF"], SNAPSHOT)
            del env["DEVELOPMENT_WINE_KEY_SHA256"]
            with self.assertRaises(ValueError):
                MODULE.inputs(env)
        with self.subTest(phase="rejects_resigning_for_old_snapshot"):
            with self.assertRaises(ValueError):
                MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT,
                               "WINE_PAIR_REF": PAIR, "WINDOWS_GUEST_RPMS_REF": SNAPSHOT})

    def test_source_rejections(self):
        with self.subTest(phase="rejects_conflicting_wine_sources"):
            with self.assertRaises(ValueError):
                MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ROOT, "WINE_PAIR_REF": PAIR,
                               "DEVELOPMENT_WINE_REF": SIGNED, "DEVELOPMENT_WINE_KEY_SHA256": "e" * 64})
        with self.subTest(phase="mutable_or_external_sources_rejected"):
            for ref in ("registry.tilo.so/tilo/wine-tools:latest", ROOT.replace("registry.tilo.so", "elsewhere.test")):
                with self.subTest(ref=ref), self.assertRaises(ValueError):
                    MODULE.inputs({"WINDOWS_GUEST_BUILDROOT_REF": ref, "WINE_PAIR_REF": PAIR})


if __name__ == "__main__":
    unittest.main()
