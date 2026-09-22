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
    def write_manifest(self, body: str) -> Path:
        directory = Path(tempfile.mkdtemp())
        manifest = directory / "manifest.toml"
        manifest.write_text(body, encoding="utf-8")
        self.addCleanup(lambda: __import__("shutil").rmtree(directory))
        return manifest

    def test_reads_complete_certification_data(self) -> None:
        manifest = self.write_manifest(
            """[package]\nid = "so.tilo.example"\nname = "Example"\n
            [entrypoint]\nbin = "bin/example"\n
            [accessibility.atspi_acceptance]\nroot = "Example"\nrequired = ["Open"]\naction = "Open"\nafter_action = ["Close"]\n"""
        )
        scenario = CLIENT.scenario_from_manifest(manifest)
        self.assertEqual(scenario.app_id, "so.tilo.example")
        self.assertEqual(scenario.required, ("Open",))

    def test_rejects_unverifiable_action_data(self) -> None:
        manifest = self.write_manifest(
            """[package]\nid = "so.tilo.example"\nname = "Example"\n
            [entrypoint]\nbin = "bin/example"\n
            [accessibility.atspi_acceptance]\nroot = "Example"\nrequired = ["Open"]\naction = "Open"\nafter_action = []\n"""
        )
        with self.assertRaises(CLIENT.GateError):
            CLIENT.scenario_from_manifest(manifest)

    def test_name_matching_uses_an_atspi_string_value(self) -> None:
        tree = {"/node/1": "(< 'Settings' >,)", "/node/2": "(< 'Other' >,)"}
        self.assertEqual(CLIENT.AtspiClient.named(tree, "Settings"), ["/node/1"])


if __name__ == "__main__":
    unittest.main()
