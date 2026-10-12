#!/usr/bin/env python3
"""Exercise the real VM acceptance harness against controlled guest responses."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "vm-e2e/run.py"
spec = importlib.util.spec_from_file_location("vm_e2e", SCRIPT)
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)


class VmAcceptanceExitTests(unittest.TestCase):
    def test_fixture_password_uses_supported_qmp_hyphen_key(self):
        guest_source = (SCRIPT.parent / "guest.sh").read_text()
        self.assertIn(f"password={harness.GUEST_PASSWORD}\n", guest_source)
        qmp = harness.Qmp.__new__(harness.Qmp)
        keys = []
        qmp.keys = lambda *args: keys.extend(args)
        qmp.type_text(harness.GUEST_PASSWORD)
        self.assertEqual(keys.count("minus"), 2)
        self.assertNotIn("-", keys)

    def exercise(self, app_state, settings=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            rpms = root / "rpms"
            rpms.mkdir()
            for package in ("tilo-session-1.rpm", "tilo-session-apps-1.rpm"):
                (rpms / package).write_bytes(b"fixture")
            args = SimpleNamespace(out=str(root / "out"), rpms=str(rpms), memory=6144,
                                   cpus=4, dwell=0, settle=0, work=str(root / "vm"),
                                   image_cache=str(root / "images"), apps="")
            noop = lambda *args, **kwargs: None
            typed = []
            layouts = []

            def type_text(text, layout="us"):
                typed.append(text)
                layouts.append(layout)

            imported = settings if settings is not None else {
                "input": {**harness.IMPORTED_INPUT, harness.IMPORT_MARKER: 4}}
            qmp = SimpleNamespace(keys=noop, type_text=type_text, screendump=noop)
            guest = SimpleNamespace(
                qmp=qmp, wait_ssh=noop, ssh=noop, scp_to=noop, stop=noop,
                alive=lambda: False,
                settings=lambda: json.dumps(imported),
                guest=lambda *args, **kwargs: SimpleNamespace(returncode=0, stdout="fixture OK"),
                status=lambda apps: {"compositor_pid": "100", "modeset": 1,
                                     **{f"app.{app}": int(app_state != "missing") for app in apps},
                                     **{f"app_stopped.{app}": int(app_state == "stopped") for app in apps}},
            )
            ticks = iter(range(10000))
            with patch.object(harness, "Vm", return_value=guest), \
                    patch.object(harness, "fetch_image", return_value=root / "fixture.qcow2"), \
                    patch.object(harness, "log", noop), \
                    patch.object(harness.time, "sleep", noop), \
                    patch.object(harness.time, "monotonic", side_effect=lambda: next(ticks)):
                status = harness.run_test(args)
            self.assertEqual(typed[0], harness.GUEST_PASSWORD,
                             "normal authentication must precede every app search")
            report = json.loads((root / "out/report.json").read_text())
            self.layouts = layouts
            return status, report

    def test_missing_apps_fail_process_and_report(self):
        status, report = self.exercise("missing")
        self.assertEqual(report["overall"], "fail")
        self.assertEqual(len([c for c in report["checks"] if c["status"] == "fail"]), 4)
        self.assertEqual(status, 1)

    def test_apps_stopping_before_window_fail_process_and_report(self):
        status, report = self.exercise("stopped")
        self.assertEqual(report["overall"], "fail")
        self.assertEqual(status, 1)

    def test_healthy_session_and_all_apps_pass_process_and_report(self):
        status, report = self.exercise("healthy")
        self.assertEqual(report["overall"], "pass")
        self.assertEqual(len(report["checks"]), 9)
        self.assertEqual(status, 0)
        self.assertEqual(set(self.layouts), {"de"},
                         "everything is typed on the German layout the session imported")

    def test_a_session_that_did_not_import_fails_and_still_tries_the_apps(self):
        status, report = self.exercise("healthy", settings={"input": {"keyboard_layout": "us",
                                                                      "keyboard_layouts": ["us"]}})
        self.assertEqual(status, 1)
        self.assertEqual(report["overall"], "fail")
        failed = [c["id"] for c in report["checks"] if c["status"] == "fail"]
        self.assertEqual(failed, ["H05"])
        self.assertEqual(set(self.layouts), {"us"})


if __name__ == "__main__":
    unittest.main()
