#!/usr/bin/env python3
"""Score captured acceptance evidence; never run models, tools or agent plans."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import xml.etree.ElementTree as ET


def digest(data):
    return hashlib.sha256(data).hexdigest()


def require_hash(value, length, name):
    if not isinstance(value, str) or not re.fullmatch(f"[0-9a-f]{{{length}}}", value):
        raise ValueError(f"{name} must be an immutable {length}-digit hex identity")


def require_text(value, name):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} is required")


def manifest(repo):
    revision = subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True
    ).strip()
    source = "apps/agent/tests/discovered_operations.rs"
    names = [
        "discovered_operations::a_discovered_operation_runs_only_when_granted_and_explicitly_confirmed",
        "discovered_operations::a_discovered_operation_is_performed_once_and_its_result_carried_into_the_next_app",
    ]
    return {
        "schema_version": 1,
        "purpose": "baseline_readiness",
        "repository_revision": revision,
        "tasks": [{
            "id": "agent-semantic-discovery",
            "instruction": "Verify existing real broker discovery, confirmation and capability tests.",
            "category": "platform_readiness",
            "checks": [{
                "id": "broker-discovery",
                "argv": ["cargo", "test", "--locked", "-p", "agent", "--test", "it",
                         "discovered_operations::"],
                "formats": ["nextest_junit", "nextest_text", "libtest_text"],
                "expected_binary": "agent::it",
                "test_source": source,
                "test_source_sha256": digest((repo / source).read_bytes()),
                "expected_tests": names,
            }],
        }],
    }


def validate_manifest(pack):
    if not isinstance(pack, dict):
        raise ValueError("manifest must be an object")
    if type(pack.get("schema_version")) is not int or pack["schema_version"] != 1:
        raise ValueError("unsupported manifest schema")
    if pack.get("purpose") not in {"baseline_readiness", "coding_tasks"}:
        raise ValueError("manifest purpose must be baseline_readiness or coding_tasks")
    require_hash(pack.get("repository_revision"), 40, "repository_revision")
    tasks = pack.get("tasks")
    if not isinstance(tasks, list) or not tasks:
        raise ValueError("manifest must contain tasks")
    seen_tasks = set()
    for task in tasks:
        if not isinstance(task, dict):
            raise ValueError("task must be an object")
        require_text(task.get("id"), "task id")
        if task["id"] in seen_tasks:
            raise ValueError("duplicate task id")
        seen_tasks.add(task["id"])
        require_text(task.get("instruction"), "task instruction")
        require_text(task.get("category"), "task category")
        if not isinstance(task.get("checks"), list) or not task["checks"]:
            raise ValueError("task requires acceptance checks")
        seen_checks = set()
        for check in task["checks"]:
            if not isinstance(check, dict):
                raise ValueError("check must be an object")
            require_text(check.get("id"), "check id")
            if check["id"] in seen_checks:
                raise ValueError("duplicate check id")
            seen_checks.add(check["id"])
            require_text(check.get("test_source"), "test_source")
            formats = check.get("formats", ["libtest_text"])
            if not isinstance(formats, list) or not formats or any(
                not isinstance(format_name, str)
                or format_name not in {"libtest_text", "nextest_junit", "nextest_text"}
                for format_name in formats
            ):
                raise ValueError("unknown acceptance format")
            if any(f.startswith("nextest") for f in formats):
                require_text(check.get("expected_binary"), "expected_binary")
            require_hash(check.get("test_source_sha256"), 64, "test_source_sha256")
            argv = check.get("argv")
            if not isinstance(argv, list) or not argv or any(
                not isinstance(arg, str) or not arg for arg in argv
            ):
                raise ValueError("check argv must be a nonempty string array")
            names = check.get("expected_tests")
            if not isinstance(names, list) or not names or any(
                not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_:]+", name)
                for name in names
            ) or len(names) != len(set(names)):
                raise ValueError("expected_tests must contain unique Rust test names")


def validate_run(run, pack_hash, purpose):
    if not isinstance(run, dict):
        raise ValueError("run must be an object")
    if type(run.get("schema_version")) is not int or run["schema_version"] != 1 \
            or run.get("manifest_sha256") != pack_hash:
        raise ValueError("run schema or manifest digest mismatch")
    expected_mode = "baseline_readiness" if purpose == "baseline_readiness" else "model_evaluation"
    if run.get("mode") != expected_mode:
        raise ValueError("readiness evidence cannot be labeled as a model evaluation")
    for name in ("candidate_revision", "harness_revision"):
        require_hash(run.get(name), 40, name)
    for name in ("environment_sha256", "action_schema_sha256", "trace_sha256"):
        require_hash(run.get(name), 64, name)
    for name in ("harness", "permissions", "network_policy", "grader_identity"):
        require_text(run.get(name), name)
    if expected_mode == "model_evaluation":
        model = run.get("model", {})
        if not isinstance(model, dict):
            raise ValueError("model provenance must be an object")
        require_text(model.get("id"), "model id")
        require_hash(model.get("revision"), 40, "model revision")
        for name in ("weights_sha256", "tokenizer_sha256", "template_sha256",
                     "runtime_sha256", "quantization_sha256", "generation_config_sha256"):
            require_hash(model.get(name), 64, name)
        require_text(model.get("precision"), "model precision")
        for name in ("context_tokens", "token_limit", "attempts"):
            value = run.get(name)
            if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
                raise ValueError(f"{name} must be a positive integer")


def libtest_outcome(output, check):
    summaries = re.findall(
        r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", output
    )
    if not summaries:
        raise ValueError("no observable Rust test completion summary")
    completed = []
    for name in check["expected_tests"]:
        matches = re.findall(r"^test " + re.escape(name) + r" \.\.\. (ok|FAILED|ignored)$",
                             output, flags=re.MULTILINE)
        if len(matches) != 1:
            raise ValueError(f"missing or duplicate completion for {name}")
        completed.append(matches[0])
    return (all(result == "ok" for result in completed)
            and all(status == "ok" and int(failed) == 0 and int(ignored) == 0
                    for status, count, failed, ignored in summaries)
            and sum(int(count) for status, count, failed, ignored in summaries) >= len(completed))


def junit_outcome(output, check):
    try:
        root = ET.fromstring(output)
    except ET.ParseError as error:
        raise ValueError(f"invalid nextest JUnit report: {error}") from error
    if root.tag != "testsuites":
        raise ValueError("nextest report requires a testsuites root")
    cases = list(root.iter("testcase"))
    if not cases:
        raise ValueError("nextest report has no observed test cases")
    names = [(c.get("classname"), c.get("name")) for c in cases]
    for name in check["expected_tests"]:
        if names.count((check["expected_binary"], name)) != 1:
            raise ValueError(f"missing or duplicate completion for {name}")
    bad_tags = {"failure", "error", "skipped", "rerunFailure", "rerunError",
                "flakyFailure", "flakyError"}
    if any(node.tag in bad_tags for case in cases for node in case.iter()):
        return False
    for suite in [root, *root.iter("testsuite")]:
        for attribute in ("failures", "errors", "disabled", "skipped"):
            value = suite.get(attribute, "0")
            if not value.isdigit():
                raise ValueError(f"invalid nextest suite {attribute}")
            if int(value):
                return False
    return True


def nextest_text_outcome(output, check):
    # Explicit fallback for remote Cargo routing that returns console output only.
    output = re.sub(r"\x1b\[[0-9;]*m", "", output)
    summaries = re.findall(r"Summary\s+\[[^\]\n]+\]\s+(\d+) tests run:\s+(\d+) passed([^\n]*)",
                           output)
    if len(summaries) != 1:
        raise ValueError("missing or ambiguous nextest completion summary")
    total, passed, tail = summaries[0]
    completed = []
    for name in check["expected_tests"]:
        pattern = (r"^\s*(PASS|FAIL|SKIP|TIMEOUT|LEAK|FLAKY)\s+\[[^\]\n]+\]\s+"
                   + re.escape(check["expected_binary"]) + r"\s+" + re.escape(name) + r"\s*$")
        matches = re.findall(pattern, output, flags=re.MULTILINE)
        if len(matches) != 1:
            raise ValueError(f"missing or duplicate completion for {name}")
        completed.append(matches[0])
    # Filtered-out tests are normal; skipped expected tests never count as PASS.
    unhealthy = re.search(r"(?:[1-9]\d* (?:failed|flaky|leaked|timed out)|canceled|not run)", tail)
    return (all(status == "PASS" for status in completed) and not unhealthy
            and int(total) == int(passed) and int(total) >= len(completed))


def test_outcome(check, observed, artifact_root):
    if observed.get("argv") != check["argv"]:
        raise ValueError("acceptance command mismatch")
    if observed.get("test_source_sha256") != check["test_source_sha256"]:
        raise ValueError("acceptance test source mismatch")
    if type(observed.get("returncode")) is not int:
        raise ValueError("missing process return code")
    log_name = observed.get("log")
    require_text(log_name, "log")
    relative = Path(log_name)
    if relative.is_absolute() or ".." in relative.parts or not relative.parts:
        raise ValueError("log must be an artifact-relative path")
    root = artifact_root.resolve()
    path = (root / relative).resolve()
    if not path.is_relative_to(root):
        raise ValueError("log escapes artifact directory")
    raw = path.read_bytes()
    if digest(raw) != observed.get("log_sha256"):
        raise ValueError("acceptance log digest mismatch")
    output = raw.decode("utf-8", errors="strict")
    format_name = observed.get("format", "libtest_text")
    parsers = {"libtest_text": libtest_outcome, "nextest_junit": junit_outcome,
               "nextest_text": nextest_text_outcome}
    if format_name not in check.get("formats", ["libtest_text"]):
        raise ValueError("observed format differs from acceptance contract")
    passed = parsers[format_name](output, check) and observed["returncode"] == 0
    return {"id": check["id"], "passed": passed, "format": format_name,
            "observed_tests": len(check["expected_tests"]),
            "log_sha256": observed["log_sha256"]}


def score(pack, run, pack_hash, artifact_root):
    validate_manifest(pack)
    validate_run(run, pack_hash, pack["purpose"])
    if not isinstance(run.get("tasks"), list):
        raise ValueError("run task evidence is missing")
    if any(not isinstance(task, dict) for task in run["tasks"]):
        raise ValueError("task evidence must be an object")
    evidence = {task["id"]: task for task in run["tasks"]}
    if len(evidence) != len(run["tasks"]) or set(evidence) != {t["id"] for t in pack["tasks"]}:
        raise ValueError("run task identities must match manifest exactly")
    results = []
    for task in pack["tasks"]:
        observations = evidence[task["id"]].get("checks", [])
        if not isinstance(observations, list) or any(not isinstance(c, dict) for c in observations):
            raise ValueError("check evidence must be an object array")
        checks = {item["id"]: item for item in observations}
        if len(checks) != len(observations) or set(checks) != {c["id"] for c in task["checks"]}:
            raise ValueError("run checks must match manifest exactly")
        outcomes = [test_outcome(check, checks[check["id"]], artifact_root)
                    for check in task["checks"]]
        results.append({"id": task["id"], "category": task["category"],
                        "passed": all(c["passed"] for c in outcomes), "checks": outcomes})
    return {"schema_version": 1, "evidence": "captured_acceptance_tests",
            "purpose": pack["purpose"], "manifest_sha256": pack_hash,
            "run": run, "tasks": results,
            "passed_tasks": sum(t["passed"] for t in results), "total_tasks": len(results),
            "limitations": ["Digests check integrity, not grader authenticity or execution isolation.",
                            "Readiness checks are not model coding scores.",
                            "No cross-manifest leaderboard, quality forecast or hardware estimate."]}


def write_new(path, value):
    with path.open("x") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("manifest", help="capture a real Tilo broker readiness contract")
    create.add_argument("--repo", type=Path, default=Path.cwd())
    create.add_argument("--out", type=Path, required=True)
    evaluate = commands.add_parser("score", help="score captured independent grader evidence")
    evaluate.add_argument("--manifest", type=Path, required=True)
    evaluate.add_argument("--run", type=Path, required=True)
    evaluate.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "manifest":
            value = manifest(args.repo)
            validate_manifest(value)
        else:
            raw = args.manifest.read_bytes()
            value = score(json.loads(raw), json.loads(args.run.read_bytes()), digest(raw),
                          args.run.parent)
        write_new(args.out, value)
        if args.command == "score" and value["passed_tasks"] != value["total_tasks"]:
            raise SystemExit(1)
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(2, f"coding_eval: {error}\n")


if __name__ == "__main__":
    main()
