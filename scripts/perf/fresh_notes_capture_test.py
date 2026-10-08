"""Guards the actual fresh-worker capture orchestration without device access."""
import argparse
import copy
import importlib.util
import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest

import fresh_notes_capture as fresh
from device_notes_latency import require_keyboard_notes

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/vm-acceptance"))
spec = importlib.util.spec_from_file_location("capture_cohort", ROOT / "scripts/vm-acceptance/app-cohort.py")
cohort = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cohort)


def state(entries):
    mapping = {str(tab): dict(room="room", app_id=fresh.APP, record_instance=record)
               for tab, record, _ in entries}
    return dict(chrome=dict(session_locked=False, native_window_provenance=dict(
        method="authenticated-wayland-peer", omitted=0, windows=[dict(
            room="room", tab_id=tab, surface_id=surface, package_id=fresh.APP,
            record_instance=record, peer_pid=100 + tab, peer_start_time_ticks="123",
            adoption_id="12345678-1234-1234-1234-123456789abc",
            certified_checkout=dict(application=fresh.APP, room="room",
                                    record=f"saved:notes:{record}"))
            for tab, record, surface in entries])), workspace_tree=dict(
        local_app_records=mapping, workspaces=[dict(room="room", strips=[dict(
            window_groups=[dict(id=7, tabs=[dict(id=tab, tile=dict(Foreign=dict(
                app_id=fresh.APP, surface_id=surface))) for tab, _, surface in entries])])])]))


class FreshCaptureGuards(unittest.TestCase):
    def test_existing_notes_preserved_and_current_certified_record_resolved(self):
        before = state([(1, 21, 31)])
        current = state([(1, 21, 31), (2, 99, 32)])
        binding = fresh.fresh_binding(current, fresh.tabs(before), cohort)
        self.assertEqual(binding["tab"], "2")
        self.assertEqual(binding["record"], "saved:notes:99")
        self.assertNotIn(binding["subject"]["peer_pid"], [101])

    def test_borrowed_record_ambiguous_launch_and_replaced_user_surface_refuse(self):
        before = fresh.tabs(state([(1, 21, 31)]))
        for current in (state([(1, 21, 31), (2, 21, 32)]),
                        state([(1, 21, 31), (2, 22, 32), (3, 23, 33)]),
                        state([(1, 21, 999), (2, 22, 32)])):
            with self.subTest(current=current), self.assertRaises(RuntimeError):
                fresh.fresh_binding(current, before, cohort)

    def test_worker_missing_or_reused_lifetime_is_not_an_owned_binding(self):
        before = fresh.tabs(state([(1, 21, 31)]))
        current = state([(1, 21, 31), (2, 22, 32)])
        current["chrome"]["native_window_provenance"]["windows"].pop()
        self.assertIsNone(fresh.fresh_binding(current, before, cohort))
        current = state([(1, 21, 31), (2, 22, 32)])
        current["chrome"]["native_window_provenance"]["windows"][-1]["record_instance"] = 21
        with self.assertRaisesRegex(RuntimeError, "record/lifetime"):
            fresh.fresh_binding(current, before, cohort)

    def test_missing_or_mismatched_certified_checkout_never_uses_a_continuity_alias(self):
        before = fresh.tabs(state([(1, 21, 31)]))
        current = state([(1, 21, 31), (2, 22, 32)])
        worker = current["chrome"]["native_window_provenance"]["windows"][-1]
        worker.pop("certified_checkout")
        self.assertIsNone(fresh.fresh_binding(current, before, cohort))
        worker["certified_checkout"] = dict(application=fresh.APP, room="other",
                                           record="saved:notes:22")
        with self.assertRaisesRegex(RuntimeError, "certified checkout"):
            fresh.fresh_binding(current, before, cohort)

    def test_exact_keyboard_surface_never_routes_to_other_open_notes(self):
        current = state([(1, 21, 31), (2, 22, 32)])
        current["chrome"]["seat_focus"] = dict(keyboard=dict(app_id=fresh.APP, root_surface=32),
                                              routed_keyboard_surface=32)
        require_keyboard_notes(current, 32)
        for selected in (None, 31, 999, True):
            with self.subTest(selected=selected), self.assertRaises(RuntimeError):
                require_keyboard_notes(current, selected)

    def test_all_diagnostics_restored_when_declared_launch_refuses(self):
        class Vm:
            def __init__(self):
                self.configured = []
                self.original = dict(presentation=False, gpu_stages=True,
                                     gpu_timestamps=False, gpu_dispatch_timestamps=False)
            def state(self):
                return state([(1, 21, 31)])
            def control_json(self, *args):
                if args[:2] == ("actions", "list"):
                    return {"actions": []}
                if args[3] == "configure@session-diagnostics":
                    value = json.loads(args[-1]); self.configured.append(value)
                    return {"diagnostics": value}
                return {"diagnostics": self.original}
        vm = Vm()
        with tempfile.TemporaryDirectory() as directory:
            options = argparse.Namespace(samples=3, device="test", slot="device", out=Path(directory))
            with self.assertRaisesRegex(RuntimeError, "new-tab operation"):
                fresh.capture_three(vm, cohort, options,
                                    {"/lib/libruntime_dylib.so": "a" * 64}, 1000, "b" * 64)
            self.assertEqual(vm.configured, [fresh.FLAGS, vm.original])
            restored = json.loads((options.out / "prelaunch-diagnostics-restored.json").read_text())
            self.assertEqual(restored, vm.original)

    def test_actual_startup_probe_exports_only_four_flags_and_rejects_partial_gpu(self):
        bootstrap = "import os, sys; p='/proc/self/stat'; s=open(p).read(); " + \
            "sys.argv=['probe',str(os.getpid()),s[s.rindex(')')+2:].split()[19],str(os.getuid())]; " + \
            "exec(" + repr(fresh.STARTUP) + ")"
        keys = ("TILO_PROFILE_PRESENTATION", "TILO_PROFILE_GPU_STAGES",
                "VELLO_GPU_TIMESTAMPS", "VELLO_GPU_DISPATCH_TIMESTAMPS")
        environment = dict(os.environ, PRIVATE_ENV_SENTINEL="never-export-this-field",
                           **{key: "1" for key in keys})
        answer = subprocess.run([sys.executable, "-c", bootstrap], env=environment,
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(answer.returncode, 0, answer.stderr)
        self.assertEqual(json.loads(answer.stdout)["flags"], dict.fromkeys(keys, True))
        self.assertNotIn("never-export", answer.stdout)
        environment["VELLO_GPU_DISPATCH_TIMESTAMPS"] = "0"
        refused = subprocess.run([sys.executable, "-c", bootstrap], env=environment,
                                 capture_output=True, text=True, timeout=5)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(refused.stdout, "")

    def test_exact_parsed_gpu_evidence_requires_fine_dispatch_for_all_three_frames(self):
        from physical_notes_stages_test import NativeGpuJoin
        fixture = NativeGpuJoin()
        report = fixture.report(fixture.complete())
        self.assertTrue(fresh.fine_gpu_ready(report, 123))
        self.assertFalse(fresh.fine_gpu_ready(report, 999))
        for change in ("missing", "coarse_only", "zero_dispatch"):
            changed = copy.deepcopy(report)
            frame = changed["native_gpu"]["frames"][1]
            if change == "missing":
                frame["complete"] = False
            else:
                stage = frame["kinds"]["dispatches"]["sample"]["stages"][0]
                if change == "coarse_only":
                    stage.update(stage_id=2, stage="coarse")
                else:
                    stage["dispatch_count"] = 0
            with self.subTest(change=change):
                self.assertFalse(fresh.fine_gpu_ready(changed, 123))

    def test_trace_namespace_pid_requires_pinned_kernel_namespace_observation(self):
        stages = dict(process_id=2, native_gpu=dict(frames=[dict(complete=True, kinds=dict(
            passes=dict(samples=[dict(sample=dict(pass_count=1))]),
            dispatches=dict(samples=[dict(sample=dict(stages=[dict(
                stage_id=20, stage="fine_area", dispatch_count=1, duration_ns=100)]))]))) for _ in range(3)]))
        proof = dict(pid=123, start_time_ticks="567", nspid=[123, 2], pid_namespace_inode=999)
        self.assertFalse(fresh.fine_gpu_ready(stages, 123))
        self.assertTrue(fresh.fine_gpu_ready(stages, 123, proof))
        for changes in (dict(pid=124), dict(nspid=[124, 2]), dict(nspid=[123, 3]),
                        dict(nspid=[True, 2]), dict(pid_namespace_inode=None),
                        dict(start_time_ticks="0")):
            with self.subTest(changes=changes):
                self.assertFalse(fresh.fine_gpu_ready(stages, 123, dict(proof, **changes)))

    def test_invalid_key_budget_refuses_before_any_device_observation(self):
        with self.assertRaisesRegex(RuntimeError, "exactly three"):
            fresh.capture_three(None, None, argparse.Namespace(samples=100), {}, 1000, None)


if __name__ == "__main__":
    unittest.main()
