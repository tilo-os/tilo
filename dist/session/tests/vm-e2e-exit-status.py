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

    def exercise(self, app_state, settings=None, driven_cards=2, journey=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            rpms = root / "rpms"
            rpms.mkdir()
            for package in ("tilo-session-1.rpm", "tilo-session-apps-1.rpm"):
                (rpms / package).write_bytes(b"fixture")
            args = SimpleNamespace(out=str(root / "out"), rpms=str(rpms), memory=6144,
                                   cpus=4, dwell=0, settle=0, work=str(root / "vm"),
                                   image_cache=str(root / "images"), apps="",
                                   journey=bool(journey))
            noop = lambda *args, **kwargs: None
            typed = []
            layouts = []

            def type_text(text, layout="us"):
                typed.append(text)
                layouts.append(layout)

            imported = settings if settings is not None else {
                "input": {**harness.IMPORTED_INPUT, harness.IMPORT_MARKER: 4}}
            qmp = SimpleNamespace(keys=noop, type_text=type_text, screendump=noop)
            steps = []

            def run_guest(*args, **kwargs):
                steps.append(args)
                answer = (journey or {}).get(args[0])
                if answer is None:
                    return SimpleNamespace(returncode=0, stdout="fixture OK")
                if isinstance(answer, tuple):
                    return SimpleNamespace(returncode=answer[0], stdout=answer[1])
                return SimpleNamespace(returncode=answer, stdout="fixture OK")

            guest = SimpleNamespace(
                qmp=qmp, wait_ssh=noop, ssh=noop, scp_to=noop, stop=noop,
                alive=lambda: False,
                settings=lambda: json.dumps(imported),
                guest=run_guest,
                status=lambda apps: {"compositor_pid": "" if ("logout",) in steps else "100",
                                     "modeset": 1,
                                     "driven_cards": str(driven_cards),
                                     "topology": f"DRM topology cards=2 driven_cards={driven_cards} outputs=2",
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
            self.steps = steps
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
        self.assertEqual(len(report["checks"]), 10)
        self.assertNotIn("uninstall", [step[0] for step in self.steps],
                         "the app-only run must not touch the host again")
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

    def test_a_session_lighting_only_one_of_the_two_gpus_fails(self):
        status, report = self.exercise("healthy", driven_cards=1)
        self.assertEqual(status, 1)
        failed = [c["id"] for c in report["checks"] if c["status"] == "fail"]
        self.assertEqual(failed, ["H06"])

    # The journey after the apps: an in-place update, signing back in to the
    # desktop the host had before Tilo, and removing the packages. A guest that
    # says nothing about a step succeeds at it, except for the two steps whose
    # answer decides the journey; these are the answers a healthy host gives.
    def journey(self, **answers):
        healthy = {"previous-available": (0, "guest: a previous desktop is installed"),
                   "previous-probe": (0, "guest: GNOME Shell started and showed a frame"),
                   # The host's own default before Tilo was added, which the
                   # uninstall has to leave alone.
                   "session-default": (0, "Session=gnome.desktop")}
        healthy.update({name.replace("_", "-"): answer for name, answer in answers.items()})
        return self.exercise("healthy", settings=None, journey=healthy)

    def test_a_complete_journey_passes_and_names_what_it_exercised(self):
        status, report = self.journey()
        checks = {c["id"]: c for c in report["checks"]}
        self.assertEqual([c["id"] for c in report["checks"] if c["status"] == "fail"], [])
        self.assertEqual(status, 0)
        self.assertEqual(checks["S05"]["status"], "pass")
        self.assertEqual(checks["S09"]["status"], "pass")
        steps = [step[0] for step in self.steps]
        self.assertIn("uninstall", steps)
        self.assertLess(steps.index("session-autologin"), steps.index("uninstall"))
        self.assertEqual([step for step in self.steps if step[0] == "session-autologin"],
                         [("session-autologin", harness.PREVIOUS_SESSION),
                          ("session-autologin", harness.TILO_SESSION)],
                         "the previous desktop is chosen first, then Tilo is removed")

    def test_a_host_with_no_previous_desktop_fails_rather_than_skips(self):
        status, report = self.journey(previous_available=(1, "guest: no previous desktop on this host: gnome-shell"))
        checks = {c["id"]: c for c in report["checks"]}
        self.assertEqual(status, 1)
        self.assertEqual(checks["S04"]["status"], "fail")
        self.assertNotIn("S05", checks, "nothing can be signed in to, so nothing is claimed")
        self.assertIn("uninstall", [step[0] for step in self.steps],
                      "the uninstall is still exercised without another desktop")

    def test_a_previous_desktop_that_starts_but_shows_nothing_fails(self):
        status, report = self.journey(
            previous_probe=(3, "guest: GNOME Shell started but showed no frame"))
        checks = {c["id"]: c for c in report["checks"]}
        self.assertEqual(status, 1)
        self.assertEqual(checks["S05"]["status"], "fail")
        self.assertIn("not verified", checks["S05"]["detail"])
        self.assertNotIn("uninstall", [step[0] for step in self.steps])

    def test_the_comparisons_name_what_the_update_and_the_uninstall_own(self):
        status, report = self.journey(previous_available=(1, "guest: no previous desktop"))
        compared = [step for step in self.steps if step[0] == "record-check"]
        self.assertEqual(compared[0][1:3], ("upgraded", "installed"))
        self.assertEqual(compared[0][3:], ("--kept", "tilo-session", "--kept", "tilo-session-apps"))
        self.assertEqual(compared[1][1:3], ("uninstalled", "installed"))
        self.assertIn("--gone", compared[1])
        self.assertIn(harness.TILO_SESSION, compared[1])
        checks = {c["id"]: c for c in report["checks"]}
        self.assertEqual(status, 1, "a host with no previous desktop cannot pass the journey")
        self.assertEqual(checks["S01b"]["status"], "pass", "an update keeps both Tilo packages")


if __name__ == "__main__":
    unittest.main()
