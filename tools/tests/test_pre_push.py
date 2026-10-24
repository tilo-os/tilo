"""The pre-push hook (.githooks/pre-push) and what it runs (tools/ci/pre_push.py)."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
import pre_push  # noqa: E402
import t_matrix  # noqa: E402

# What a push to main must never wait for: tests, VM checks, images, suites, previews.
TEST_FAMILIES = ("test/", "test-x86_64/", "goldens/", "check/")


def is_test(name: str) -> bool:
    return name.startswith(TEST_FAMILIES) or name.startswith("suite-") or name.startswith("policy/tests/")


class SelectTests(unittest.TestCase):
    def test_a_push_runs_no_test_vm_suite_or_whole_tree_check_whatever_it_touches(self):
        config = t_matrix.load(ROOT)
        checks = {check["name"]: check for check in config["check"]}
        # Everything t could name for a change: every command check, and every family's check of
        # two packages.
        families = [family["name"] for family in config.get("cargo", [])]
        scope = {"base": "b" * 40,
                 "checks": [*checks, *(f"{family}/{package}" for family in families for package in ("shell", "notes"))]}
        chosen = pre_push.select(scope, ["crates/session/shell/src/lib.rs"])
        run = chosen["run"]
        self.assertEqual([name for name in run if is_test(name)], [])
        self.assertNotIn("fmt", run, "the whole tree's fmt stays on main; a push formats what it changed")
        self.assertIn("fmt-changed", run)
        self.assertIn("changes", run)
        self.assertIn("clippy/shell", run)
        for name in run:
            with self.subTest(check=name):
                if name.startswith("clippy/"):
                    continue
                check = checks[name]
                self.assertNotEqual(check.get("isolation"), "vm")
                self.assertFalse(check.get("vm") or check.get("privileged"))
                self.assertFalse(check.get("cargo_packages"), "a push builds no package beyond clippy's")
                self.assertNotIn(check.get("cost"), ("minutes", "heavy"))
                self.assertTrue(name.startswith("policy/") or name in pre_push.GATES or name in pre_push.ALWAYS)
        # Everything else is left to main and to `t test`.
        self.assertEqual(set(run) | set(chosen["left"]), set(scope["checks"]) | set(pre_push.ALWAYS))

    def test_the_gates_and_always_run_checks_are_cheap_checks_of_t_toml(self):
        checks = {check["name"]: check for check in t_matrix.load(ROOT)["check"]}
        for name in (*pre_push.GATES, *pre_push.ALWAYS):
            with self.subTest(check=name):
                self.assertIn(name, checks)
                self.assertNotEqual(checks[name].get("isolation"), "vm")
                self.assertLessEqual(checks[name]["requirement"]["cores"], 8)
        self.assertIs(checks["fmt-changed"].get("default"), False)

    def test_policy_tests_run_only_when_the_policy_tools_change(self):
        scope = {"base": "b" * 40, "checks": ["policy/formats", "policy/tests/unit"]}
        self.assertEqual(pre_push.select(scope, ["apps/notes/src/lib.rs"])["run"],
                         ["fmt-changed", "changes", "policy/formats"])
        self.assertIn("policy/tests/unit",
                      pre_push.select(scope, ["tools/policy/check_formats.py"])["run"])

    def test_a_check_red_on_main_runs_all_the_same(self):
        # t compares its failures with main's: a push that adds one is refused however red main is.
        scope = {"base": "base", "checks": ["clippy/shell", "policy/source-file-size"]}
        self.assertEqual(pre_push.select(scope, ["crates/session/shell/src/lib.rs"])["run"],
                         ["fmt-changed", "changes", "clippy/shell", "policy/source-file-size"])

    def test_a_change_to_a_library_lints_that_library_and_none_of_its_dependents(self):
        # t names the lint of every package whose key the change moves: the library's
        # dependents too. A push lints only the packages whose files it changes.
        scope = {"base": "base", "checks": ["clippy/tilo-desktop-entry", "clippy/shell", "clippy/settings"]}
        chosen = pre_push.select(scope, ["crates/system/desktop-entry/src/entry.rs"])
        self.assertEqual(chosen["run"], ["fmt-changed", "changes", "clippy/tilo-desktop-entry"])
        self.assertEqual(chosen["left"], ["clippy/shell", "clippy/settings"])
        both = pre_push.select(scope, ["crates/system/desktop-entry/src/entry.rs",
                                       "crates/session/shell/src/shell/window_actions.rs"])
        self.assertEqual(both["run"], ["fmt-changed", "changes", "clippy/tilo-desktop-entry", "clippy/shell"])
        # Outside every package (the workspace manifest, docs) nothing is linted.
        self.assertEqual(pre_push.select(scope, ["Cargo.toml", "docs/t.md"])["run"], ["fmt-changed", "changes"])

    def test_an_app_s_change_selects_its_aarch64_size_check_and_a_library_s_none(self):
        scope = {"base": "base", "checks": []}
        chosen = pre_push.select(scope, ["apps/notes/src/lib.rs", "apps/notes/locales/en-US.ftl",
                                         "apps/messages/src/app.rs", "docs/t.md"])
        self.assertEqual(chosen["sizes"], ["app-size/messages-aarch64", "app-size/notes-aarch64"])
        # A shared crate reaches most apps: main's whole-collection size gate holds it.
        self.assertEqual(pre_push.select(scope, ["crates/sdk/ui/src/lib.rs"])["sizes"], [])
        checks = {check["name"] for check in t_matrix.load(ROOT)["check"]}
        for name in chosen["sizes"]:
            self.assertIn(name, checks)
        self.assertIn(pre_push.SIZE_RUNTIME, checks)

    def test_a_runtime_t_has_not_built_skips_the_size_checks_says_so_and_records_it(self):
        checks = ["app-size/notes-aarch64"]
        built = pre_push.runtime_built
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "t-gate-size-skips"
            try:
                pre_push.runtime_built = lambda top, rev: True
                self.assertEqual(pre_push.sizes(".", "a" * 40, checks, record), checks)
                self.assertFalse(record.exists())
                pre_push.runtime_built = lambda top, rev: False
                self.assertEqual(pre_push.sizes(".", "a" * 40, checks, record), [])
            finally:
                pre_push.runtime_built = built
            lines = record.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(lines), 1)
        self.assertTrue(lines[0].endswith(" aaaaaaaaaa runtime-not-built app-size/notes-aarch64"), lines[0])

    def test_only_a_run_with_every_check_cached_counts_as_built(self):
        self.assertTrue(pre_push.all_cached({"type": "planned", "checks": ["thin-runtime-aarch64"], "cached": 1}))
        self.assertFalse(pre_push.all_cached({"type": "planned", "checks": ["thin-runtime-aarch64"], "cached": 0}))
        self.assertFalse(pre_push.all_cached({"type": "planned", "checks": [], "cached": 0}))
        self.assertFalse(pre_push.all_cached(None))

    def test_opt_in_names_what_t_suggest_says_the_change_touches(self):
        suggest = {"checks": [
            {"check": "test-x86_64/shell", "reason": "opt-in; your change touches it: `t test test-x86_64/shell`"},
            {"check": "t-toml", "reason": "failed for you recently"},
        ]}
        self.assertEqual(pre_push.opt_in(suggest), ["test-x86_64/shell"])
        # One line, whatever the change touches; the push runs fmt-changed itself.
        suggest["checks"] += [{"check": name, "reason": "opt-in; your change touches it"}
                              for name in ["fmt-changed", *(f"suite-{n}" for n in range(10))]]
        line = pre_push.opt_in_line(pre_push.opt_in(suggest))
        self.assertEqual(line, "not run before a push; main runs them, or run them yourself: t test "
                               "test-x86_64/shell suite-0 suite-1 suite-2 suite-3 suite-4 suite-5 suite-6 "
                               "(and 3 more: t suggest)")
        self.assertEqual(pre_push.opt_in_line([]), "")


def result(check: str, outcome: str, excerpt: str = "", **extra) -> dict:
    failures = [{"test": None, "excerpt": excerpt}] if excerpt else []
    return {"type": "result", "check": check, "outcome": outcome, "failures": failures, **extra}


class UnpassedTests(unittest.TestCase):
    def test_a_check_that_could_not_run_is_named_as_t_s_failure_with_its_reason(self):
        events = [{"type": "planned", "run": "r1", "checks": ["changes", "clippy/shell"]},
                  result("changes", "pass"),
                  result("clippy/shell", "error", "the machine running it ran out of disk space\nmore"),
                  {"type": "done", "exit_code": 2, "run": "r1"}]
        self.assertEqual(pre_push.unpassed(events, ["changes", "clippy/shell"]), [
            "clippy/shell: could not run, a failure of t's infrastructure, not of the change: "
            "the machine running it ran out of disk space"])

    def test_every_outcome_but_pass_is_named(self):
        events = [result("a", "fail", "boom", blame={"kind": "inherited"}), result("b", "fail", "bang"),
                  result("c", "cancelled"), result("d", "skipped"), result("e", "later"),
                  {"type": "error", "message": "503 from the server", "hint": "t status"}]
        self.assertEqual(pre_push.unpassed(events, ["a", "b", "c", "d", "e", "f"]), [
            "t failed, not a check: 503 from the server (t status)",
            "a: fails (main fails it too, as a whole: it found nothing it names, so it can't be compared "
            "finding by finding): boom",
            "b: fails: bang",
            "c: cancelled before it finished",
            "d: not run, since a check it is gated on failed",
            "e: later, which this hook can't tell apart from a failure",
            "f: no result; the run ended before t reported one, a failure of t, not of the change"])

    def test_what_fails_on_main_too_says_how_it_was_compared(self):
        def finding(test: str, kind: str) -> dict:
            return {"test": test, "excerpt": test, "blame": {"kind": kind}}
        inherited = {"kind": "inherited"}
        by_finding = result("policy/source-file-size", "fail", blame=inherited,
                            failures=[finding("apps/a.rs: N lines, over its ceiling of N", "inherited"),
                                      finding("source file size: N problem(s)", "inherited")])
        whole = result("clippy/agent", "fail", "not checked: shell did not compile, which it builds on",
                       blame=inherited)
        self.assertEqual(pre_push.unpassed([by_finding, whole], ["policy/source-file-size", "clippy/agent"]), [
            "policy/source-file-size: fails (main fails it too: each of its 2 findings, compared one by one): "
            "apps/a.rs: N lines, over its ceiling of N",
            "clippy/agent: fails (main fails it too, as a whole: it found nothing it names, so it can't be "
            "compared finding by finding): not checked: shell did not compile, which it builds on"])
        # Main's result named no findings (it predates them, or could not compile): as a whole.
        unnamed = result("clippy/shell", "fail", blame=inherited,
                         failures=[finding("clippy::redundant_clone crates/session/shell/src/a.rs", "unknown")])
        self.assertEqual(pre_push.unpassed([unnamed], ["clippy/shell"]), [
            "clippy/shell: fails (main fails it too, as a whole: main's result names no findings to compare its 1 "
            "with one by one): clippy::redundant_clone crates/session/shell/src/a.rs"])
        # A test check whose tests did not run is not main's for main failing it as a whole.
        untested = result("policy/tests/unit", "fail", "not tested: the build failed", blame={"kind": "uncompared"})
        self.assertEqual(pre_push.unpassed([untested], ["policy/tests/unit"]), [
            "policy/tests/unit: fails (it failed as a whole, so what fails can't be compared with main's one by one: "
            "its tests did not run, or main's failure named findings this one doesn't; main failing too doesn't make "
            "it main's): not tested: the build failed"])
        # A finding main doesn't have is named, whatever else main fails too.
        added = result("policy/source-file-size", "fail", blame={"kind": "new"},
                       failures=[finding("apps/a.rs: N lines, over its ceiling of N", "inherited"),
                                 finding("apps/b.rs: N lines, over its ceiling of N", "new")])
        self.assertEqual(pre_push.unpassed([added], ["policy/source-file-size"]), [
            "policy/source-file-size: fails (not on main: apps/b.rs: N lines, over its ceiling of N): "
            "apps/a.rs: N lines, over its ceiling of N"])

    def test_a_run_that_passed_names_nothing(self):
        self.assertEqual(pre_push.unpassed([result("a", "pass")], ["a"]), [])


FAKE_T = r'''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_T_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
command = args[0]
if command == "test" and "--help" in args:
    print("--rev <COMMIT>")
elif command == "scope":
    print(json.dumps({"base": os.environ["FAKE_BASE"], "checks": [
        "test-x86_64/shell", "test/notes", "clippy/shell", "clippy/notes", "security", "suite-os",
        "desktop-preflight", "policy/formats", "fmt", "changes"]}))
elif command == "suggest":
    print(json.dumps({"checks": [{"check": "test-x86_64/shell", "reason": "opt-in; your change touches it"}]}))
elif command == "test":
    sys.stdout.write(os.environ.get("FAKE_RUN") or json.dumps({"type": "done", "run": "r1"}) + "\n")
    sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
elif command == "show":
    print("(the run, as t shows it)")
'''


class HookTests(unittest.TestCase):
    def git(self, *args: str, cwd: Path | None = None, **kwargs) -> subprocess.CompletedProcess:
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.org", GIT_COMMITTER_NAME="t",
                   GIT_COMMITTER_EMAIL="t@example.org", **kwargs.pop("env", {}))
        return subprocess.run(["git", *args], cwd=cwd or self.work, env=env, capture_output=True, text=True,
                              check=kwargs.pop("check", True), **kwargs)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        self.work, remote, bin_dir = root / "work", root / "remote.git", root / "bin"
        bin_dir.mkdir()
        (bin_dir / "t").write_text(FAKE_T, encoding="utf-8")
        (bin_dir / "t").chmod(0o755)
        self.log = root / "t.log"
        self.git("init", "-q", "--bare", "-b", "main", str(remote), cwd=root)
        self.git("init", "-q", "-b", "main", str(self.work), cwd=root)
        for path in (".githooks/pre-push", "tools/ci/pre_push.py"):
            (self.work / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, self.work / path)
        for package in ("shell", "notes"):
            (self.work / package / "src").mkdir(parents=True)
            (self.work / package / "Cargo.toml").write_text(f'[package]\nname = "{package}"\n', encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "chore: base")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("remote", "add", "origin", str(remote))
        self.git("push", "-q", "origin", "main")
        (self.work / "shell/src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (self.work / "notes/src/lib.rs").write_text("pub fn note() {}\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "feat: a change")
        self.git("config", "core.hooksPath", ".githooks")
        self.env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_T_LOG": str(self.log),
                    "FAKE_BASE": self.base}

    def tearDown(self):
        self.directory.cleanup()

    def test_a_push_to_main_runs_no_tests_and_names_the_opt_in_checks(self):
        pushed = self.git("push", "origin", "main", env=self.env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        calls = [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()]
        runs = [call for call in calls if call[0] == "test" and "--help" not in call]
        self.assertEqual(len(runs), 1, calls)
        run = runs[0]
        self.assertEqual(run[run.index("--rev") + 1], self.git("rev-parse", "HEAD").stdout.strip())
        self.assertEqual(run[run.index("--priority") + 1], "high")
        self.assertEqual(run[run.index("--json") + 1:],
                         ["fmt-changed", "changes", "clippy/shell", "clippy/notes", "policy/formats"])
        self.assertFalse(any(call[0] == "main" for call in calls), "nothing is skipped for being red on main")
        self.assertIn("t test test-x86_64/shell", pushed.stderr)
        # It names what t suggests and runs none of it.
        self.assertFalse(any("test-x86_64/shell" in run for run in runs))

    def gate_runs(self) -> int:
        calls = [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()]
        return len([call for call in calls if call[0] == "test" and "--help" not in call])

    def test_a_change_that_passed_passes_again_once_rebased_onto_a_newer_main(self):
        # The gate passes, and main moves meanwhile: the push is refused, the change rebased.
        pushed = self.git("push", "origin", "main", env=self.env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        self.assertEqual(self.gate_runs(), 1)
        self.git("push", "-q", "--no-verify", "-f", "origin", f"{self.base}:main")
        self.git("branch", "change", "HEAD")
        self.git("reset", "-q", "--hard", self.base)
        (self.work / "other.rs").write_text("fn other() {}\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "feat: someone else's change")
        self.git("push", "-q", "--no-verify", "origin", "main")
        self.git("cherry-pick", "change")
        # The same patch on the newer main: it passes without running the gate again.
        pushed = self.git("push", "origin", "main", env=self.env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        self.assertIn("this change passed the gate as", pushed.stderr)
        self.assertEqual(self.gate_runs(), 1)
        # Another patch runs it.
        (self.work / "shell/src/main.rs").write_text("fn main() { let _x = 1; }\n", encoding="utf-8")
        self.git("commit", "-qam", "feat: a change of the change")
        pushed = self.git("push", "origin", "main", env=self.env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        self.assertEqual(self.gate_runs(), 2)

    def test_a_refusal_names_each_check_that_could_not_run_and_why(self):
        run = [{"type": "planned", "run": "r9", "checks": ["fmt-changed", "changes", "clippy/shell", "clippy/notes", "policy/formats"]},
               result("fmt-changed", "pass"), result("changes", "pass"), result("policy/formats", "pass"),
               result("clippy/shell", "pass"),
               result("clippy/notes", "error", "the machine running it ran out of disk space"),
               {"type": "done", "exit_code": 2, "run": "r9"}]
        env = {**self.env, "FAKE_EXIT": "2", "FAKE_RUN": "".join(json.dumps(event) + "\n" for event in run)}
        pushed = self.git("push", "origin", "main", env=env, check=False)
        self.assertNotEqual(pushed.returncode, 0)
        self.assertIn("a failure of t, not of the change", pushed.stderr)
        self.assertIn("    clippy/notes: could not run, a failure of t's infrastructure, not of the change: "
                      "the machine running it ran out of disk space", pushed.stderr)
        self.assertNotIn("no-verify", pushed.stderr)

    def test_a_refusal_for_a_failure_names_the_check(self):
        run = [{"type": "planned", "run": "r9", "checks": ["fmt-changed", "changes", "clippy/shell", "clippy/notes", "policy/formats"]},
               result("fmt-changed", "pass"), result("changes", "fail", "a commit message says nothing"),
               result("policy/formats", "pass"), result("clippy/notes", "pass"), result("clippy/shell", "pass"),
               {"type": "done", "exit_code": 1, "run": "r9"}]
        env = {**self.env, "FAKE_EXIT": "1", "FAKE_RUN": "".join(json.dumps(event) + "\n" for event in run)}
        pushed = self.git("push", "origin", "main", env=env, check=False)
        self.assertNotEqual(pushed.returncode, 0)
        self.assertIn("    changes: fails: a commit message says nothing", pushed.stderr)

    def test_a_pass_names_what_fails_on_main_too_and_how_it_was_compared(self):
        findings = [{"test": "clippy::redundant_clone crates/session/shell/src/a.rs", "excerpt": "error: redundant clone",
                     "blame": {"kind": "inherited"}}]
        run = [{"type": "planned", "run": "r9", "checks": ["fmt-changed", "changes", "clippy/shell", "clippy/notes", "policy/formats"]},
               result("fmt-changed", "pass"), result("changes", "pass"), result("policy/formats", "pass"),
               result("clippy/shell", "fail", blame={"kind": "inherited"}, failures=findings),
               result("clippy/notes", "fail", "not checked: shell did not compile, which it builds on",
                      blame={"kind": "inherited"}),
               {"type": "done", "exit_code": 3, "run": "r9"}]
        env = {**self.env, "FAKE_EXIT": "3", "FAKE_RUN": "".join(json.dumps(event) + "\n" for event in run)}
        pushed = self.git("push", "origin", "main", env=env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        self.assertIn("what fails, fails on main too", pushed.stderr)
        self.assertIn("    clippy/shell: fails (main fails it too: each of its 1 findings, compared one by one)",
                      pushed.stderr)
        self.assertIn("    clippy/notes: fails (main fails it too, as a whole: it found nothing it names, so it "
                      "can't be compared finding by finding)", pushed.stderr)

    def test_a_push_to_another_branch_runs_nothing(self):
        self.git("push", "-q", "origin", "HEAD:topic", env=self.env)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
