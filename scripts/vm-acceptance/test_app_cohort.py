"""Exercise receipt transitions and fresh-instance safety in the production runner."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from acceptance import action_jobs
from acceptance.vm import VmError

spec = importlib.util.spec_from_file_location("app_cohort", Path(__file__).with_name("app-cohort.py"))
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)


def receipt(state, job=17, invocation="owner", value=None):
    return {"receipt": {"broker_invocation_id": job, "update": {
        "invocation": invocation, "state": state, "output": {"value": value}}}}


def polls(probe, timeout, **kwargs):
    for _ in range(5):
        result = probe()
        if result is not None:
            return result
    return None


ROOM = "room"


def state(*tabs):
    return {"workspace_tree": {"local_app_records": {str(tab): {"room": "room",
            "app_id": app.BROWSER, "record_instance": tab + 10} for tab in tabs}, "workspaces": [{"room": "room", "strips": [{
        "window_groups": [{"tabs": [{"id": tab, "tile": {"Foreign": {
            "app_id": app.BROWSER, "surface_id": tab + 100}}} for tab in tabs]}]}]}]}}


def row(record="derived:default@tab:000000000000000b", operation="navigate"):
    return {"provider": "app-actions", "application": app.BROWSER,
            "room": "room", "record": record, "resource": "bound:" + record,
            "id": record + ":" + operation, "operation": operation,
            "semantic_identity": {"kind": "browser.page"}, "params": [],
            "contract": {"availability": {"state": "available"}}}


class Jobs(unittest.TestCase):
    def test_waits_for_same_job_terminal_without_reinvoke(self):
        class VM:
            calls = 0
            updates = iter([receipt("accepted"), receipt("running"), receipt("succeeded", value="done")])

            def invoke(self, *args):
                self.calls += 1
                return receipt("accepted")

            def control_json(self, *args):
                return next(self.updates)
        vm = VM()
        with tempfile.TemporaryDirectory() as directory, patch.object(action_jobs, "wait_until", polls):
            value = action_jobs.invoke(vm, {"provider": "app-actions", "id": "bound"}, {}, Path(directory), "test")
            self.assertEqual(value, "done")
            self.assertEqual(vm.calls, 1)
            self.assertEqual(__import__('json').loads((Path(directory) / "test-status.json").read_text()), receipt("succeeded", value="done"))

    def test_stale_unknown_failed_and_untracked_are_not_replayed(self):
        for reply in (receipt("succeeded", invocation="other"), receipt("unknown"), receipt("failed")):
            class VM:
                calls = 0

                def invoke(self, *args):
                    self.calls += 1
                    return receipt("accepted")

                def control_json(self, *args):
                    return reply
            vm = VM()
            with tempfile.TemporaryDirectory() as directory, patch.object(action_jobs, "wait_until", polls):
                with self.assertRaises(VmError):
                    action_jobs.invoke(vm, {"provider": "app-actions", "id": "bound"}, {}, Path(directory), "test")
            self.assertEqual(vm.calls, 1)
        with tempfile.TemporaryDirectory() as directory:
            class Untracked:
                def invoke(self, *args):
                    return {"receipt": {"update": {"state": "accepted"}}}
            with self.assertRaises(VmError):
                action_jobs.invoke(Untracked(), {"provider": "actions", "id": "open"}, {}, Path(directory), "open", require_job=False)


class BrowserWorker(unittest.TestCase):
    def current(self):
        value = state(1, 2)
        worker = {"room":ROOM, "tab_id":2, "surface_id":102, "package_id":app.BROWSER,
                  "certified_checkout":{"application":app.BROWSER,"room":ROOM,"record":"derived:default@tab:000000000000000c"},
                  "record_instance":12, "peer_pid":456, "peer_start_time_ticks":"12345",
                  "adoption_id":"12345678-1234-1234-1234-123456789abc"}
        value["chrome"] = {"native_window_provenance":{"method":"authenticated-wayland-peer", "omitted":0,
                                                     "windows":[worker]}}
        return value, worker

    def test_uses_public_record_instance_and_exact_new_surface_not_other_workers(self):
        value, worker = self.current()
        value["chrome"]["native_window_provenance"]["windows"].append(dict(worker, tab_id=1, surface_id=101, peer_pid=789))
        self.assertEqual(app.browser_subject(value,(ROOM,"2","102")),worker)

    def test_foreign_record_pid_lifetime_ambiguous_or_missing_provenance_refuses(self):
        for field,bad in [("record_instance",2),("package_id",app.STORE),("peer_pid",True),
                          ("peer_start_time_ticks",""),("adoption_id","00000000-0000-0000-0000-000000000000")]:
            value,worker=self.current();worker[field]=bad
            with self.assertRaises(VmError):app.browser_subject(value,(ROOM,"2","102"))
        value,worker=self.current();value["chrome"]["native_window_provenance"]["windows"].append(dict(worker))
        with self.assertRaises(VmError):app.browser_subject(value,(ROOM,"2","102"))
        value,_=self.current();value["chrome"]["native_window_provenance"]["omitted"]=1
        with self.assertRaises(VmError):app.browser_subject(value,(ROOM,"2","102"))

    def test_current_saved_checkout_is_not_reconstructed_from_continuity(self):
        value, worker = self.current()
        worker["certified_checkout"]["record"] = "saved:restored-notebook"
        self.assertEqual(app.mapped_record(value, (ROOM,"2","102"), app.BROWSER),
                         "saved:restored-notebook")
        self.assertEqual(worker["record_instance"], 12)

    def test_no_checkout_proof_never_falls_back_to_a_derived_record(self):
        value, worker = self.current()
        worker.pop("certified_checkout")
        self.assertIsNone(app.mapped_record(value, (ROOM,"2","102"), app.BROWSER))

    def test_foreign_or_malformed_certified_checkout_refuses(self):
        for field, bad in [("application",app.STORE),("room","foreign"),
                           ("record",""),("record","bad\x1froute"),("record",True)]:
            value, worker = self.current()
            worker["certified_checkout"][field] = bad
            with self.assertRaises(VmError):
                app.mapped_record(value,(ROOM,"2","102"),app.BROWSER)

class FreshBrowser(unittest.TestCase):
    def vm(self, new_record=True, new_surface=True):
        class VM:
            stage = 0
            invoked = []

            def state(self):
                tabs=(1, 2) if self.stage and new_surface else (1,)
                value=state(*tabs)
                value["chrome"]={"session_locked":False,"native_window_provenance":{"method":"authenticated-wayland-peer", "omitted":0,
                    "windows":[{"room":ROOM, "tab_id":tab, "surface_id":tab+100, "package_id":app.BROWSER,
                                "certified_checkout":{"application":app.BROWSER,"room":ROOM,"record":f"derived:default@tab:{tab+10:016x}"},
                                "record_instance":tab+10, "peer_pid":400+tab, "peer_start_time_ticks":str(1000+tab),
                                "adoption_id":f"12345678-1234-1234-1234-{tab:012x}"} for tab in tabs]}}
                return value

            def actions(self, query):
                return [{"provider": "actions", "id": "tab.open-beside"}]

            def invoke(self, provider, identity, args):
                self.invoked.append((provider, identity, args))
                self.stage = 1
                return {"receipt": {"update": {"state": "succeeded"}}}

            def control_json(self, *args):
                rows = [row()]
                if self.stage and new_record:
                    rows.append(row("derived:default@tab:000000000000000c"))
                return {"query": app.BROWSER, "actions": rows}
        return VM()

    def test_only_new_mapped_record_becomes_target(self):
        vm = self.vm()
        with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
            door, tab = app.fresh_browser(vm, Path(directory))
        self.assertEqual(door.bound[2], "derived:default@tab:000000000000000c")
        self.assertEqual(tab, ("room", "2", "102"))
        self.assertEqual(vm.invoked, [("actions", "tab.open-beside", {"app": app.BROWSER})])

    def test_open_success_without_new_record_or_surface_aborts(self):
        for record, surface in ((False, False), (False, True), (True, False)):
            vm = self.vm(record, surface)
            with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
                with self.assertRaises(VmError):
                    app.fresh_browser(vm, Path(directory))
            self.assertEqual(len(vm.invoked), 1)
            self.assertEqual(vm.invoked[0][1], "tab.open-beside")

    def test_unrelated_existing_records_cold_publish_without_ambiguity(self):
        vm = self.vm()
        original = vm.control_json
        def catalogue(*args):
            value = original(*args)
            if vm.stage:
                value["actions"].append(row("derived:default@tab:0000000000000099"))
            return value
        vm.control_json = catalogue
        with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
            door, tab = app.fresh_browser(vm, Path(directory))
        self.assertEqual(door.bound[2], "derived:default@tab:000000000000000c")
        self.assertEqual(tab[1], "2")
        self.assertEqual(len(vm.invoked), 1)

    def test_missing_foreign_or_invalid_public_association_never_guesses_tab_identity(self):
        for association in (None, {"room": "other", "app_id": app.BROWSER, "record_instance": 12},
                            {"room": "room", "app_id": app.BROWSER, "record_instance": True}):
            vm = self.vm()
            original = vm.state
            def current():
                value = original()
                if association is None:
                    value["workspace_tree"]["local_app_records"].pop("2", None)
                else:
                    value["workspace_tree"]["local_app_records"]["2"] = association
                return value
            vm.state = current
            with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
                with self.assertRaises(VmError):
                    app.fresh_browser(vm, Path(directory))
            self.assertEqual(len(vm.invoked), 1)

    def test_host_prefix_does_not_authorize_different_https_site(self):
        self.assertTrue(app.example_url("https://example.com/"))
        for url in ("https://example.com.evil/", "https://user@example.com/", "http://example.com/", None):
            self.assertFalse(app.example_url(url))


class ResidentPackage(unittest.TestCase):
    def test_selected_worker_requires_exact_pid_lifetime_and_preserves_unrelated_report(self):
        import copy
        import json
        from unittest.mock import Mock
        vm = Mock()
        selected = {"peer_pid": 222, "peer_start_time_ticks": "999"}
        data = {"method": "proc-map-files-fd", "reader_uid": 0, "uid": 1000,
                "processes": [{"pid": 222, "start_time_ticks": "999", "runtime": [
                    {"path": "/run/tilo-runtime/sdk/lib/libruntime_dylib.so", "sha256": "sdk"}],
                    "executable": {"sha256": "app"}}],
                "unrelated_processes": [{"pid": 111, "status": "unrelated_not_verified_against_selected_offer"}]}
        for field, value in (("pid", 111), ("start_time_ticks", "1000")):
            foreign = copy.deepcopy(data)
            foreign["processes"][0][field] = value
            vm.root_exec.return_value = json.dumps(foreign)
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                with self.assertRaisesRegex(VmError, "foreign worker"):
                    app.mapped(vm, "calculator", {}, 1000, Path(directory), selected=selected)
        vm.root_exec.return_value = json.dumps(data)
        with tempfile.TemporaryDirectory() as directory:
            app.mapped(vm, "calculator", {"/run/tilo-runtime/sdk/lib/libruntime_dylib.so": "sdk"},
                       1000, Path(directory), executable="app", selected=selected)
            report = json.loads((Path(directory) / "calculator-mapped-sdk.json").read_text())
            self.assertEqual(report["unrelated_processes"], data["unrelated_processes"])
            self.assertIn(" 222 999", vm.root_exec.call_args.args[0])

    def test_old_executable_fails_even_when_runtime_matches(self):
        import json
        from unittest.mock import Mock
        vm = Mock()
        data = {"method": "proc-map-files-fd", "reader_uid": 0, "uid": 1000,
                "processes": [{"runtime": [{"path": "/run/tilo-runtime/sdk/lib/libruntime_dylib.so", "sha256": "verified-sdk"}],
                               "executable": {"sha256": "old-app"}}]}
        vm.root_exec.return_value = json.dumps(data)
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(VmError):
                app.mapped(vm, "calculator", {"/run/tilo-runtime/sdk/lib/libruntime_dylib.so": "verified-sdk"}, 1000, Path(directory),
                           executable="new-app")
            data["processes"][0]["executable"]["sha256"] = "new-app"
            vm.root_exec.return_value = json.dumps(data)
            app.mapped(vm, "calculator", {"/run/tilo-runtime/sdk/lib/libruntime_dylib.so": "verified-sdk"}, 1000, Path(directory),
                       executable="new-app")
            self.assertTrue((Path(directory) / "calculator-mapped-sdk.json").exists())


class Store(unittest.TestCase):
    def test_busy_readback_is_not_settled(self):
        door = app.AppDoor(None, app.STORE, "store.catalog", (), Path('.'))
        values = [{"worker_attached": True, "worker_busy": True, "pending_jobs": 1},
                  {"worker_attached": True, "worker_busy": False, "pending_jobs": 0}]
        with patch.object(door, "call", side_effect=values) as call, patch.object(app.time, "sleep"):
            self.assertEqual(door.settled(), values[-1])
        self.assertEqual(call.call_count, 2)

    def test_installed_calculator_never_becomes_first_install(self):
        class Door:
            def call(self, *args, **kwargs):
                self.fail("must not invoke")
        with self.assertRaises(VmError):
            app.flatpak_flow(Door(), {"flatpak_apps": [app.CALCULATOR]})

    def test_mutated_review_identity_aborts_before_install(self):
        class VM:
            def state(self):
                return state()
        class Door:
            vm = VM()
            calls = []

            def call(self, op, args):
                self.calls.append(op)

            def settled(self, app_id):
                return {"imported_package": {"app": app_id, "root": "f" * 64, "version": "1"}}
        door = Door()
        with self.assertRaises(VmError):
            app.package_flow(door, {"app": "so.tilo.calculator", "root": "a" * 64, "version": "1", "path": "/review.tapp"}, True)
        self.assertEqual(door.calls, ["import-package"])


if __name__ == "__main__":
    unittest.main()
