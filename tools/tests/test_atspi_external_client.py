#!/usr/bin/env python3
"""Unit tests for the manifest-driven external AT-SPI client."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("atspi_external_client", ROOT / "tools/atspi_external_client.py")
assert SPEC and SPEC.loader
CLIENT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CLIENT
SPEC.loader.exec_module(CLIENT)


class PageServerTests(unittest.TestCase):
    def test_the_served_page_carries_the_expected_title_and_records_its_fetch(self):
        import urllib.request

        server, url, requests = CLIENT.serve_page("Engine <check>")
        try:
            self.assertTrue(url.startswith("http://127.0.0.1:"))
            with urllib.request.urlopen(url, timeout=10) as response:
                body = response.read().decode()
            self.assertIn("<title>Engine &lt;check&gt;</title>", body)
            self.assertEqual(requests, ["/"])
        finally:
            server.shutdown()


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


class OnScreenKeyboardTests(unittest.TestCase):
    def test_extents_replies_parse_into_rectangles(self):
        self.assertEqual(CLIENT.extents_reply("((12, -3, 640, 48),)\n"), (12, -3, 640, 48))
        self.assertIsNone(CLIENT.extents_reply("()"))

    def test_the_keyboard_docks_where_the_compositor_places_it(self):
        # crates/session/shell/src/shell/frame.rs `layout_osk_overlay`.
        self.assertEqual(CLIENT.keyboard_rect(1280, 800), (160.0, 460.0, 960.0, 320.0))
        self.assertEqual(CLIENT.keyboard_rect(1920, 1080), (480.0, 680.0, 960.0, 380.0))
        self.assertEqual(CLIENT.keyboard_rect(400, 300), (20.0, 60.0, 360.0, 220.0))


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


class TypingTargetTests(unittest.TestCase):
    def test_a_named_field_or_else_the_document_takes_typing(self):
        from unittest import mock

        client = mock.Mock()
        client.interfaces.return_value = "['org.a11y.atspi.EditableText', 'org.a11y.atspi.Component']"
        # AccessKit answers GetRole with an AtspiRole number; 82 is a document frame.
        client.role.return_value = 82
        target = CLIENT.is_typing_target
        with self.subTest(phase="a block editor's editable line, by name"):
            self.assertTrue(target(client, "/1", "(<'Block text'>,)", "Block text"))
            self.assertFalse(target(client, "/2", "(<'Page title'>,)", "Block text"))
        with self.subTest(phase="without a field name, the document"):
            self.assertTrue(target(client, "/3", "(<''>,)", None))
            client.role.return_value = 43  # a push button
            self.assertFalse(target(client, "/4", "(<'New note'>,)", None))


class CheckedRoleTests(unittest.TestCase):
    def test_checked_role_preserves_transport_failure_and_malformed_success(self):
        from unittest import mock

        client = CLIENT.AtspiClient("address", "bus")
        with mock.patch.object(
            client, "call", side_effect=CLIENT.GateError("UnknownObject")
        ) as read, self.assertRaisesRegex(CLIENT.GateError, "UnknownObject"):
            client.checked_role("/retired")
        read.assert_called_once_with(
            "/retired", "org.a11y.atspi.Accessible.GetRole"
        )

        with mock.patch.object(client, "call", return_value="(uint32 37,)\n"):
            self.assertEqual(client.checked_role("/tab"), 37)
        for malformed in ("", "prefix uint32 37 suffix", "(uint32 37, 'extra')"):
            with self.subTest(malformed=malformed), mock.patch.object(
                client, "call", return_value=malformed
            ):
                self.assertIsNone(client.checked_role("/malformed"))


class KeyboardFocusTests(unittest.TestCase):
    def focus_client(self, states: list[int]):
        from unittest import mock

        client = mock.Mock()
        client.tree.return_value = {"/page": "(<'Web content'>,)", "/address": "(<'Address and search'>,)"}
        client.interfaces.return_value = "['org.a11y.atspi.EditableText', 'org.a11y.atspi.Component']"
        replies = iter(states)

        def call(path, method, *args, check=True):
            if method.endswith("GetState"):
                return f"([uint32 {next(replies, states[-1])}, 0],)"
            return "()"

        client.call.side_effect = call
        return client

    def test_ready_waits_until_the_field_holds_the_keyboard(self):
        from unittest import mock

        # The window is mapped and focused on the third look.
        client = self.focus_client([0, 0, CLIENT.FOCUSED_STATE_BIT])
        with mock.patch.object(CLIENT.time, "sleep"):
            CLIENT.wait_for_keyboard_focus(client, "Address and search", 5)
        methods = [c.args[:2] for c in client.call.call_args_list]
        self.assertEqual(methods[0], ("/address", "org.a11y.atspi.Component.GrabFocus"))
        self.assertEqual(sum(m == "org.a11y.atspi.Accessible.GetState" for _, m in methods), 3)

    def test_a_window_that_never_takes_the_keyboard_fails_the_gate(self):
        from unittest import mock

        client = self.focus_client([0])
        clock = iter(range(0, 1000, 10))
        with mock.patch.object(CLIENT.time, "sleep"), \
                mock.patch.object(CLIENT.time, "monotonic", lambda: next(clock)), \
                mock.patch.object(CLIENT, "app_log", return_value="(nothing)"):
            with self.assertRaisesRegex(CLIENT.GateError, "never took keyboard focus"):
                CLIENT.wait_for_keyboard_focus(client, "Address and search", 30)


class TypedTextTests(unittest.TestCase):
    def test_text_replies_read_back_exactly(self):
        self.assertEqual(CLIENT.text_reply("('hi',)\n"), "hi")
        self.assertEqual(CLIENT.text_reply("('',)"), "")
        self.assertEqual(CLIENT.text_reply("('it\\'s, twice!',)"), "it's, twice!")
        self.assertIsNone(CLIENT.text_reply("Error: GDBus.Error"))

    def test_property_replies_read_back_exactly(self):
        self.assertEqual(CLIENT.property_text("(<'Level 2'>,)\n"), "Level 2")
        self.assertEqual(CLIENT.property_text("(< 'Level 2' >,)"), "Level 2")
        self.assertEqual(CLIENT.property_text("(<''>,)"), "")
        self.assertIsNone(CLIENT.property_text("Error: GDBus.Error"))


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

    def run_action_card(self, app_tree: dict[str, str], closed: list[int]) -> tuple[str, int]:
        from unittest import mock

        shell = mock.Mock()
        shell.call.return_value = "(true,)"
        with mock.patch.object(CLIENT, "shell_client", return_value=shell), \
                mock.patch.object(CLIENT, "bus_pids", return_value={":1.1": 10}), \
                mock.patch.object(CLIENT, "wait_for_shell_node", return_value=({}, "/card")), \
                mock.patch.object(CLIENT, "wait_for_new_app", return_value=(":1.9", 99)), \
                mock.patch.object(CLIENT.AtspiClient, "property", return_value="(<'calendar'>,)"), \
                mock.patch.object(CLIENT.AtspiClient, "tree", return_value=app_tree), \
                mock.patch.object(CLIENT, "process_command", return_value="/usr/bin/tilo-launcher"), \
                mock.patch.object(CLIENT, "START_TIMEOUT_SECONDS", 0.05), \
                mock.patch.object(CLIENT, "POLL_SECONDS", 0.01), \
                mock.patch.object(CLIENT, "close_app", side_effect=closed.append):
            return CLIENT.open_from_launchpad_card(
                "unix:path=/bus", "Tasks", "Calendar", ("Task list",),
            )

    def test_a_card_must_do_what_it_names_in_its_app(self):
        with self.subTest(phase="the app shows what the card's action opened"):
            closed: list[int] = []
            self.assertEqual(
                self.run_action_card({"/a": "(<'Task list'>,)"}, closed), (":1.9", 99),
            )
            self.assertEqual(closed, [])
        with self.subTest(phase="opening the app alone is not enough"):
            closed = []
            with self.assertRaisesRegex(CLIENT.GateError, "not what it names.*Task list"):
                self.run_action_card({"/a": "(<'Month'>,)"}, closed)
            self.assertEqual(closed, [99], "the app is closed for the next card")


class AppDrivingTests(unittest.TestCase):
    def test_names_are_read_from_gdbus_variants(self):
        tree = {
            "/root": "(<'App Store'>,)",
            "/a": "(<'Update all'>,)",
            "/b": "(<''>,)",
            "/c": "(<'Settings, offered by a test registry'>,)",
        }
        original = CLIENT.AtspiClient.tree
        CLIENT.AtspiClient.tree = lambda self, limit=0: tree
        try:
            names = CLIENT.app_names("unix:path=/x", ":1.2")
        finally:
            CLIENT.AtspiClient.tree = original
        self.assertEqual(names, ["App Store", "Update all", "Settings, offered by a test registry"])


class AppProcessTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.proc = Path(self.work.name)

    def tearDown(self):
        self.work.cleanup()

    def process(
        self,
        pid: int,
        parent: int,
        comm: str,
        *,
        started: int | None = None,
        executable: str | None = None,
        argv0: str | None = None,
    ) -> None:
        entry = self.proc / str(pid)
        entry.mkdir()
        start = started if started is not None else pid * 10
        fields = ["S", str(parent), *(["0"] * 17), str(start)]
        (entry / "stat").write_text(f"{pid} ({comm}) {' '.join(fields)}\n")
        (entry / "comm").write_text(f"{comm}\n")
        if executable is not None:
            (entry / "exe").symlink_to(executable)
        if argv0 is not None:
            (entry / "cmdline").write_bytes(argv0.encode() + b"\0--flag\0")

    def sandbox(self, *, duplicate_chat: bool = False) -> None:
        self.process(100, 1, "tilo-launcher", executable="/usr/bin/tilo-launcher")
        self.process(101, 100, "xdg-dbus-proxy", executable="/usr/bin/xdg-dbus-proxy")
        self.process(102, 100, "bwrap", executable="/usr/bin/bwrap")
        self.process(103, 102, "tilo-launcher", executable="/tilo-launcher")
        self.process(104, 103, "agent-helper", executable="/app/bin/agent-helper")
        self.process(105, 103, "chat", executable="/app/bin/chat")
        if duplicate_chat:
            self.process(106, 103, "chat", executable="/app/bin/chat")

    def test_exact_program_ignores_retained_launcher_and_helper_order(self):
        self.sandbox()
        # A same-named process outside the sandbox branch grants no authority.
        self.process(90, 100, "chat", executable="/tmp/chat")
        self.assertEqual(CLIENT.app_process(101, "chat", self.proc), 105)

    def test_exact_program_refuses_duplicate_or_missing_executables(self):
        self.sandbox(duplicate_chat=True)
        with self.assertRaisesRegex(CLIENT.GateError, "2 live 'chat' executables"):
            CLIENT.app_process(101, "chat", self.proc)
        self.assertIsNone(CLIENT.app_process(101, "notes", self.proc))

    def test_exact_program_skips_an_unrelated_direct_bus_peer(self):
        self.process(200, 1, "tilo-shell", executable="/usr/bin/tilo-shell")
        self.assertIsNone(CLIENT.app_process(200, "chat", self.proc))

    def test_exact_program_skips_a_direct_peer_that_exits_before_identity(self):
        self.process(200, 1, "transient", executable="/usr/bin/transient")

        def exited(root, pid):
            __import__("shutil").rmtree(root / str(pid))
            raise FileNotFoundError(pid)

        with patch.object(CLIENT, "_proc_stat", side_effect=exited):
            self.assertIsNone(CLIENT.app_process(200, "chat", self.proc))
        self.assertIsNone(CLIENT.app_process(201, "chat", self.proc))

    def test_exact_direct_peer_is_rechecked_before_acceptance(self):
        self.process(200, 1, "chat", executable="/usr/bin/chat")
        with patch.object(CLIENT, "_exact_program", side_effect=["chat", "notes"]):
            with self.assertRaisesRegex(CLIENT.GateError, "changed during app resolution"):
                CLIENT.app_process(200, "chat", self.proc)

    def test_exact_program_uses_argv0_only_when_exe_is_unavailable(self):
        self.sandbox()
        (self.proc / "105" / "exe").unlink()
        (self.proc / "105" / "cmdline").write_bytes(b"/app/bin/chat\0--private-value\0")
        self.assertEqual(CLIENT.app_process(101, "chat", self.proc), 105)

        # A readable kernel executable always wins over a spoofable argv.
        (self.proc / "105" / "exe").symlink_to("/app/bin/not-chat")
        self.assertIsNone(CLIENT.app_process(101, "chat", self.proc))

    def test_exact_program_does_not_hide_a_live_unreadable_identity(self):
        self.sandbox()
        (self.proc / "105" / "exe").unlink()
        with self.assertRaisesRegex(CLIENT.GateError, "exposes neither exe nor argv0"):
            CLIENT.app_process(101, "chat", self.proc)

    def test_exact_program_refuses_changed_proxy_identity(self):
        self.sandbox()
        original = CLIENT._proc_stat
        proxy_reads = 0

        def changing(root, pid):
            nonlocal proxy_reads
            result = original(root, pid)
            if pid == 101:
                proxy_reads += 1
                if proxy_reads >= 3:
                    return result[0], result[1] + 1
            return result

        with patch.object(CLIENT, "_proc_stat", side_effect=changing):
            with self.assertRaisesRegex(CLIENT.GateError, "changed during app resolution"):
                CLIENT.app_process(101, "chat", self.proc)

    def test_exact_program_refuses_changed_intermediate_ancestry(self):
        self.sandbox()
        original = CLIENT._proc_stat
        bwrap_reads = 0

        def changing(root, pid):
            nonlocal bwrap_reads
            result = original(root, pid)
            if pid == 102:
                bwrap_reads += 1
                if bwrap_reads >= 2:
                    return result[0], result[1] + 1
            return result

        with patch.object(CLIENT, "_proc_stat", side_effect=changing):
            with self.assertRaisesRegex(CLIENT.GateError, "changed during app resolution"):
                CLIENT.app_process(101, "chat", self.proc)

    def test_legacy_call_shape_remains_available(self):
        self.sandbox()
        self.assertIsInstance(CLIENT.app_process(101, proc_root=self.proc), int)


class FakeShell:
    """A desktop with two window groups: [Notes] and [Browser, Browser]."""

    ROOT = CLIENT.ROOT_PATH
    BASE = "/org/a11y/atspi/accessible/"

    def __init__(self):
        b = self.BASE
        self.nodes = {
            self.ROOT: ("frame", "", [b + "1", b + "4"]),
            b + "1": ("page tab list", "", [b + "2", b + "3"]),
            b + "2": ("page tab", "Notes", []),
            b + "3": ("push button", "New tab", []),
            b + "4": ("page tab list", "", [b + "5", b + "6", b + "7"]),
            b + "5": ("page tab", "Browser", []),
            b + "6": ("page tab", "Browser", []),
            b + "7": ("push button", "New tab", []),
        }

    def tree(self, _limit):
        return {path: f"(<'{name}'>,)" for path, (_role, name, _kids) in self.nodes.items()}

    def property(self, path, _name):
        return f"(<'{self.nodes[path][1]}'>,)"

    def role(self, path):
        return CLIENT.AtspiClient.role(self, path)

    def call(self, path, method, *args, check=True):
        role, _name, kids = self.nodes[path]
        if method.endswith("GetRole"):
            number = {"frame": 23, "page tab": 37, "page tab list": 38, "push button": 43}[role]
            return f"(uint32 {number},)"
        if method.endswith("GetChildren"):
            return "([" + ", ".join(f"(':1.2', objectpath '{kid}')" for kid in kids) + "],)"
        raise AssertionError(method)


class AccessiblePathTraversalTests(unittest.TestCase):
    def test_production_walk_visits_numeric_accesskit_and_app_namespaced_gtk_children(self):
        shell = FakeShell()
        gtk = "/so/tilo/android/qualify/fixture/a11y/f511c531_6ad3_465c_88d1_41a767adc1df"
        shell.nodes[shell.ROOT][2].append(gtk)
        shell.nodes[gtk] = ("frame", "Approved Android document status", [])
        tree = CLIENT.AtspiClient.tree(shell)
        self.assertEqual(set(tree), set(shell.nodes))
        self.assertEqual(CLIENT.AtspiClient.named(tree, "Approved Android document status"), [gtk])


class FakeSettings(CLIENT.AtspiClient):
    """Settings on the shared shell: wide, the category list sits beside the
    pane; narrow, it opens on the Network pane with a Back arrow to the list.
    Each screen maps a name to whether it can be clicked."""

    LIST = {"Settings": False, "Network": True, "Appearance": True, "Accessibility": True}
    PANE = {"Settings": False, "Back": True, "Network": False, "Connectivity": False}

    def __init__(self, narrow: bool):
        super().__init__("unix:path=/x", ":1.2")
        wide = {**self.PANE, **self.LIST}
        del wide["Back"]
        self.screen = self.PANE if narrow else wide
        self.clicked: list[str] = []

    def tree(self, limit=0):
        return {f"/{index}": f"(<'{name}'>,)" for index, name in enumerate(self.screen)}

    def call(self, path, method, *args, check=True):
        name = list(self.screen)[int(path[1:])]
        if method.endswith("GetActions"):
            return "([('click', '', '')],)" if self.screen[name] else "([],)"
        if method.endswith("DoAction"):
            self.clicked.append(name)
            if name == "Back":
                self.screen = self.LIST
            return "(true,)"
        raise AssertionError(method)


class NarrowWindowTests(unittest.TestCase):
    def test_a_wide_window_clicks_the_category_in_place(self):
        settings = FakeSettings(narrow=False)
        tree = CLIENT.wait_for_reachable(settings, ("Network",), "settings", clickable=True)
        settings.click(tree, "Network")
        self.assertEqual(settings.clicked, ["Network"])

    def test_a_narrow_window_goes_back_to_the_list_for_the_category(self):
        # The pane's heading is named "Network" too; only the list entry clicks.
        settings = FakeSettings(narrow=True)
        tree = CLIENT.wait_for_reachable(settings, ("Network",), "settings", clickable=True)
        settings.click(tree, "Network")
        self.assertEqual(settings.clicked, ["Back", "Network"])

    def test_manifest_names_on_the_list_are_reached_from_a_narrow_pane(self):
        settings = FakeSettings(narrow=True)
        CLIENT.wait_for_reachable(settings, ("Settings", "Appearance", "Accessibility"), "settings")
        self.assertEqual(settings.clicked, ["Back"])

    def test_a_name_missing_from_both_screens_fails_after_one_back(self):
        from unittest import mock

        settings = FakeSettings(narrow=True)
        with mock.patch.object(CLIENT, "START_TIMEOUT_SECONDS", 0.05), \
                mock.patch.object(CLIENT, "POLL_SECONDS", 0.001):
            with self.assertRaisesRegex(CLIENT.GateError, "click-capable Sound .also not after going Back"):
                CLIENT.wait_for_reachable(settings, ("Sound",), "settings", clickable=True)
        self.assertEqual(settings.clicked, ["Back"])


class TabListTests(unittest.TestCase):
    def test_window_group_strips_list_only_their_tabs(self):
        lists = CLIENT.tab_lists(FakeShell())
        self.assertEqual(len(lists), 2)
        self.assertEqual([len(tabs) for tabs in lists], [1, 2])
        self.assertTrue(all("Browser" in name for name in lists[1]))


if __name__ == "__main__":
    unittest.main()
