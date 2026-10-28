"""Portal races and original job identity in the maintained acceptance driver."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from acceptance.action_jobs import PendingJob
from acceptance.vm import VmError
from acceptance import official_runtime
spec = importlib.util.spec_from_file_location("store_chooser", Path(__file__).with_name("store-chooser.py"))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
IDENTITY = "09f4eaa4-6d38-4e62-a644-e9924e5a4361"
OTHER = "b6256c85-9d56-44c2-8392-cf3fba9d825f"


def row(operation="inspect", identity=IDENTITY):
    return {"provider": "file-picker", "id": f"{operation}@{identity}"}


def value(**changes):
    return dict(request_id=IDENTITY, application=driver.app.STORE, state="a" * 64,
                completed=False, pages=1, entries=[], **changes)


class ChooserSafety(unittest.TestCase):
    def test_refuses_competing_and_malformed_requests(self):
        for rows in ([], [row(), row(identity=OTHER)], [row(identity="guess")], [row(operation="execute")]):
            with self.assertRaises(VmError):
                driver.request(rows)
        self.assertEqual(driver.request([row(), row("select")]), IDENTITY)

    def test_closed_original_or_replaced_portal_never_invokes(self):
        class Job:
            def pending(self):
                raise VmError("original ended")
        chooser = driver.Chooser(None, IDENTITY, "room", Path("."), Job())
        with self.assertRaises(VmError):
            chooser.call("confirm", {"confirm": True})
        job = type("Job", (), {"pending": lambda self: None})()
        chooser.original = job
        with patch.object(driver, "workspace"), patch.object(driver, "picker_rows", return_value=[row(identity=OTHER)]), patch.object(driver, "invoke") as invoke:
            with self.assertRaises(VmError):
                chooser.call("confirm", {"confirm": True})
            invoke.assert_not_called()

    def test_returned_foreign_application_and_request_are_refused(self):
        job = type("Job", (), {"pending": lambda self: None})()
        for changes in ({"application": "so.tilo.files"}, {"request_id": OTHER}, {"state": "short"}):
            reply = value()
            reply.update(changes)
            chooser = driver.Chooser(None, IDENTITY, "room", Path("."), job)
            with patch.object(driver, "workspace"), patch.object(driver, "picker_rows", return_value=[row()]), patch.object(driver, "invoke", return_value=reply):
                with self.assertRaises(VmError):
                    chooser.call("inspect")

    def test_initial_documents_parent_uses_current_state_and_declared_up(self):
        chooser = driver.Chooser(None, IDENTITY, "room", Path("."), None)
        inspected = value(); inspected["can_up"] = True
        with patch.object(chooser, "call", return_value=inspected) as call:
            chooser.ascend(1)
        self.assertEqual(call.call_args_list[0].args, ("inspect", {"page": 0}))
        self.assertEqual(call.call_args_list[1].args, ("up", {"state": inspected["state"]}))
        for fields in ({"can_up": False}, {"completed": True}, {}):
            refused = value(); refused.update(fields)
            with patch.object(chooser, "call", return_value=refused) as call:
                with self.assertRaises(VmError):
                    chooser.ascend(1)
                self.assertEqual(call.call_count, 1)

    def test_paged_entry_uses_actual_opaque_id_and_rejects_listing_changes(self):
        chooser = driver.Chooser(None, IDENTITY, "room", Path("."), None)
        first = value(); first["pages"] = 2
        second = value(); second.update(pages=2, entries=[{"name": "official.tapp", "directory": False, "id": "b" * 64}])
        with patch.object(chooser, "call", side_effect=[first, second]):
            self.assertEqual(chooser.find("official.tapp", False), ("a" * 64, "b" * 64))
        second["state"] = "c" * 64
        with patch.object(chooser, "call", side_effect=[first, second]):
            with self.assertRaises(VmError):
                chooser.find("official.tapp", False)
        with self.assertRaises(VmError):
            chooser.find("../official.tapp", False)

    def test_workspace_change_lock_and_unobservable_owner_state_refuse(self):
        state = {"chrome": {"session_locked": False}, "workspace_tree": {
            "active": "workspace", "workspaces": [{"id": "workspace", "room": "room"}]}}
        vm = type("VM", (), {"state": lambda self: state})()
        driver.workspace(vm, "room")
        with self.assertRaises(VmError):
            driver.workspace(vm, "other")
        state["chrome"]["session_locked"] = True
        with self.assertRaises(VmError):
            driver.workspace(vm, "room")

    def test_path_arguments_refuse_before_any_import_or_status_write(self):
        door = type("Door", (), {"vm": object(), "bound": (driver.app.STORE, "room", "record", "resource")})()
        for filename, directories in (("../full.tapp", []), ("full.tapp", [".."]), ("full.tapp", ["Downloads/scratch"])):
            with patch.object(driver, "begin") as start:
                with self.assertRaises(VmError):
                    driver.review(door, {}, filename, directories, Path("."))
                start.assert_not_called()

    def test_preexisting_chooser_refuses_before_import_submission(self):
        door = type("Door", (), {"vm": object(), "bound": (driver.app.STORE, "room", "record", "resource")})()
        with patch.object(driver, "workspace"), patch.object(driver, "picker_rows", return_value=[row()]), patch.object(driver, "begin") as start:
            with self.assertRaises(VmError):
                driver.review(door, {}, "full.tapp", [], Path("."))
            start.assert_not_called()

    def test_owner_cli_uses_slot_user_and_canonical_admin_socket_without_actor_override(self):
        with tempfile.TemporaryDirectory() as directory:
            vm = driver.OwnerVm("tilo-vm", "yoga", "owned", Path(directory) / "commands.log")
            with patch.object(vm, "slot_uid", return_value=1000), patch.object(vm, "run", return_value='{"actions": []}') as run:
                vm.control_json("actions", "list", "file-picker", "--json")
            args = run.call_args.args
            self.assertEqual(args[:3], ("slot", "exec", "--"))
            self.assertIn("/usr/libexec/tilo/tilo-control", args)
            self.assertIn("/run/user/1000/tilo-session/admin-control.sock", args)
            self.assertEqual(args[3:6], ("env", "-u", "TILO_TEST_CONTROL"))
            self.assertNotIn("--actor", args)

    def test_import_stays_pending_until_declared_select_and_confirm_then_review(self):
        events = []
        class Job:
            def pending(self):
                events.append("pending")
            def finish(self, timeout):
                events.append("finish")
        class Door:
            vm = object()
            bound = (driver.app.STORE, "room", "record", "resource")
            def settled(self, identity):
                events.append("review")
                return {"imported_package": {"app": identity, "root": "b" * 64, "version": "1"},
                        "detail": {"trust": "PublisherSigned"}}
        fixture = {"package_id": "so.tilo.calculator", "offer": {"tapp_root_hash_hex": "b" * 64, "version": "1"}}
        inspected = value(); inspected["entries"] = [{"id": "c" * 64, "name": "full.tapp", "directory": False}]
        selected = value(); selected.update(can_confirm=True)
        completed = value(); completed["completed"] = True
        def perform(vm, action, args, *unused):
            operation = action["id"].split("@")[0]
            events.append(operation)
            if operation == "select":
                self.assertEqual(args, {"state": "a" * 64, "entry": "c" * 64})
                return selected
            if operation == "confirm":
                self.assertEqual(args, {"state": "a" * 64, "confirm": True})
                return completed
            return inspected
        with tempfile.TemporaryDirectory() as directory, patch.object(driver, "workspace"), \
             patch.object(driver.app, "catalog"), patch.object(driver.app, "choose", return_value={}), \
             patch.object(driver, "begin", return_value=Job()), \
             patch.object(driver, "picker_rows", side_effect=[[], [row()], [row()], [row("select")], [row("confirm")], []]), \
             patch.object(driver, "invoke", side_effect=perform):
            driver.review(Door(), fixture, "full.tapp", [], Path(directory))
        self.assertLess(events.index("confirm"), events.index("finish"))
        self.assertLess(events.index("finish"), events.index("review"))
        self.assertEqual(events.count("confirm"), 1)

    def test_pending_job_preserves_original_numeric_and_uuid_identity(self):
        def receipt(state="running", identity="original"):
            return {"receipt": {"broker_invocation_id": 17, "update": {"invocation": identity, "state": state}}}
        vm = type("VM", (), {"control_json": lambda self, *args: receipt(identity="replacement")})()
        job = PendingJob(vm, receipt(), Path("."), "import")
        with self.assertRaises(VmError):
            job.pending()


class CanonicalRuntimeAudit(unittest.TestCase):
    def test_invalid_subject_does_not_invoke_installer(self):
        with patch.object(official_runtime, "command") as command:
            with self.assertRaises(RuntimeError):
                official_runtime.inspect("so.tilo.notes", "f" * 64, "1", "tilo-runtime/official", "d" * 64)
        command.assert_not_called()

    def test_canonical_refusal_and_wrong_root_stop_before_bundle_lookup(self):
        for response in (VmError("signature revoked"),
                         "id : so.tilo.calculator\nversion : 1\npackage-id : tapp:" + "e" * 64):
            with patch.object(official_runtime, "command", side_effect=["verified", response]) as command:
                with self.assertRaises((RuntimeError, VmError)):
                    official_runtime.inspect("so.tilo.calculator", "f" * 64, "1", "tilo-runtime/official", "d" * 64)
            self.assertEqual(command.call_count, 2)

    def test_public_file_hash_is_bounded_and_matches_actual_bytes(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "official-public-payload"
            file.write_bytes(b"public signed-payload fixture bytes")
            digest, status = official_runtime.hash_file(file)
            self.assertEqual(digest, hashlib.sha256(file.read_bytes()).hexdigest())
            self.assertEqual(status.st_size, file.stat().st_size)
            file.write_bytes(b"")
            with self.assertRaises(RuntimeError):
                official_runtime.hash_file(file)

    def test_duplicate_inspector_fields_are_ambiguous(self):
        with self.assertRaises(RuntimeError):
            official_runtime.fields("version : old\nversion : new")


class InstalledOpen(unittest.TestCase):
    fixture = {"package_id": "so.tilo.calculator", "publisher": "d" * 64,
               "offer": {"version": "official", "tapp_root_hash_hex": "f" * 64,
                         "target": {"runtime": "tilo-runtime/official"}}}

    def subject(self, opened=True):
        state = {"workspace_tree": {"local_app_records": {"7": {
            "room": "room", "app_id": "so.tilo.calculator", "record_instance": 11}},
            "workspaces": [{"room": "room", "strips": [{"window_groups": [{"tabs": [
                {"id": 7, "tile": {"Foreign": {"app_id": "so.tilo.calculator", "surface_id": 99}}}
            ]}]}]}]}}
        if opened:
            state["workspace_tree"]["local_app_records"]["8"] = {
                "room": "room", "app_id": "so.tilo.calculator", "record_instance": 12}
            tabs = state["workspace_tree"]["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"]
            tabs.append({"id": 8, "tile": {"Foreign": {"app_id": "so.tilo.calculator", "surface_id": 100, "adoption_id": "11111111-1111-4111-8111-111111111111"}}})
        state["chrome"] = {"native_window_provenance": {"method": "authenticated-wayland-peer", "omitted": 0,
            "windows": [{"room": "room", "tab_id": 8, "surface_id": 100, "package_id": "so.tilo.calculator",
                         "adoption_id": "11111111-1111-4111-8111-111111111111", "record_instance": 12, "peer_pid": 222, "peer_start_time_ticks": "999",
                         "certified_checkout": {"application": "so.tilo.calculator", "room": "room", "record": "derived:default@tab:000000000000000c"}}] if opened else []}}
        return state

    def door(self):
        from unittest.mock import Mock
        vm = Mock()
        opened = []
        vm.state.side_effect = lambda: self.subject(bool(opened))
        vm.slot_uid.return_value = 1000
        vm.run.return_value = __import__("json").dumps({"uid": 1000,
            "runtime": "tilo-runtime/official", "files": {"signed-file": "sha"}, "executable_sha256": "app-sha"})
        door = Mock(vm=vm, bound=(driver.app.STORE, "room", "record", "resource"))
        door.settled.return_value = {"detail": {"installed": True}}
        def call(operation, *_):
            if operation == "open-app":
                opened.append(True)
                return {"launch": {"adopted_window": {"package_id": "so.tilo.calculator", "room": "room", "surface_id": 100, "adoption_id": "11111111-1111-4111-8111-111111111111"}}}
        door.call.side_effect = call
        return door

    def test_existing_window_update_requires_official_backing_without_close(self):
        door = self.door()
        with tempfile.TemporaryDirectory() as directory, patch.object(driver.app, "mapped") as mapped:
            value = driver.install_open(door, self.fixture, True, Path(directory))
        self.assertEqual(value["installation"], "update/reuse")
        self.assertEqual(value["window"], "new")
        self.assertEqual(value["record"], "derived:default@tab:000000000000000c")
        self.assertEqual([call.args[0] for call in door.call.call_args_list],
                         ["install-imported-package", "open-app"])
        self.assertEqual(mapped.call_args.args[1:4], ("calculator", {"signed-file": "sha"}, 1000))
        self.assertEqual(mapped.call_args.kwargs["selected"]["peer_pid"], 222)
        self.assertEqual(value["tab"], ("room", "8", "100"))
        self.assertIn(("room", "7", "99"), driver.app.windows(door.vm.state(), "so.tilo.calculator"))

    def test_old_resident_sdk_and_missing_association_fail_without_closing(self):
        for missing in (False, True):
            door = self.door()
            if missing:
                state = self.subject()
                state["workspace_tree"]["local_app_records"] = {}
                door.vm.state.side_effect = None
                door.vm.state.return_value = state
            with tempfile.TemporaryDirectory() as directory, \
                    patch.object(driver, "wait_until", side_effect=lambda probe, _: probe()), \
                    patch.object(driver.app, "mapped", side_effect=VmError("old resident SDK")):
                with self.assertRaises(VmError):
                    driver.install_open(door, self.fixture, True, Path(directory))
            self.assertEqual([call.args[0] for call in door.call.call_args_list],
                             ["install-imported-package", "open-app"])

    def test_canonical_identity_failure_stops_before_open(self):
        door = self.door()
        door.vm.run.side_effect = VmError("verified installed root differs")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(VmError):
                driver.install_open(door, self.fixture, True, Path(directory))
        self.assertEqual([call.args[0] for call in door.call.call_args_list], ["install-imported-package"])

    def test_closed_replaced_foreign_and_unobservable_windows_do_not_qualify(self):
        import copy
        window = {"package_id": "so.tilo.calculator", "room": "room", "surface_id": 100, "adoption_id": "11111111-1111-4111-8111-111111111111"}
        for kind in ("closed", "record", "package", "room", "missing", "duplicate", "lifetime", "missing_lifetime"):
            with self.subTest(kind=kind):
                state = copy.deepcopy(self.subject())
                proof = state["chrome"]["native_window_provenance"]
                if kind == "closed":
                    state["workspace_tree"]["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"].pop()
                elif kind == "record": proof["windows"][0]["record_instance"] = 99
                elif kind == "package": proof["windows"][0]["package_id"] = "foreign-package"
                elif kind == "room": proof["windows"][0]["room"] = "foreign-room"
                elif kind == "missing": state["chrome"].clear()
                elif kind == "duplicate": proof["windows"].append(copy.deepcopy(proof["windows"][0]))
                elif kind == "lifetime": proof["windows"][0]["adoption_id"] = "22222222-2222-4222-8222-222222222222"
                elif kind == "missing_lifetime": proof["windows"][0].pop("adoption_id")
                if kind in ("closed", "room"):
                    self.assertIsNone(driver.opened_subject(state, window))
                else:
                    with self.assertRaises(VmError): driver.opened_subject(state, window)

    def test_reused_numeric_surface_cannot_replace_the_opened_resource_lifetime(self):
        state = self.subject()
        window = {"package_id": "so.tilo.calculator", "room": "room", "surface_id": 100,
                  "adoption_id": "11111111-1111-4111-8111-111111111111"}
        state["chrome"]["native_window_provenance"]["windows"][0]["adoption_id"] = "22222222-2222-4222-8222-222222222222"
        with self.assertRaises(VmError):
            driver.opened_subject(state, window)

    def test_association_change_during_fd_audit_fails_without_closing(self):
        door = self.door()
        current = door.vm.state.side_effect
        altered = []
        def state():
            value = current()
            if altered:
                value["chrome"]["native_window_provenance"]["windows"][0]["peer_start_time_ticks"] = "1000"
            return value
        door.vm.state.side_effect = state
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(driver.app, "mapped", side_effect=lambda *_args, **_kwargs: altered.append(True)):
            with self.assertRaisesRegex(VmError, "association changed"):
                driver.install_open(door, self.fixture, True, Path(directory))
        self.assertEqual([call.args[0] for call in door.call.call_args_list], ["install-imported-package", "open-app"])


if __name__ == "__main__":
    unittest.main()
