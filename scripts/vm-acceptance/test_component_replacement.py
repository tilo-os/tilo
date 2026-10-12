"""The replacement auditor must reject incomplete or contradictory captures."""
import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from acceptance.replacement import FILES, ReplacementError, audit, audit_directory

VM, SLOT, SOURCE = "qa", "replace-012345abcdef", "ab" * 20
BASE, CANDIDATE, RUNTIME, FAILED = (ch * 64 for ch in "1234")


def receipt(command, result, **fields):
    return {"tool": "tilo-vm", "vm": VM, "command": command.split(),
            "status": "success", "result": result, **fields}


def captures():
    user = "tds-" + SLOT
    info = {"name": SLOT, "kind": "headless", "user": user,
            "generation": 0, "components": {}}
    provenance = {"source_commit": SOURCE, "dirty_tree_hash": None,
                  "target": "x86_64-unknown-linux-gnu", "toolchain": "1.90.0",
                  "cargo_lock_hash": "cd" * 32}
    runtime = {**provenance, "component": "apps+runtime", "cargo_profile": "thin-app"}

    def show(layer, generation):
        return receipt("--json slot show " + SLOT, {
            "slot": {**info, "generation": generation,
                     "components": {"shell": layer, "apps+runtime": RUNTIME}},
            "files": [{"component": "apps+runtime", "layer": RUNTIME,
                       "path": "/usr/lib/tilo-runtime/sdk/lib/libruntime_dylib.so"}],
            "provenance": {"shell": {**provenance, "component": "shell", "cargo_profile": "dev"},
                           "apps+runtime": runtime}})

    fixture = {"vm": VM, "slot": SLOT, "user": user, "uid": 1007,
               "home": "/var/home/" + user,
               "identity_root": "/var/home/" + user + "/.local/share/tilo/test-identity",
               "sentinel_sha256": "ef" * 32}
    failure = receipt("--json swap shell --from layer:" + FAILED, None,
                      slot=SLOT, component="shell", layer=FAILED)
    failure.update(status="failure", error={"error": "new desktop failed; restored generation 3"})
    del failure["result"]
    active = show(CANDIDATE, 3)
    return {
        "create-run": receipt("--json slot create " + SLOT + " --headless",
                              {"created": True, "slot": info, "identity": None}),
        "before-run": show(BASE, 2),
        "activate-run": receipt("--json swap shell --from layer:" + CANDIDATE,
                                {"slot": SLOT, "generation": 3, "changed": True,
                                 "policy": "session_restart"},
                                slot=SLOT, component="shell", layer=CANDIDATE),
        "active-run": active, "fail-run": failure,
        "recovered-run": copy.deepcopy(active),
        "fixture-before": fixture, "fixture-after": copy.deepcopy(fixture),
        "remove-run": receipt("--json slot rm " + SLOT, {"removed": True}),
        "teardown": {"vm": VM, "slot": SLOT, "user": user, "uid": 1007,
                     "home": fixture["home"], **{field: False for field in (
                         "slot_present", "account_present", "home_present", "session_active",
                         "user_manager_active", "session_dropins_present", "user_dropins_present")}},
    }


class ReplacementTest(unittest.TestCase):
    def test_matching_graph_with_different_profiles_is_accepted_as_captured_evidence(self):
        result = audit(captures(), VM, SLOT, SOURCE)
        self.assertEqual(result["status"], "captured_evidence_consistent")
        self.assertEqual(result["live_execution"], "not_performed_by_auditor")

    def test_mismatched_or_missing_provenance_fails(self):
        for field in ("source_commit", "dirty_tree_hash", "target", "toolchain", "cargo_lock_hash"):
            with self.subTest(field=field):
                data = captures()
                data["active-run"]["result"]["provenance"]["shell"][field] = "de" * 32
                with self.assertRaises(ReplacementError):
                    audit(data, VM, SLOT, SOURCE)
        data = captures()
        del data["active-run"]["result"]["provenance"]
        with self.assertRaisesRegex(ReplacementError, "provenance unavailable"):
            audit(data, VM, SLOT, SOURCE)

    def test_noop_or_wrong_activation_receipt_fails(self):
        for field, value in (("changed", False), ("generation", 2), ("policy", "no_restart")):
            data = captures()
            data["activate-run"]["result"][field] = value
            with self.assertRaisesRegex(ReplacementError, "successful session activation"):
                audit(data, VM, SLOT, SOURCE)
        data = captures()
        data["activate-run"]["layer"] = FAILED
        with self.assertRaisesRegex(ReplacementError, "target/layer mismatch"):
            audit(data, VM, SLOT, SOURCE)

    def test_preflight_failure_failed_recovery_and_wrong_generation_do_not_prove_rollback(self):
        for error in ("untrusted publisher", "restored generation 30", "recovery also failed"):
            data = captures()
            data["fail-run"]["error"]["error"] = error
            with self.assertRaisesRegex(ReplacementError, "recovery unavailable"):
                audit(data, VM, SLOT, SOURCE)
        data = captures()
        data["recovered-run"]["result"]["slot"]["components"]["shell"] = FAILED
        with self.assertRaisesRegex(ReplacementError, "components changed"):
            audit(data, VM, SLOT, SOURCE)

    def test_existing_slot_global_identity_and_lost_state_fail(self):
        data = captures()
        data["create-run"]["result"]["created"] = False
        with self.assertRaisesRegex(ReplacementError, "fresh ownership"):
            audit(data, VM, SLOT, SOURCE)
        data = captures()
        for name in ("fixture-before", "fixture-after"):
            data[name]["identity_root"] = "/var/lib/tilo"
        with self.assertRaisesRegex(ReplacementError, "isolated home"):
            audit(data, VM, SLOT, SOURCE)
        data = captures()
        data["fixture-after"]["sentinel_sha256"] = "00" * 32
        with self.assertRaisesRegex(ReplacementError, "fixture state changed"):
            audit(data, VM, SLOT, SOURCE)

    def test_missing_teardown_observation_or_remaining_account_fails(self):
        for field in ("slot_present", "account_present", "home_present", "session_active",
                      "user_manager_active", "session_dropins_present", "user_dropins_present"):
            for value in (None, True):
                data = captures()
                data["teardown"][field] = value
                with self.assertRaisesRegex(ReplacementError, field):
                    audit(data, VM, SLOT, SOURCE)
        data = captures()
        data["remove-run"]["command"] = ["slot", "rm", "daily"]
        with self.assertRaisesRegex(ReplacementError, "another command"):
            audit(data, VM, SLOT, SOURCE)

    def test_directory_hashes_exact_inputs_and_cli_fails_on_missing_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            for name, data in captures().items():
                (directory / (name + ".json")).write_text(json.dumps(data))
            result = audit_directory(directory, VM, SLOT, SOURCE)
            self.assertEqual(len(result["input_sha256"]), len(FILES))
            (directory / "remove-run.json").unlink()
            cli = Path(__file__).parent / "component-replacement.py"
            output = subprocess.run([sys.executable, str(cli), str(directory), "--vm", VM,
                                     "--slot", SLOT, "--source", SOURCE],
                                    capture_output=True, text=True)
            self.assertEqual(output.returncode, 1)
            self.assertEqual(json.loads(output.stdout)["status"], "failed")


if __name__ == "__main__":
    unittest.main()
