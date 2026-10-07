#!/usr/bin/env python3
"""Validate private coding-task calibration artifacts without running agents."""

import argparse
import json
from pathlib import Path

from coding_eval import digest, require_hash, require_text, test_outcome, validate_manifest, write_new


def artifact(root, record):
    if not isinstance(record, dict):
        raise ValueError("artifact reference must be an object")
    require_text(record.get("path"), "artifact path")
    require_hash(record.get("sha256"), 64, "artifact sha256")
    relative = Path(record["path"])
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("artifacts must be pack-relative paths")
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()):
        raise ValueError("artifact escapes private pack directory")
    data = path.read_bytes()
    if digest(data) != record["sha256"]:
        raise ValueError("task artifact digest mismatch")
    return path, data


def calibration(root, reference, task, contract, expected_pass):
    path, data = artifact(root, reference)
    proof = json.loads(data)
    if not isinstance(proof, dict) or type(proof.get("schema_version")) is not int \
            or proof["schema_version"] != 1:
        raise ValueError("unsupported calibration proof")
    require_hash(proof.get("candidate_revision"), 40, "calibration candidate revision")
    if proof["candidate_revision"] != reference["candidate_revision"]:
        raise ValueError("calibration revision mismatch")
    require_text(proof.get("grader_identity"), "calibration grader identity")
    checks = proof.get("checks")
    if not isinstance(checks, list) or any(not isinstance(c, dict) for c in checks):
        raise ValueError("calibration checks must be an object array")
    observed = {c["id"]: c for c in checks}
    expected = {c["id"] for c in contract["checks"]}
    if len(observed) != len(checks) or set(observed) != expected:
        raise ValueError("calibration checks must match acceptance contract exactly")
    outcomes = [test_outcome(c, observed[c["id"]], path.parent) for c in contract["checks"]]
    passed = all(c["passed"] for c in outcomes)
    if passed != expected_pass:
        raise ValueError(f"{task['id']} calibration outcome does not match required result")
    if not expected_pass and not any(c["failed_expected_tests"] for c in outcomes):
        raise ValueError("negative calibration requires an observed expected-test failure")
    return {"candidate_revision": proof["candidate_revision"], "passed": passed,
            "proof_sha256": reference["sha256"], "checks": outcomes}


def validate(pack, root):
    if not isinstance(pack, dict) or type(pack.get("schema_version")) is not int \
            or pack["schema_version"] != 1:
        raise ValueError("unsupported task pack schema")
    if pack.get("visibility") != "evaluator_private":
        raise ValueError("held-out task pack requires evaluator_private visibility")
    require_text(pack.get("pack_id"), "pack_id")
    tasks = pack.get("tasks")
    if not isinstance(tasks, list) or not tasks:
        raise ValueError("task pack must contain calibrated tasks")
    seen = set()
    validated = []
    for task in tasks:
        if not isinstance(task, dict):
            raise ValueError("task must be an object")
        require_text(task.get("id"), "task id")
        require_text(task.get("category"), "task category")
        if task["id"] in seen:
            raise ValueError("duplicate task id")
        seen.add(task["id"])
        require_hash(task.get("base_revision"), 40, "base revision")
        _, prompt = artifact(root, task.get("prompt"))
        if not prompt.decode("utf-8").strip():
            raise ValueError("task prompt is empty")
        _, raw = artifact(root, task.get("acceptance_manifest"))
        acceptance = json.loads(raw)
        validate_manifest(acceptance)
        if acceptance["purpose"] != "coding_tasks" or acceptance["repository_revision"] != task["base_revision"]:
            raise ValueError("acceptance manifest must target coding tasks at the task base")
        if len(acceptance["tasks"]) != 1 or acceptance["tasks"][0]["id"] != task["id"]:
            raise ValueError("acceptance contract must identify exactly this task")
        contract = acceptance["tasks"][0]
        if contract["category"] != task["category"] or contract["instruction"].strip() != prompt.decode("utf-8").strip():
            raise ValueError("task prompt and category must match acceptance contract")
        proofs = task.get("calibration")
        if not isinstance(proofs, dict) or set(proofs) != {"baseline", "reference", "mutant"}:
            raise ValueError("baseline, reference and mutant calibrations are required")
        if any(not isinstance(p, dict) for p in proofs.values()):
            raise ValueError("calibration references must be objects")
        for proof in proofs.values():
            require_hash(proof.get("candidate_revision"), 40, "calibration revision")
        if proofs["baseline"]["candidate_revision"] != task["base_revision"]:
            raise ValueError("baseline calibration must observe the unrepaired base")
        revisions = {p["candidate_revision"] for p in proofs.values()}
        if len(revisions) != 3:
            raise ValueError("base, correct repair and wrong repair must be distinct revisions")
        outcomes = {kind: calibration(root, reference, task, contract, kind == "reference")
                    for kind, reference in proofs.items()}
        validated.append({"id": task["id"], "category": task["category"],
                          "base_revision": task["base_revision"],
                          "prompt_sha256": task["prompt"]["sha256"],
                          "acceptance_sha256": task["acceptance_manifest"]["sha256"],
                          "calibration": outcomes})
    return {"schema_version": 1, "evidence": "observed_task_calibration",
            "pack_id": pack["pack_id"], "tasks": validated,
            "limitations": ["No model was evaluated by this validator.",
                            "Digests do not authenticate grader identity or establish isolation.",
                            "Private visibility is a declaration; access and contamination require independent review.",
                            "Commit identities are recorded, not verified against a repository object database.",
                            "Passing calibration proves these checks detect these repairs, not complete task quality."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pack", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        raw = args.pack.read_bytes()
        report = validate(json.loads(raw), args.pack.parent)
        report["pack_sha256"] = digest(raw)
        write_new(args.out, report)
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(2, f"task_pack: {error}\n")


if __name__ == "__main__":
    main()
