"""Fail-closed public binding and receipt decoding guards."""
import importlib.util
import copy
import tempfile
from unittest.mock import patch, Mock
from types import SimpleNamespace
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from acceptance.vm import VmError, first_json

spec = importlib.util.spec_from_file_location('terminal_cohort', Path(__file__).with_name('terminal-cohort.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class TerminalCohort(unittest.TestCase):
    def row(self, resource='bound', operation='run'):
        return dict(application=module.APP, room='room', record='record', resource=resource,
                    provider='app-actions', operation=operation,
                    semantic_identity={'kind': 'terminal.session', 'key': 'terminal.session'},
                    params=[], id=resource + operation, contract={'availability': {'state': 'available'}})

    def test_same_bound_connection_only(self):
        row = self.row()
        self.assertIs(module.apps.choose([row, self.row('other')], module.APP, module.FAMILY, 'run', {}, module.identity(row)), row)
        with self.assertRaises(VmError):
            module.apps.choose([self.row('other')], module.APP, module.FAMILY, 'run', {}, module.identity(row))

    def test_ambiguous_missing_denied_are_errors(self):
        row = self.row()
        for rows in ([], [row, row]):
            with self.assertRaises(VmError):
                module.apps.choose(rows, module.APP, module.FAMILY, 'run', {})
        row['contract']['availability']['state'] = 'unavailable'
        with self.assertRaises(VmError):
            module.apps.choose([row], module.APP, module.FAMILY, 'run', {})

    def test_title_does_not_imply_authority(self):
        row = self.row()
        row['semantic_identity'] = None
        row['title'] = 'Run command'
        with self.assertRaises(VmError):
            module.apps.choose([row], module.APP, module.FAMILY, 'run', {})

    def test_acceptance_is_not_completion(self):
        with self.assertRaises(VmError):
            module.succeeded({'receipt': {'update': {'state': 'accepted'}}})

    def test_pretty_and_compact_canonical_json(self):
        self.assertEqual(first_json('{\n "slot": {}\n}\n'), {'slot': {}})
        self.assertEqual(first_json('warning\n{"slot": {}}\n'), {'slot': {}})
        with self.assertRaises(VmError):
            first_json('unobservable')


ROOM = "5ba25f73-87c5-447f-a5a6-880dc4249a81"


def mounted(*ids):
    workers = [dict(room=ROOM, tab_id=tab, surface_id=tab + 100, record_instance=tab + 10,
                    package_id=module.APP, peer_pid=tab + 500, peer_start_time_ticks="1234",
                    adoption_id="4a2b3a5b-96bc-4c4a-985e-c84b12fffbfc",
                    certified_checkout=dict(application=module.APP, room=ROOM,
                        record=f"derived:default@tab:{tab+10:016x}")) for tab in ids]
    return {"chrome": {"session_locked": False, "native_window_provenance": {
                "method": "authenticated-wayland-peer", "omitted": 0, "windows": workers}},
            "workspace_tree": {"local_app_records": {str(tab): dict(room=ROOM, app_id=module.APP,
                record_instance=tab + 10) for tab in ids}, "workspaces": [dict(room=ROOM, strips=[{
                "window_groups": [dict(id=7, tabs=[dict(id=tab, tile={"Foreign": {
                    "app_id": module.APP, "surface_id": tab + 100}}) for tab in ids])]}])]}}


class VM:
    def __init__(self, state):
        self.current = state
        self.effects = []
        self.discovery = []

    def state(self):
        return copy.deepcopy(self.current)

    def actions(self, query):
        return [{"provider": "desktop-windows", "id": query}]

    def control_json(self, *args):
        self.discovery.append(args)
        scope = {key: args[args.index(flag)+1] for key, flag in (("application", "--application"),
                 ("room", "--room"), ("record", "--record"), ("semantic_family", "--semantic-family"))}
        # A current adopted resource nonce differs from the launcher's old nonce.
        row = dict(application=module.APP, room=scope["room"], record=scope["record"], resource="adopted",
                   provider="app-actions", operation="inspect-session", id="inspect@adopted", params=[],
                   semantic_identity={"kind": module.FAMILY, "key": module.FAMILY},
                   contract={"availability": {"state": "available"}})
        return dict(query="", completion=dict(complete=True, incomplete=[]), scope=scope, count=1, actions=[row])


class FreshTerminalGuards(unittest.TestCase):
    def owned(self, vm, out):
        owned = module.OwnedTerminal(vm, out)
        vm.current = mounted(1, 2)
        self.assertIsNotNone(owned.adopt())
        return owned

    def test_existing_terminal_is_preserved_and_new_record_is_scoped(self):
        with tempfile.TemporaryDirectory() as folder:
            vm = VM(mounted(1))
            owned = self.owned(vm, Path(folder))
            self.assertEqual(owned.tab, (ROOM, "2", "102"))
            self.assertEqual(owned.worker["peer_pid"], 502)
            self.assertTrue(vm.discovery)
            for args in vm.discovery:
                self.assertIn("--semantic-family", args)
                self.assertEqual(args[args.index("--record")+1], "derived:default@tab:000000000000000c")
            def close(_vm, row, args, out, name, **kwargs):
                self.assertEqual(row["id"], "tab.close:7:2")
                _vm.effects.append(row["id"])
                _vm.current = mounted(1)
            with patch.object(module, "invoke", side_effect=close):
                self.assertTrue(owned.close()["closed"])
            self.assertEqual(vm.effects, ["tab.close:7:2"])
            self.assertEqual(module.apps.windows(vm.state(), module.APP), {(ROOM, "1", "101")})

    def test_replaced_worker_locked_missing_auth_and_foreign_record_refuse_before_close(self):
        for mutation in (lambda s: s["chrome"].update(session_locked=True),
                         lambda s: s["chrome"]["native_window_provenance"]["windows"][1].update(peer_pid=999),
                         lambda s: s["chrome"]["native_window_provenance"]["windows"][1].update(peer_start_time_ticks="999"),
                         lambda s: s["chrome"]["native_window_provenance"]["windows"][1].update(adoption_id=None),
                         lambda s: s["workspace_tree"]["local_app_records"]["2"].update(room="foreign")):
            with tempfile.TemporaryDirectory() as folder:
                vm = VM(mounted(1))
                owned = self.owned(vm, Path(folder))
                mutation(vm.current)
                with patch.object(module, "invoke") as effect, self.assertRaises(VmError):
                    owned.close()
                effect.assert_not_called()

    def test_two_fresh_tabs_cannot_be_adopted(self):
        with tempfile.TemporaryDirectory() as folder:
            vm = VM(mounted(1))
            owned = module.OwnedTerminal(vm, Path(folder))
            vm.current = mounted(1, 2, 3)
            with self.assertRaises(VmError):
                owned.adopt()
            self.assertIsNone(owned.worker)

    def test_returned_prompt_requires_standalone_exact_marker_and_session(self):
        content = dict(session="s", target={"kind": "environment"}, output_truncated=False,
                       output="printf MARK\n[Default test]$ ")
        self.assertFalse(module.marker_returned(content, "s", "MARK"))
        content["output"] = "MARK\n[Default test]$ "
        self.assertTrue(module.marker_returned(content, "s", "MARK"))
        for invalid in (dict(content, session="other"), dict(content, output_truncated=True),
                        dict(content, output="MARK\nMARK\n[Default test]$ ")):
            with self.assertRaises(VmError):
                module.marker_returned(invalid, "s", "MARK")

    def test_failed_command_still_closes_only_the_pinned_owned_tab(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            receipt = root / "receipt.json"
            receipt.write_text('{}')
            args = SimpleNamespace(out=root / "proof", cohort_receipt=receipt, uid=1000, vm="owned", slot="slot")
            owner = Mock()
            owner.worker = {"peer_pid": 502, "peer_start_time_ticks": "1234"}
            owner.close.return_value = {"closed": True, "tab": [ROOM, "2", "102"]}
            owner.open.return_value.call.side_effect = [dict(lifecycle="open", input_available=True,
                transport_disconnected=False, target={"kind": "environment"}, session="s"), VmError("Run refused")]
            shown = {"slot": {"components": {"apps+runtime": "new"}}}
            def mapped(*args, **kwargs):
                (root / "proof" / "terminal-mapped-sdk.json").write_text(
                    '{"processes":[{"runtime":[{"path":"/run/tilo-runtime/root/lib/libstd-test.so"}]}]}')
            with patch.object(module.apps, "cohort", return_value=({"cohort_source": {}}, shown, {})), \
                    patch.object(module, "executable", return_value="a"*64), \
                    patch.object(module, "OwnedTerminal", return_value=owner), \
                    patch.object(module.apps, "mapped", side_effect=mapped), self.assertRaisesRegex(VmError, "Run refused"):
                module.run(args, vm=object())
            owner.close.assert_called_once_with()
            self.assertFalse((root / "proof" / "proof.json").exists())
            self.assertIn('"closed": true', (root / "proof" / "cleanup.json").read_text())

    def test_executable_requires_selected_cohort_member_not_other_layer(self):
        row = dict(component="apps+runtime", layer="new", path="/usr/share/tilo-apps/hash/bin/terminal", sha256="a"*64)
        self.assertEqual(module.executable({"files": [row]}, "new"), "a"*64)
        for rows in ([dict(row, layer="old")], [row, row], [dict(row, link="elsewhere")]):
            with self.assertRaises(VmError):
                module.executable({"files": rows}, "new")


class OwnerConnectionJoin(unittest.TestCase):
    FIRST = "11111111-1111-4111-8111-111111111111"
    NEXT = "22222222-2222-4222-8222-222222222222"

    def fixture(self, sessions):
        info = dict(session=self.FIRST, target={"kind": "environment"}, lifecycle="open",
                    input_available=True, transport_disconnected=False)
        owned = Mock()
        owned.vm.actions.side_effect = lambda query: [self.row(operation, connection) for connection in sessions
            for operation in ("inspect", "request", "run", "read")]
        owned.out = Path("unused")
        owned.door.call.side_effect = lambda operation: dict(info)
        def operation(vm, row, args, out, name, **kwargs):
            kind, connection = row["id"].split("@")
            if kind == "inspect":
                return dict(info, session=sessions[connection])
            if kind == "request":
                sessions[connection] = self.NEXT
                info.update(session=self.NEXT, target={"kind": "host"})
                return dict(info)
            return dict(session=sessions[connection], status="submitted")
        return owned, info, operation

    def row(self, operation, connection):
        return dict(provider=module.DEVICE_PROVIDER, id=operation + "@" + connection,
                    contract={"availability": {"state": "available"}, "confirmation": {"mode": "confirm"}})

    def test_exact_local_token_selects_one_connection_not_catalog_order(self):
        owned, info, operation = self.fixture({"7": self.NEXT, "9": self.FIRST})
        with patch.object(module, "invoke", side_effect=operation):
            joined = module.JoinedDeviceShell(owned)
            self.assertEqual(joined.connection, "9")
            joined.request()
            self.assertEqual(joined.session, self.NEXT)
            joined.call("run", {"command": "printf marker"})
            with self.assertRaises(VmError):
                joined.request()

    def test_continuation_cannot_replay_request(self):
        owned, info, operation = self.fixture({"9": self.FIRST})
        with patch.object(module, "invoke", side_effect=operation) as calls:
            joined = module.JoinedDeviceShell(owned)
            joined.request()
            before = calls.call_count
            with self.assertRaisesRegex(VmError, "unsupported device-shell continuation"):
                joined.call("request", {"confirm": True})
            self.assertEqual(calls.call_count, before)

    def test_missing_or_ambiguous_local_token_cannot_request_host(self):
        for sessions in ({"7": self.NEXT}, {"7": self.FIRST, "9": self.FIRST}, {}):
            owned, info, operation = self.fixture(sessions)
            with patch.object(module, "invoke", side_effect=operation) as invocations, self.assertRaises(VmError):
                module.JoinedDeviceShell(owned)
            self.assertTrue(all(call.args[1]["id"].startswith("inspect@") for call in invocations.call_args_list))

    def test_replaced_session_or_worker_refuses_before_run(self):
        sessions = {"9": self.FIRST}
        owned, info, operation = self.fixture(sessions)
        with patch.object(module, "invoke", side_effect=operation) as invocations:
            joined = module.JoinedDeviceShell(owned)
            joined.request()
            info["session"] = sessions["9"] = self.FIRST
            with self.assertRaises(VmError):
                joined.call("run", {"command": "printf marker"})
            self.assertFalse(any(call.args[1]["id"].startswith("run@") for call in invocations.call_args_list))
            owned.guard.side_effect = VmError("native PID/start/adoption changed")
            with self.assertRaises(VmError):
                joined.call("run", {"command": "printf marker"})

    def test_awaiting_consent_never_grants_or_runs_without_operator(self):
        sessions = {"9": self.FIRST}
        owned, info, operation = self.fixture(sessions)
        def pending(vm, row, args, out, name, **kwargs):
            if row["id"] == "request@9":
                info.update(target={"kind": "host"}, lifecycle="awaiting_allow")
                return dict(info)
            return operation(vm, row, args, out, name, **kwargs)
        with patch.object(module, "invoke", side_effect=pending) as calls:
            joined = module.JoinedDeviceShell(owned)
            with self.assertRaisesRegex(VmError, "explicit owner consent"):
                joined.request()
            self.assertFalse(any(call.args[1]["id"].startswith("run@") for call in calls.call_args_list))
            self.assertTrue(all(call.args[1]["provider"] == module.DEVICE_PROVIDER for call in calls.call_args_list))

    def test_actual_host_prompt_ignores_only_empty_terminal_padding(self):
        value = dict(session=self.FIRST, truncated=False, text="startup\nuser@host ~$ \n\n\n")
        self.assertEqual(module.host_prompt(value, self.FIRST), "user@host ~$ ")
        self.assertIsNone(module.host_prompt(dict(value, text="still starting\n\n"), self.FIRST))
        for invalid in (dict(value, session=self.NEXT), dict(value, truncated=True)):
            with self.assertRaises(VmError):
                module.host_prompt(invalid, self.FIRST)

    def test_crdt_or_caption_does_not_supply_native_token(self):
        for value in ({"local_session": self.FIRST}, {"session": None, "title": self.FIRST}, {"session": "not-a-uuid"},
                      {"session": "00000000-0000-0000-0000-000000000000"}):
            with self.assertRaises(VmError):
                module.local_session(value)

    def test_agent_refusal_is_not_replaced_with_generic_host_action(self):
        owned, info, operation = self.fixture({"9": self.FIRST})
        with patch.object(module, "invoke", side_effect=VmError("owner authorization required")) as effects:
            with self.assertRaises(VmError):
                module.JoinedDeviceShell(owned)
        self.assertEqual(effects.call_count, 1)
        self.assertEqual(effects.call_args.args[1]["provider"], module.DEVICE_PROVIDER)


if __name__ == '__main__':
    unittest.main()
