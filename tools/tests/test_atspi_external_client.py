#!/usr/bin/env python3
"""Unit tests for the manifest-driven external AT-SPI client."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("atspi_external_client", ROOT / "tools/atspi_external_client.py")
assert SPEC and SPEC.loader
CLIENT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CLIENT
SPEC.loader.exec_module(CLIENT)


class ManifestScenarioTests(unittest.TestCase):
    def test_package_store_metadata_and_payload_are_separate(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work) / "pkgs"
            metadata = root / ("a" * 64 + ".pkg")
            payload = root / ("a" * 64)
            metadata.mkdir(parents=True)
            binary = payload / "bin/example"
            binary.parent.mkdir(parents=True)
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            (metadata / "manifest.toml").write_text(
                '[package]\nid="so.tilo.example"\nname="Example"\n'
                '[entrypoint]\nbin="bin/example"\n'
                '[accessibility.atspi_acceptance]\nroot="Example"\n'
                'required=["Open"]\naction="Open"\nafter_action=["Close"]\n'
            )
            rows = CLIENT.installed_scenarios(root)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0][1], binary)
            binary.unlink()
            with self.assertRaisesRegex(CLIENT.GateError, "certified binary is absent"):
                CLIENT.installed_scenarios(root)

    def write_manifest(self, body: str) -> Path:
        directory = Path(tempfile.mkdtemp())
        manifest = directory / "manifest.toml"
        manifest.write_text(body, encoding="utf-8")
        self.addCleanup(lambda: __import__("shutil").rmtree(directory))
        return manifest

    def test_manifest_scenarios_require_verifiable_atspi_data(self):
        with self.subTest(phase='reads complete certification data'):
            manifest = self.write_manifest(
                """[package]\nid = "so.tilo.example"\nname = "Example"\n
            [entrypoint]\nbin = "bin/example"\n
            [accessibility.atspi_acceptance]\nroot = "Example"\nrequired = ["Open"]\naction = "Open"\nafter_action = ["Close"]\n"""
            )
            scenario = CLIENT.scenario_from_manifest(manifest)
            self.assertEqual(scenario.app_id, "so.tilo.example")
            self.assertEqual(scenario.required, ("Open",))
        with self.subTest(phase='rejects unverifiable action data'):
            manifest = self.write_manifest(
                """[package]\nid = "so.tilo.example"\nname = "Example"\n
            [entrypoint]\nbin = "bin/example"\n
            [accessibility.atspi_acceptance]\nroot = "Example"\nrequired = ["Open"]\naction = "Open"\nafter_action = []\n"""
            )
            with self.assertRaises(CLIENT.GateError):
                CLIENT.scenario_from_manifest(manifest)
        with self.subTest(phase='name matching uses an atspi string value'):
            tree = {"/node/1": "(< 'Settings' >,)", "/node/2": "(< 'Other' >,)"}
            self.assertEqual(CLIENT.AtspiClient.named(tree, "Settings"), ["/node/1"])


if __name__ == "__main__":
    unittest.main()
