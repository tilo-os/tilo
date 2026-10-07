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


def state(*tabs):
    return {"workspace_tree": {"workspaces": [{"room": "room", "strips": [{
        "window_groups": [{"tabs": [{"id": tab, "tile": {"Foreign": {
            "app_id": app.BROWSER, "surface_id": tab + 100}}} for tab in tabs]}]}]}]}}


def row(record="old", operation="navigate"):
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
                    rows.append(row("new"))
                return {"query": app.BROWSER, "actions": rows}
        return VM()

    def test_only_new_mapped_record_becomes_target(self):
        vm = self.vm()
        with tempfile.TemporaryDirectory() as directory, patch.object(app, "wait_until", polls):
            door, tab = app.fresh_browser(vm, Path(directory))
        self.assertEqual(door.bound[2], "new")
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
