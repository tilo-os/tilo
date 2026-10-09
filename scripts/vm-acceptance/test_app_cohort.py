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


ROOM = "5ba25f73-87c5-447f-a5a6-880dc4249a81"

def response(args, rows):
    scope = {key: args[args.index(flag)+1] for key, flag in
             (("application", "--application"), ("room", "--room"),
              ("record", "--record"), ("semantic_family", "--semantic-family"))} if "--application" in args else None
    return dict(query="", completion=dict(complete=True, incomplete=[]), scope=scope, count=len(rows), actions=rows)

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
    return {"workspace_tree": {"local_app_records": {str(tab): {"room": ROOM,
            "app_id": app.BROWSER, "record_instance": tab + 10} for tab in tabs}, "workspaces": [{"room": ROOM, "strips": [{
        "window_groups": [{"tabs": [{"id": tab, "tile": {"Foreign": {
            "app_id": app.BROWSER, "surface_id": tab + 100}}} for tab in tabs]}]}]}]}}


def row(record="derived:default@tab:000000000000000b", operation="navigate"):
    return {"provider": "app-actions", "application": app.BROWSER,
            "room": ROOM, "record": record, "resource": "bound:" + record,
            "id": record + ":" + operation, "operation": operation,
            "semantic_identity": {"kind": "browser.page", "key": "browser.page"}, "params": [],
            "contract": {"availability": {"state": "available"}}}


class ApplicationMetadataDiscovery(unittest.TestCase):
    def test_application_identifier_is_metadata_not_a_text_query(self):
        descriptor = row()
        descriptor["title"] = "Read the page"
        class VM:
            def control_json(self, *args):
                words = [word for word in args[2:] if not word.startswith("--")]
                query = " ".join(words)
                rows = [] if query else [descriptor, dict(descriptor, application=app.STORE)]
                return {"query": query, "completion": {"complete": True, "incomplete": []}, "scope": None, "count": len(rows), "actions": rows}
        self.assertEqual(app.catalog(VM(), app.BROWSER), [descriptor])


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

    def test_clean_source_explicit_null_selects_the_exact_installed_sdk(self):
        for login in (True, False):
            receipt, shown = self.fixture(login)
            receipt["cohort_source"]["dirty_tree_hash"] = None
            for member in receipt["cohort_members"]:
                member["build"]["dirty_tree_hash"] = None
            original, current, installed = self.run_cohort(receipt, shown)
            self.assertEqual(original, receipt)
            self.assertIsNone(original["cohort_source"]["dirty_tree_hash"])
            self.assertEqual(current, shown)
            self.assertEqual(installed["/run/tilo-runtime/native/lib/libruntime_dylib.so"], "e" * 64)
            individual = {"tool": "tilo-vm", "status": "success", "component": "apps+runtime",
                          "vm": "owned", "slot": "device",
                          "cohort_source": dict(receipt["cohort_source"], component="apps+runtime"),
                          "layer": shown["slot"]["components"]["apps+runtime"]}
            self.assertEqual(self.run_cohort(individual, shown)[0], individual)

    def test_missing_or_malformed_dirty_provenance_refuses(self):
        for value in ("", "b" * 63, "B" * 64, False, 0, [], {}):
            receipt, shown = self.fixture()
            receipt["cohort_source"]["dirty_tree_hash"] = value
            for member in receipt["cohort_members"]:
                member["build"]["dirty_tree_hash"] = value
            with self.subTest(value=value), self.assertRaises(VmError):
                self.run_cohort(receipt, shown)
        receipt, shown = self.fixture()
        receipt["cohort_source"].pop("dirty_tree_hash")
        with self.assertRaises(VmError):
            self.run_cohort(receipt, shown)

    def test_clean_source_requires_explicit_matching_member_provenance(self):
        for missing in (True, False):
            receipt, shown = self.fixture()
            receipt["cohort_source"]["dirty_tree_hash"] = None
            for member in receipt["cohort_members"]:
                member["build"]["dirty_tree_hash"] = None
            if missing:
                receipt["cohort_members"][0]["build"].pop("dirty_tree_hash")
            else:
                receipt["cohort_members"][0]["build"]["dirty_tree_hash"] = "b" * 64
            with self.subTest(missing=missing), self.assertRaises(VmError):
                self.run_cohort(receipt, shown)

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


class AdoptedAppDoor(unittest.TestCase):
    def subject(self):
        return {"room":ROOM, "tab_id":1, "surface_id":101, "package_id":app.STORE,
                "certified_checkout":{"application":app.STORE,"room":ROOM,"record":"derived:default@tab:000000000000000b"},
                "record_instance":11, "peer_pid":456, "peer_start_time_ticks":"12345",
                "adoption_id":"12345678-1234-1234-1234-123456789abc"}

    def descriptor(self, alias="initial"):
        value = row(operation="show-runtime-details")
        value.update(application=app.STORE, resource=alias, id=alias+":show-runtime-details",
                     semantic_identity={"kind":"store.catalog", "key":"store.catalog"},
                     params=[{"name":"runtime", "required":True, "type":"text"},
                             {"name":"registry", "required":False, "type":"text"}])
        return value

    def vm(self):
        owner=self
        class VM:
            descriptor=owner.descriptor()
            effects=[]
            worker=owner.subject()
            descriptors=None
            catalogue_sequence=None
            on_catalogue=None
            on_invoke=None
            gone=False
            def state(self):
                value=state(1)
                value["workspace_tree"]["local_app_records"]["1"]["app_id"]=app.STORE
                value["workspace_tree"]["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"][0]["tile"]["Foreign"]["app_id"]=app.STORE
                value["chrome"]={"session_locked":False,"native_window_provenance":{"method":"authenticated-wayland-peer", "omitted":0, "windows":[] if self.gone else [self.worker]}}
                return value
            def control_json(self,*args):
                if args[:2]==("actions","status"):return receipt("succeeded",value="reviewed")
                rows=([self.descriptor] if self.descriptors is None else self.descriptors)
                if self.catalogue_sequence is not None:rows=next(self.catalogue_sequence)
                result=response(args,copy.deepcopy(rows))
                if self.on_catalogue is not None:self.on_catalogue()
                return result
            def invoke(self,provider,identity,args):
                if identity!=self.descriptor["id"]:raise VmError("stale descriptor reached invocation")
                self.effects.append((provider,identity,args))
                if self.on_invoke is not None:self.on_invoke()
                return receipt("accepted")
        return VM()

    def test_same_owned_worker_rebinds_the_stable_resource_after_startup_adoption(self):
        vm=self.vm()
        with tempfile.TemporaryDirectory() as directory, patch.object(app,"wait_until",polls):
            door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
            vm.descriptor=self.descriptor("adopted")
            self.assertEqual(door.call("show-runtime-details",{"runtime":"tilo-runtime/test"}),"reviewed")
        self.assertEqual(vm.effects,[("app-actions","adopted:show-runtime-details",{"runtime":"tilo-runtime/test"})])


    def test_startup_null_identity_is_only_read_until_the_exact_resource_is_adopted(self):
        vm=self.vm()
        pending=self.descriptor();pending["semantic_identity"]=None
        vm.catalogue_sequence=iter([[pending],[self.descriptor()]])
        with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
            door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
            self.assertEqual(vm.effects,[])
            vm.catalogue_sequence=None
            self.assertEqual(door.call("show-runtime-details",{"runtime":"test"}),"reviewed")
        self.assertEqual(len(vm.effects),1)

    def test_original_public_semantic_identity_and_worker_allow_already_adopted_alias(self):
        vm=self.vm();original=app.binding(vm.descriptor);subject=copy.deepcopy(vm.worker)
        semantic=app.semantic_identity(vm.descriptor);vm.descriptor=self.descriptor("adopted-before-door")
        with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
            door=app.AppDoor(vm,app.STORE,"store.catalog",original,Path(directory),semantic,subject)
            self.assertEqual(door.call("show-runtime-details",{"runtime":"test"}),"reviewed")
        self.assertEqual(vm.effects[0][1],"adopted-before-door:show-runtime-details")

    def test_unknown_initial_alias_never_guesses_a_unique_current_child(self):
        vm=self.vm();original=app.binding(vm.descriptor);vm.descriptor=self.descriptor("other")
        with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
            with self.assertRaises(VmError):
                app.AppDoor(vm,app.STORE,"store.catalog",original,Path(directory))
        self.assertEqual(vm.effects,[])

    def test_foreign_revoked_missing_and_ambiguous_current_descriptors_never_invoke(self):
        for fault in ("foreign-app","foreign-room","foreign-record","foreign-child","null-identity",
                      "missing-key","revoked","ambiguous","missing"):
            vm=self.vm()
            with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
                door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
                if fault=="foreign-app":vm.descriptor["application"]=app.BROWSER
                elif fault=="foreign-room":vm.descriptor["room"]="other"
                elif fault=="foreign-record":vm.descriptor["record"]="other"
                elif fault=="foreign-child":vm.descriptor["semantic_identity"]["key"]="runtime:other"
                elif fault=="null-identity":vm.descriptor["semantic_identity"]=None
                elif fault=="missing-key":vm.descriptor["semantic_identity"].pop("key")
                elif fault=="revoked":vm.descriptor["contract"]["availability"]["state"]="unavailable"
                elif fault=="ambiguous":vm.descriptors=[vm.descriptor,self.descriptor("other-alias")]
                else:vm.descriptors=[]
                with self.subTest(fault=fault),self.assertRaises(VmError):
                    door.call("show-runtime-details",{"runtime":"test"})
                self.assertEqual(vm.effects,[])

    def test_replaced_closed_or_unattested_worker_never_inherits_the_original_door(self):
        for fault in ("pid","start","adoption","record","package","closed"):
            vm=self.vm()
            with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
                door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
                if fault=="pid":vm.worker["peer_pid"]=789
                elif fault=="start":vm.worker["peer_start_time_ticks"]="999"
                elif fault=="adoption":vm.worker["adoption_id"]="22345678-1234-1234-1234-123456789abc"
                elif fault=="record":vm.worker["record_instance"]=12
                elif fault=="package":vm.worker["package_id"]=app.BROWSER
                else:vm.gone=True
                with self.subTest(fault=fault),self.assertRaises(VmError):
                    door.call("show-runtime-details",{"runtime":"test"})
                self.assertEqual(vm.effects,[])

    def test_worker_changes_during_discovery_refuse_before_effect_and_after_effect_never_replay(self):
        for phase in ("catalogue","effect"):
            vm=self.vm()
            with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
                door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
                def replace():vm.worker["peer_start_time_ticks"]="999"
                if phase=="catalogue":vm.on_catalogue=replace
                else:vm.on_invoke=replace
                with self.subTest(phase=phase),self.assertRaises(VmError):
                    door.call("show-runtime-details",{"runtime":"test"})
                self.assertEqual(len(vm.effects),0 if phase=="catalogue" else 1)

    def test_unavailable_owner_diagnostic_or_locked_context_never_invoke(self):
        for fault in ("missing-owner-proof","foreign-method","locked","unknown-lock"):
            vm=self.vm();current=vm.state
            def denied():
                value=current()
                if fault=="missing-owner-proof":value["chrome"].pop("native_window_provenance")
                elif fault=="foreign-method":value["chrome"]["native_window_provenance"]["method"]="caller-label"
                elif fault=="locked":value["chrome"]["session_locked"]=True
                else:value["chrome"].pop("session_locked")
                return value
            vm.state=denied
            with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
                with self.subTest(fault=fault),self.assertRaises(VmError):
                    app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
            self.assertEqual(vm.effects,[])

    def test_genuine_other_worker_subject_cannot_authorize_the_bound_record(self):
        vm=self.vm();original=copy.deepcopy(vm.worker)
        vm.descriptor["record"]="derived:default@tab:000000000000000c"
        current=vm.state
        def two_store_windows():
            value=current()
            records=value["workspace_tree"]["local_app_records"]
            records["2"]={"room":ROOM,"app_id":app.STORE,"record_instance":12}
            tabs=value["workspace_tree"]["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"]
            tabs.append({"id":2,"tile":{"Foreign":{"app_id":app.STORE,"surface_id":102}}})
            value["chrome"]["native_window_provenance"]["windows"].append(
                dict(original,tab_id=2,surface_id=102,record_instance=12,peer_pid=789))
            return value
        vm.state=two_store_windows
        with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
            with self.assertRaises(VmError):
                app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory),
                            app.semantic_identity(vm.descriptor),original)
        self.assertEqual(vm.effects,[])

    def test_each_call_uses_one_new_authorized_alias_and_tracks_its_original_job(self):
        vm=self.vm()
        with tempfile.TemporaryDirectory() as directory,patch.object(app,"wait_until",polls):
            door=app.AppDoor(vm,app.STORE,"store.catalog",app.binding(vm.descriptor),Path(directory))
            for alias in ("adopted-one","adopted-two"):
                vm.descriptor=self.descriptor(alias)
                self.assertEqual(door.call("show-runtime-details",{"runtime":"test"}),"reviewed")
                self.assertEqual(door.bound[3],alias)
        self.assertEqual([effect[1] for effect in vm.effects],
                         ["adopted-one:show-runtime-details","adopted-two:show-runtime-details"])


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

    def test_executable_must_be_unique_in_the_current_selected_signed_layer(self):
        row={"component":"apps+runtime","layer":"a"*64,"path":"/usr/share/tilo-apps/pkg/bin/browser","sha256":"b"*64}
        self.assertEqual(app.browser_executable({"files":[row]},"a"*64),"b"*64)
        for rows in [[],[dict(row,layer="c"*64)],[row,row],[dict(row,link="elsewhere")],[dict(row,sha256=None)]]:
            with self.assertRaises(VmError):app.browser_executable({"files":rows},"a"*64)


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
                if "--record" in args:
                    rows = [r for r in rows if r["record"] == args[args.index("--record")+1]]
                return response(args, rows)
        return VM()

    def test_only_new_mapped_record_becomes_target(self):
        vm = self.vm()
        with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
            door, tab = app.fresh_browser(vm, Path(directory))
        self.assertEqual(door.bound[2], "derived:default@tab:000000000000000c")
        self.assertEqual(tab, (ROOM, "2", "102"))
        self.assertEqual(vm.invoked, [("actions", "tab.open-beside", {"app": app.BROWSER, "background": True})])

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
            if vm.stage and "--record" not in args:
                value["actions"].append(row("derived:default@tab:0000000000000099"))
                value["count"] = len(value["actions"])
            return value
        vm.control_json = catalogue
        with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
            door, tab = app.fresh_browser(vm, Path(directory))
        self.assertEqual(door.bound[2], "derived:default@tab:000000000000000c")
        self.assertEqual(tab[1], "2")
        self.assertEqual(len(vm.invoked), 1)

    def test_missing_foreign_or_invalid_public_association_never_guesses_tab_identity(self):
        for association in (None, {"room": "other", "app_id": app.BROWSER, "record_instance": 12},
                            {"room": ROOM, "app_id": app.BROWSER, "record_instance": True}):
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
            bound = (app.BROWSER, ROOM, "new-record", "page")
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
