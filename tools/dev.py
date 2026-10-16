#!/usr/bin/env python3
"""Run focused development checks without building an OS or app fleet.

This is the shared local/CI entry point for cheap contracts. It writes timing and
failure receipts; passing contracts is not VM, hardware or release qualification.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
SUITES = {
    "installer": [
        ("target-modes", "os/tests/installer_target_modes.py"),
        ("target-artifacts", "os/tests/installer_target_artifacts.py"),
        ("installer-overlay", "os/tests/test_installer_overlay.py"),
    ],
    "app-packaging": [
        ("app-selection", "tools/tests/test_app_release.py"),
        ("collection-members", "tools/tests/test_assemble_app_collection.py"),
        ("artifact-handoff", "tools/tests/test_app_artifact_handoff.py"),
    ],
}
REQUIREMENTS = {"installer": ("bash",),
                "app-packaging": ("bash", "jq", "tar", "sha256sum", "python3")}


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def plan(suite: str, root: Path = ROOT, *, steps: list[str] | None = None) -> dict:
    available = {name for name, _ in SUITES[suite]}
    requested = available if steps is None else set(steps)
    if not requested or not requested <= available:
        raise ValueError(f"invalid steps for {suite}: {', '.join(sorted(requested - available)) or '(empty selection)'}; "
                         f"choose from {', '.join(sorted(available))}")
    selected = [(name, path) for name, path in SUITES[suite] if name in requested]
    selection = {"schema": 1, "suite": suite,
            "scope": "contract-tests" if requested == available else "contract-test-subset", "platform": "linux",
            "omitted_steps": [name for name, _ in SUITES[suite] if name not in requested],
            "qualification": {"vm": False, "hardware": False, "release": False},
            "requirements": list(REQUIREMENTS[suite]),
            "steps": [{"name": name, "argv": [sys.executable, str(root / path)],
                       "test_source": path,
                       "test_source_sha256": hashlib.sha256((root / path).read_bytes()).hexdigest()}
                      for name, path in selected]}
    # Materialization checks exercise the production root-ownership boundary.
    # A private user namespace gives fixtures UID 0 without host root, sudo,
    # modifying the verifier or pretending a failed ownership check passed.
    if suite == "installer" and "target-artifacts" in requested and getattr(os, "geteuid", lambda: -1)() != 0:
        selection["requirements"].append("unshare")
        for step in selection["steps"]:
            if step["name"] == "target-artifacts":
                step["argv"] = ["unshare", "--user", "--map-root-user", *step["argv"]]
                step["isolation"] = "private-user-namespace"
    return selection


def save(path: Path, receipt: dict) -> None:
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def task_worktree(name: str, base: str, root: Path = ROOT) -> dict:
    """Create a committed task snapshot beside the shared Git directory."""
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", name):
        raise ValueError("task name must contain 1–63 lowercase letters, digits or hyphens")
    def git(*args: str) -> str:
        return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
    common = Path(git("rev-parse", "--path-format=absolute", "--git-common-dir"))
    commit = git("rev-parse", "--verify", "--end-of-options", f"{base}^{{commit}}")
    destination = common.parent / ".worktrees" / name
    if destination.exists() or destination.is_symlink():
        raise FileExistsError(f"task worktree already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    branch = f"codex/{name}"
    subprocess.run(["git", "-C", str(root), "worktree", "add", "-b", branch,
                    str(destination), commit], check=True, stdout=subprocess.DEVNULL)
    return {"path": str(destination), "branch": branch, "commit": commit,
            "shared_git_dir": str(common), "target_dir": str(destination / "target"),
            "source": "committed snapshot; existing edits remain in their original worktree"}


def run_plan(selection: dict, out: Path, *, root: Path = ROOT, keep_going: bool = False) -> int:
    # Refuse to overwrite or adopt a prior request's evidence.
    out.mkdir(parents=True, exist_ok=False)
    receipt_path = out / "run.json"
    receipt = {**selection, "request_id": uuid.uuid4().hex, "started_at": now(),
               "status": "running", "steps": [], "unrun_steps": [s["name"] for s in selection["steps"]]}
    started = time.monotonic()
    save(receipt_path, receipt)
    exit_code = 0
    try:
        if selection.get("platform") == "linux" and not sys.platform.startswith("linux"):
            raise RuntimeError("these contracts require Linux; run inside WSL or a Linux builder (--plan works on any host)")
        missing = [tool for tool in selection["requirements"] if not shutil.which(tool)]
        if missing:
            raise RuntimeError("missing required tools: " + ", ".join(missing))
        for step in selection["steps"]:
            row = {**step, "status": "running", "started_at": now()}
            receipt["steps"].append(row)
            receipt["unrun_steps"].remove(step["name"])
            save(receipt_path, receipt)
            print(f"[{step['name']}]", flush=True)
            before = time.monotonic()
            source = step.get("test_source")
            if source and hashlib.sha256((root / source).read_bytes()).hexdigest() != step["test_source_sha256"]:
                raise RuntimeError(f"test source changed after planning: {source}")
            # Keep live output on the terminal. No ambient env/credentials or raw
            # signing/VM console output is collected into a portable receipt.
            child = subprocess.Popen(step["argv"], cwd=root)
            try:
                code = child.wait()
            except KeyboardInterrupt:
                # These suites have no background services or privileged guests.
                # Stop only the child we own; never kill a host-wide process name.
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                row["status"] = "interrupted"
                raise
            finally:
                row["elapsed_seconds"] = round(time.monotonic() - before, 3)
                row["finished_at"] = now()
            if source and hashlib.sha256((root / source).read_bytes()).hexdigest() != step["test_source_sha256"]:
                raise RuntimeError(f"test source changed during execution: {source}")
            row.update(status="success" if code == 0 else "failure", exit_code=code)
            save(receipt_path, receipt)
            if code:
                exit_code = 1
                if not keep_going:
                    break
        receipt["status"] = "success" if exit_code == 0 else "failure"
    except KeyboardInterrupt:
        receipt["status"] = "interrupted"
        if receipt["steps"] and receipt["steps"][-1]["status"] == "running":
            receipt["steps"][-1]["status"] = "interrupted"
        exit_code = 130
    except (OSError, RuntimeError) as error:
        receipt.update(status="failure", error=str(error))
        if receipt["steps"] and receipt["steps"][-1]["status"] == "running":
            receipt["steps"][-1]["status"] = "failure"
        print(f"dev check: {error}", file=sys.stderr)
        exit_code = 2
    finally:
        receipt.update(finished_at=now(), elapsed_seconds=round(time.monotonic() - started, 3))
        save(receipt_path, receipt)
        print(f"{receipt['status']}: {receipt_path}", flush=True)
    return exit_code


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check", help="run focused contracts, without compilation or VM creation")
    check.add_argument("suite", choices=SUITES)
    check.add_argument("--plan", action="store_true", help="print the commands and requirements without running")
    check.add_argument("--out", type=Path, help="new receipt directory (default: target/dev-runs/<unique-id>)")
    check.add_argument("--keep-going", action="store_true", help="run independent remaining checks after failure")
    check.add_argument("--step", action="append", help="run only this named contract (repeatable; use --plan to list steps)")
    worktree = commands.add_parser("worktree", help="create a separate task worktree beside the shared Git directory")
    worktree.add_argument("name", help="lowercase task name; creates branch codex/<name>")
    worktree.add_argument("--base", default="HEAD", help="committed source revision (default: HEAD); does not copy uncommitted edits")
    args = parser.parse_args()
    try:
        if args.command == "worktree":
            print(json.dumps(task_worktree(args.name, args.base), indent=2))
            return 0
        selection = plan(args.suite, steps=args.step)
        if args.plan:
            print(json.dumps(selection, indent=2))
            return 0
        out = args.out or ROOT / "target/dev-runs" / uuid.uuid4().hex
        return run_plan(selection, out.resolve(), keep_going=args.keep_going)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"dev check: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
