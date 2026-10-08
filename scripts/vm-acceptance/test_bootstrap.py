"""Actual acceptance bootstrap command sequencing, with the canonical CLI process observed."""

import json
import os
import subprocess
import sys
import tempfile
from contextlib import ExitStack
from types import SimpleNamespace
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as runner
from acceptance.vm import TiloVm, VmError
from acceptance.checks.provenance import commit
from acceptance.core import Result


class BootstrapTest(unittest.TestCase):
    def run_bootstrap(self, mutation=None, components=None, expect_failure=None, cleanup=False):
        with tempfile.TemporaryDirectory() as temp:
            vm = TiloVm("tilo-vm", "owned-vm", "acceptance", Path(temp) / "commands.log")
            source = runner.REPO.resolve()
            calls = []
            layers = {}
            slot = {"name": "acceptance", "kind": "headful", "session": "active",
                    "generation": 0, "generations": [0], "components": layers,
                    "user": "owned-user", "seat": "seat-owned",
                    "owner": {"host": "owned-host", "worktree": str(source),
                              "branch": "codex/test"},
                    "outputs": [{"width": 1920, "height": 1080, "scale": 1.0},
                                {"width": 1280, "height": 800, "scale": 1.25}]}
            error = {"error": "slot 'acceptance' outputs could not be set: exit status: 1: "
                              "the compositor did not pick up the display change",
                     "hint": "rerun tilo-vm slot create acceptance --headful --outputs <same list> "
                             "to apply it again; tilo-vm logs acceptance shows what the compositor did",
                     "see": "tilo-vm guide driving"}

            settings = {"swap_error": None, "plain_error": False, "initial_success": False,
                        "final_mutation": None, "changed_head": False, "lost_shell": False, "error_status": 1, "swap_mutation": None}
            if mutation:
                mutation(slot, error, settings)

            def command(cmd, **kwargs):
                args = [arg for arg in cmd[1:] if arg != "--json"]
                calls.append((args, kwargs))
                if args == ["slot", "create", "--help"]:
                    return subprocess.CompletedProcess(cmd, 0, "--identity", "")
                if args[:2] == ["slot", "create"]:
                    self.assertEqual(args[2:], ["acceptance", "--headful", "--outputs",
                                               "1920x1080@1,1280x800@1.25", "--identity"])
                    if "shell" not in layers and not settings["initial_success"]:
                        payload = str(error) if settings["plain_error"] else json.dumps(error)
                        return subprocess.CompletedProcess(cmd, settings["error_status"], "", payload)
                    result = {
                        "slot": slot, "created": False,
                        "identity": {"created": False, "throwaway": True,
                                     "display_name": "Acceptance"},
                        "actions": [{"action": "set_outputs", "outputs": "1920x1080@1,1280x800@1.25"}]
                    }
                    if settings["final_mutation"]:
                        settings["final_mutation"](result)
                    return subprocess.CompletedProcess(cmd, 0, json.dumps(result), "")
                if args[:2] == ["slot", "show"]:
                    return subprocess.CompletedProcess(cmd, 0, json.dumps({"slot": slot}), "")
                if args[:1] == ["swap"]:
                    component = args[1]
                    if settings["swap_error"]:
                        return subprocess.CompletedProcess(cmd, 1, "", settings["swap_error"])
                    layers[component] = "verified-" + component
                    if component != "shell" and settings["lost_shell"]:
                        layers["shell"] = "different-shell"
                    slot["generation"] += 1
                    slot["generations"].append(slot["generation"])
                    result = {"slot": "acceptance", "generation": slot["generation"], "changed": True,
                              "policy": "session_restart", "actions": [], "warnings": []}
                    if settings["swap_mutation"]:
                        settings["swap_mutation"](result)
                    return subprocess.CompletedProcess(cmd, 0, json.dumps(result), "")
                if args[:2] in (["vm", "create"], ["vm", "start"], ["vm", "stop"], ["vm", "rm"]):
                    return subprocess.CompletedProcess(cmd, 0, "{}", "")
                raise AssertionError(args)

            def git(*args, **kwargs):
                if args == ("rev-parse", "--show-toplevel"):
                    return str(source)
                if args[0] == "symbolic-ref":
                    return "codex/test"
                return "changed-head" if settings["changed_head"] and "shell" in layers else "candidate-head"

            provenance = {"record": {"commit": "base-image"}}
            with ExitStack() as stack:
                stack.enter_context(mock.patch.object(runner, "git", side_effect=git))
                stack.enter_context(mock.patch("subprocess.run", side_effect=command))
                stack.enter_context(mock.patch.dict(os.environ, {"HOSTNAME": "owned-host"}))
                sleep = stack.enter_context(mock.patch.object(runner.time, "sleep"))
                if cleanup:
                    argv = ["run.py", "--disk", str(Path(temp) / "base.qcow2"),
                            "--commit", "base-image", "--out", str(Path(temp) / "report"),
                            "--swap", "shell,app:settings", "--swap-from", str(source),
                            "--swap-ref", "candidate-head"]
                    stack.enter_context(mock.patch.object(sys, "argv", argv))
                    stack.enter_context(mock.patch.object(runner, "find_tilo_vm", return_value="tilo-vm"))
                    stack.enter_context(mock.patch.object(runner, "vms_toml", return_value={}))
                    stack.enter_context(mock.patch.object(runner, "running_vms", return_value=[]))
                    stack.enter_context(mock.patch.object(runner, "mem_available_mb", return_value=100000))
                    stack.enter_context(mock.patch.object(runner, "provenance_of", return_value=provenance))
                    self.assertEqual(runner.main(), 1)
                    report = json.loads((Path(temp) / "report/report.json").read_text())
                    self.assertEqual(report["overall"], "fail")
                else:
                    def invoke():
                        runner.bootstrap_session(vm, ["1920x1080@1", "1280x800@1.25"],
                                                 {"components": components if components is not None
                                                  else ["shell", "app:settings"],
                                                  "source": source, "ref": "candidate-head"}, provenance)
                    if expect_failure:
                        with self.assertRaisesRegex(VmError, expect_failure):
                            invoke()
                    else:
                        invoke()
                sleep.assert_not_called()
            return calls, provenance

    def test_candidate_shell_bootstraps_the_owned_base_display_timeout_once(self):
        calls, provenance = self.run_bootstrap()
        swaps = [args[1] for args, _ in calls if args[:1] == ["swap"]]
        creates = [args for args, _ in calls if args[:2] == ["slot", "create"]
                   and "--help" not in args]
        self.assertEqual(swaps, ["shell", "app:settings"])
        self.assertEqual(len(creates), 2)
        self.assertLess(next(i for i, (args, _) in enumerate(calls) if args[:2] == ["swap", "shell"]),
                        [i for i, (args, _) in enumerate(calls) if args in creates][1])
        res = Result("P01", "provenance", "test")
        commit(SimpleNamespace(facts={"provenance": provenance}), res)
        self.assertEqual([entry["component"] for entry in provenance["swap"]["components"]],
                         ["shell", "app:settings"])
        self.assertEqual(provenance["bootstrap"]["base_slot"]["generation"], 0)
        self.assertEqual(provenance["bootstrap"]["candidate_create"]["slot"]["generation"], 1)

    def test_other_errors_never_apply_shell(self):
        variants = ["permission denied", "could not get a throwaway test identity",
                    "kernel failed", "the compositor rejected the layout",
                    "the compositor did not publish 2 outputs"]
        for text in variants:
            with self.subTest(text=text):
                calls, _ = self.run_bootstrap(
                    lambda slot, error, settings: error.update(error=text),
                    expect_failure=text)
                self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))
        calls, _ = self.run_bootstrap(
            lambda slot, error, settings: settings.update(plain_error=True),
            expect_failure="outputs could not be set")
        self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))

    def test_exact_structured_error_and_status_are_required(self):
        for key in ("hint", "see"):
            with self.subTest(key=key):
                calls, _ = self.run_bootstrap(
                    lambda slot, error, settings: error.update({key: "different"}),
                    expect_failure="outputs could not be set")
                self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))
        calls, _ = self.run_bootstrap(
            lambda slot, error, settings: settings.update(error_status=2),
            expect_failure="exited 2")
        self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))

    def test_canonical_swap_result_must_observe_owned_candidate_generation(self):
        for key, value in (("slot", "other"), ("generation", 0), ("generation", None)):
            with self.subTest(key=key, value=value):
                calls, _ = self.run_bootstrap(
                    lambda slot, error, settings: settings.update(
                        swap_mutation=lambda result: result.update({key: value})),
                    expect_failure="owned candidate generation")
                self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]], ["shell"])
                self.assertEqual(sum(args[:2] == ["slot", "create"] and "--help" not in args
                                     for args, _ in calls), 1)

    def test_changed_early_shell_layer_is_not_relabelled_as_the_candidate(self):
        calls, provenance = self.run_bootstrap(
            lambda slot, error, settings: settings.update(lost_shell=True),
            expect_failure="Shell layer changed")
        self.assertEqual(provenance["swap"]["components"][0]["layer"], "verified-shell")
        self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]],
                         ["shell", "app:settings"])

    def test_shell_must_be_requested_exactly_once(self):
        for components in ([], ["app:settings"], ["shell", "shell"]):
            with self.subTest(components=components):
                calls, _ = self.run_bootstrap(components=components, expect_failure="outputs could not be set")
                self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))

    def test_missing_or_different_slot_observation_refuses_recovery(self):
        changes = [("name", "other"), ("kind", "headless"), ("session", "inactive"),
                   ("owner", {}), ("generation", 2), ("components", {"shell": "old"}),
                   ("generations", []), ("outputs", []), ("user", None), ("seat", None)]
        for key, value in changes:
            with self.subTest(key=key):
                calls, _ = self.run_bootstrap(
                    lambda slot, error, settings: slot.update({key: value}),
                    expect_failure="bootstrap")
                self.assertFalse(any(args[:1] == ["swap"] for args, _ in calls))

    def test_failed_shell_swap_stops_before_other_components(self):
        calls, provenance = self.run_bootstrap(
            lambda slot, error, settings: settings.update(swap_error="builder refused source"),
            expect_failure="Shell was not applied")
        self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]], ["shell"])
        self.assertIn("error", provenance["swap"]["components"][0])
        self.assertNotIn("candidate_create", provenance["bootstrap"])

    def test_partial_bootstrap_failure_still_runs_actual_main_cleanup(self):
        calls, _ = self.run_bootstrap(
            lambda slot, error, settings: settings.update(swap_error="builder refused source"), cleanup=True)
        self.assertTrue(any(args[:2] == ["vm", "stop"] for args, _ in calls))
        self.assertTrue(any(args[:2] == ["vm", "rm"] for args, _ in calls))

    def test_final_outputs_identity_and_generation_are_observed(self):
        changes = [lambda result: result.update(identity={}),
                   lambda result: result.update(identity="unobserved"),
                   lambda result: result.update(actions=None),
                   lambda result: result.update(actions=[None]),
                   lambda result: result["slot"].update(components=None),
                   lambda result: result.update(actions=[]),
                   lambda result: result["slot"].update(generation=0),
                   lambda result: result["slot"].update(owner={}),
                   lambda result: result["slot"].update(outputs=[]),
                   lambda result: result["slot"].update(user="other-user")]
        for change in changes:
            with self.subTest(change=change):
                calls, _ = self.run_bootstrap(
                    lambda slot, error, settings: settings.update(final_mutation=change),
                    expect_failure="bootstrap")
                self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]], ["shell"])

    def test_changed_source_after_shell_does_not_mix_app_layers(self):
        calls, _ = self.run_bootstrap(
            lambda slot, error, settings: settings.update(changed_head=True),
            expect_failure="source changed")
        self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]], ["shell"])

    def test_normal_creation_keeps_requested_order_without_bootstrap(self):
        calls, provenance = self.run_bootstrap(
            lambda slot, error, settings: settings.update(initial_success=True),
            components=["app:settings", "shell"])
        self.assertNotIn("bootstrap", provenance)
        self.assertEqual([args[1] for args, _ in calls if args[:1] == ["swap"]], ["app:settings", "shell"])



if __name__ == "__main__":
    unittest.main()
