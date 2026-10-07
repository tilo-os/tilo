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
            self.assertIn("/usr/bin/tilo-control", args)
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


if __name__ == "__main__":
    unittest.main()
