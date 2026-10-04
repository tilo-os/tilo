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


class TypedTextTests(unittest.TestCase):
    def test_text_replies_read_back_exactly(self):
        self.assertEqual(CLIENT.text_reply("('hi',)\n"), "hi")
        self.assertEqual(CLIENT.text_reply("('',)"), "")
        self.assertEqual(CLIENT.text_reply("('it\\'s, twice!',)"), "it's, twice!")
        self.assertIsNone(CLIENT.text_reply("Error: GDBus.Error"))


class LaunchpadCardTests(unittest.TestCase):
    def run_card(self, opened_root: str) -> tuple[str, int]:
        from unittest import mock

        shell = mock.Mock()
        shell.call.return_value = "(true,)"
        with mock.patch.object(CLIENT, "shell_client", return_value=shell), \
                mock.patch.object(CLIENT, "bus_pids", return_value={":1.1": 10}), \
                mock.patch.object(CLIENT, "wait_for_shell_node", return_value=({}, "/card")) as node, \
                mock.patch.object(CLIENT, "wait_for_new_app", return_value=(":1.9", 99)) as new_app, \
                mock.patch.object(CLIENT.AtspiClient, "property", return_value=f"(<'{opened_root}'>,)"), \
                mock.patch.object(CLIENT, "process_command", return_value="/usr/bin/tilo-launcher"), \
                mock.patch.object(CLIENT, "close_app"):
            opened = CLIENT.open_from_launchpad_card("unix:path=/bus", "Notes", "Notes")
        node.assert_called_once_with(shell, "Notes")
        shell.call.assert_called_once_with("/card", "org.a11y.atspi.Action.DoAction", "0")
        new_app.assert_called_once_with("unix:path=/bus", {":1.1"}, "Notes")
        return opened

    def test_a_card_must_open_its_own_app(self):
        with self.subTest(phase="the card's app joined the bus"):
            self.assertEqual(self.run_card("notes"), (":1.9", 99))
        with self.subTest(phase="another app is not the card's"):
            with self.assertRaisesRegex(CLIENT.GateError, "not Notes"):
                self.run_card("calendar")


if __name__ == "__main__":
    unittest.main()
