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


class SettingsNetworkTests(unittest.TestCase):
    DEVICES = "ethernet:connected:Wired connection 1\nwifi:connected:caf\\:e\nloopback:connected (externally):lo\n"

    def test_nmcli_rows_become_what_settings_must_show(self):
        expected = CLIENT.network_expectation(self.DEVICES, "full\n")
        self.assertEqual(expected.connectivity, "Connected to the internet")
        self.assertEqual(expected.wifi, "Connected to caf:e")
        self.assertTrue(expected.wired)
        with self.assertRaisesRegex(CLIENT.GateError, "no connected Wi-Fi or wired link"):
            CLIENT.network_expectation("wifi:disconnected:--\n", "none")
        with self.assertRaisesRegex(CLIENT.GateError, "unknown connectivity"):
            CLIENT.network_expectation(self.DEVICES, "sideways")

    def test_a_pane_showing_a_false_state_or_an_error_fails(self):
        expected = CLIENT.network_expectation(self.DEVICES, "full")
        good = ["('Connected to caf:e',)", "('Connected · 1000 Mb/s',)", "('Connected to the internet',)"]
        self.assertEqual(CLIENT.network_mismatches(good, expected), [])
        stale = ["('Not connected',)", "('Connectivity not checked',)"]
        problems = CLIENT.network_mismatches(stale, expected)
        self.assertEqual(len(problems), 3, problems)
        failed = good + ["('Can\u2019t read network status: nmcli not installed',)", "(\"Can't check connectivity: x\",)"]
        self.assertTrue(CLIENT.network_mismatches(failed, expected))


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
