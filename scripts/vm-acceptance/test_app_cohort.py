"""Exercise receipt transitions and fresh-instance safety in the production runner."""
import copy
import json
from types import SimpleNamespace
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


class ActivatedCohort(unittest.TestCase):
    # Wire shape from tilo-vm-protocol::{UserspaceResult, UserspaceMember,
    # ApplyResult, SlotShowResult}; both Receipt::finish and cohort_activation
    # retain the same result, rather than a fabricated individual receipt.
    def fixture(self, login=True):
        components = ("launcher", "install", "apps+runtime", "app-host", "native-runtime-host",
                      "control", "app-cli", "ush", "shell")
        source = {"component": "userspace", "authority": "local", "source_commit": "a" * 40,
                  "dirty_tree_hash": "b" * 64, "cargo_lock_hash": "c" * 64, "builder": "remote"}
        members = []
        for index, component in enumerate(components):
            build = dict(source, component=component, target="aarch64-unknown-linux-gnu",
                         toolchain="rustc pinned", cargo_profile="thin-app" if component == "apps+runtime" else "release")
            members.append({"component": component, "layer": f"{index + 1:064x}",
                            "build": build, "source_inventory": "d" * 64})
        identity = {"user": "owner", "unit": "tilo-session@owned.service",
                    "login_session": "login" if login else None, "login_wrapper": 123 if login else None}
        actions = [{"action": "restart_desktop", "user": "owner"}] if login else [
            {"action": "stop_unit", "unit": identity["unit"]},
            {"action": "restart_unit", "unit": "user@1002.service"},
            {"action": "start_unit", "unit": identity["unit"]}]
        activation = {"apply": {"slot": "device", "generation": 12, "changed": True,
                                 "policy": "session_restart", "actions": actions, "warnings": []},
                      "source": source, "source_inventory": "d" * 64, "members": members,
                      "phases": [{"name": "refresh", "seconds": 0.5}], "failure": None}
        receipt = {"tool": "tilo-vm", "status": "success", "component": "userspace",
                   "vm": "owned", "slot": "device", "cohort_source": source,
                   "source_inventory": "d" * 64, "cohort_members": members,
                   "cohort_activation": activation, "result": dict(activation, next=[], warnings=[])}
        layer = members[2]["layer"]
        shown = {"slot": {"name": "device", "session": "active", "generation": 12,
                           "user": "owner", "session_identity": identity,
                           "components": {m["component"]: m["layer"] for m in members}},
                 "files": [{"component": "apps+runtime", "layer": layer,
                            "path": "/usr/lib/tilo-runtime/native/lib/libruntime_dylib.so", "sha256": "e" * 64},
                           {"component": "apps+runtime", "layer": layer,
                            "path": "/usr/lib/tilo-runtime/native/lib/libstd-native.so", "sha256": "f" * 64}]}
        return receipt, shown

    def run_cohort(self, receipt, shown):
        class VM:
            def run(self, *args):
                self.args = args
                return json.dumps(shown)
        vm = VM()
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            file = out / "run.json"
            file.write_text(json.dumps(receipt))
            result = app.cohort(vm, SimpleNamespace(cohort_receipt=file, vm="owned", slot="device"), out)
            self.assertEqual(vm.args, ("--json", "slot", "show"))
            self.assertEqual(json.loads(file.read_text()), receipt, "never relabel original aggregate receipt")
            return result

    def test_successful_userspace_wire_receipt_selects_its_current_sdk_without_relabeling(self):
        for login in (True, False):
            receipt, shown = self.fixture(login)
            original, current, installed = self.run_cohort(receipt, shown)
            self.assertEqual(original, receipt)
            self.assertEqual(original["component"], "userspace")
            self.assertNotIn("layer", original)
            self.assertEqual(current, shown)
            self.assertEqual(installed["/run/tilo-runtime/native/lib/libruntime_dylib.so"], "e" * 64)

    def test_individual_apps_runtime_wire_receipt_remains_supported(self):
        receipt, shown = self.fixture()
        receipt = {"tool": "tilo-vm", "status": "success", "component": "apps+runtime",
                   "vm": "owned", "slot": "device", "cohort_source": dict(receipt["cohort_source"], component="apps+runtime"),
                   "layer": shown["slot"]["components"]["apps+runtime"]}
        original, _, _ = self.run_cohort(receipt, shown)
        self.assertEqual(original, receipt)

    def test_partial_stale_foreign_or_incoherent_aggregate_refuses_before_app_actions(self):
        mutations = [
            lambda r, s: r.update(status="failure"),
            lambda r, s: r.update(vm="foreign"),
            lambda r, s: r["cohort_activation"].update(failure="selection failed"),
            lambda r, s: r["cohort_activation"].pop("failure"),
            lambda r, s: r["result"].update(source_inventory="0" * 64),
            lambda r, s: r["cohort_members"].pop(),
            lambda r, s: r["cohort_members"].reverse(),
            lambda r, s: r["cohort_members"][0].update(source_inventory="0" * 64),
            lambda r, s: r["cohort_members"][0]["build"].update(source_commit="0" * 40),
            lambda r, s: r["cohort_members"][0]["build"].update(cargo_lock_hash="0" * 64),
            lambda r, s: r["cohort_members"][0]["build"].update(target="x86_64-unknown-linux-gnu"),
            lambda r, s: r["cohort_members"][2]["build"].update(cargo_profile="release"),
            lambda r, s: s["slot"].update(generation=13),
            lambda r, s: s["slot"].update(generation=True),
            lambda r, s: s["slot"].update(name="foreign"),
            lambda r, s: s["slot"].update(session="failed"),
            lambda r, s: s["slot"].update(user="foreign"),
            lambda r, s: s["slot"].pop("session_identity"),
            lambda r, s: s["slot"]["components"].update(shell="0" * 64),
            lambda r, s: r["cohort_activation"]["apply"].update(slot="foreign"),
            lambda r, s: r["cohort_activation"]["apply"].update(policy="reboot"),
            lambda r, s: r["cohort_activation"]["apply"]["actions"].append({"action": "deferred"}),
            lambda r, s: r["cohort_activation"]["apply"]["actions"][0].update(user="foreign"),
            lambda r, s: r["cohort_activation"]["apply"].update(actions=[]),
            lambda r, s: s.update(files=[]),
            lambda r, s: s["files"][0].pop("sha256"),
            lambda r, s: s["files"].append(copy.deepcopy(s["files"][0])),
            lambda r, s: s["files"][0].update(path="/usr/lib/tilo-runtime/../foreign/lib/libruntime_dylib.so"),
        ]
        for mutation in mutations:
            receipt, shown = self.fixture()
            mutation(receipt, shown)
            with self.subTest(mutation=mutation), self.assertRaises(VmError):
                self.run_cohort(receipt, shown)


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


class FreshBrowser(unittest.TestCase):
    def vm(self, new_record=True, new_surface=True):
        class VM:
            stage = 0
            invoked = []

            def state(self):
                return state(1, 2) if self.stage and new_surface else state(1)

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
        self.assertTrue(app.fixture_url(app.HTTPS_FIXTURE["url"]))
        for url in ("https://wpt.live.evil/", "https://user@wpt.live/", "http://wpt.live/", app.HTTPS_FIXTURE["url"] + "?other=1", "https://wpt.live/elsewhere", None):
            self.assertFalse(app.fixture_url(url))


class HttpsReadback(unittest.TestCase):
    def page(self):
        fixture = app.HTTPS_FIXTURE
        return {"provenance": "web-content", "url": fixture["url"], "title": fixture["title"],
                "outline": '- heading ' + __import__('json').dumps(fixture["heading"]) + ' level=1\n' +
                    '\n'.join('- listitem: link ' + __import__('json').dumps(link["name"]) + ' [ref=d1e2] -> ' + link["url"] for link in fixture["links"])}

    def test_exact_live_fixture_roles_and_links_are_required(self):
        app.verify_https_page(self.page())
        for field, replacement in (("provenance", "test-engine"), ("title", "TLS error"),
                ("url", app.HTTPS_FIXTURE["url"] + "?other=1"), ("outline", app.HTTPS_FIXTURE["heading"])):
            page = self.page(); page[field] = replacement
            with self.assertRaises(VmError):
                app.verify_https_page(page)
        for old, new in (("level=1", "level=2"), ("[ref=d1e2]", "[ref=guessed]"),
                ("https://wpt.live/", "https://wpt.live.evil/")):
            page = self.page(); page["outline"] = page["outline"].replace(old, new)
            with self.assertRaises(VmError):
                app.verify_https_page(page)

    def test_navigate_then_settle_then_read_and_refuse_transport_only_success(self):
        class Door:
            bound = (app.BROWSER, "room", "new-record", "page")
            def call(self, *args):
                return next(self.results)
        door = Door()
        for results in ([app.HTTPS_FIXTURE["url"], {"data": {"settled": False}}, self.page()],
                        [app.HTTPS_FIXTURE["url"], {"data": {"settled": True}}, {"title": app.HTTPS_FIXTURE["title"]}]):
            door.results = iter(results)
            with self.assertRaises(VmError):
                app.browser_flow(door)
        door.results = iter([app.HTTPS_FIXTURE["url"], {"data": {"settled": True}}, self.page()])
        self.assertFalse(app.browser_flow(door)["latency_claim"])


class ResidentPackage(unittest.TestCase):
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
