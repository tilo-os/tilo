"""A check judges one file by its own text, so a commit judges its own files.

The pre-commit hook runs these checks with ``--staged`` (tools/policy/judged.py):
the files a commit stages, read from the index, instead of every file of the
tree. That is only sound if the commit's files are judged exactly as the tree's
are, and if nothing else is read. Each case below builds a repository and
proves, for one check:

* the whole-tree run finds the violation, in a file the commit stages or not --
  tools/policy/run.sh and CI keep that run;
* the staged run finds the same violation when the commit stages the file, and
  reads it as committed, not as the working tree has it after further edits;
* the staged run reports nothing for a violation only in the working tree, and
  reads no Rust file the commit did not stage, so the hook stays as cheap as
  the change.
"""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


POLICY = Path(__file__).resolve().parents[1] / "policy"
HOOK = Path(__file__).resolve().parents[2] / ".githooks" / "pre-commit"
STEPS = POLICY / "steps.toml"


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, POLICY / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


judged_module = load("judged")


class Repository:
    """A git repository with one commit, whose files a test stages again."""

    def __init__(self, files: dict[str, str]):
        temporary = tempfile.TemporaryDirectory()
        self.cleanup = temporary.cleanup
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        self.write(files)
        self.git("add", "--", ".")
        self.git(
            "-c", "user.name=Policy Test", "-c", "user.email=policy@example.org",
            "commit", "-qm", "the tree",
        )

    def git(self, *args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(self.root), *args],
            capture_output=True, text=True, check=True,
        ).stdout

    def write(self, files: dict[str, str]) -> None:
        for relative, text in files.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def stage(self, files: dict[str, str]) -> None:
        self.write(files)
        self.git("add", "--", ".")

    def staged_names(self) -> set[str]:
        return {
            name
            for name in self.git("diff", "--cached", "--name-only").splitlines()
            if name
        }


class Case:
    """One check, a tree it judges, and the file whose violation it reports."""

    def __init__(
        self,
        check: str,
        files: dict[str, str],
        violation: dict[str, str],
        unstaged: dict[str, str] | None = None,
    ):
        self.check = check
        self.files = files
        self.violation = violation
        # The same violation in the working tree for a check that only judges
        # tracked files: the path must be committed, then grown without staging.
        self.unstaged = violation if unstaged is None else unstaged
        self.module = load(check)

    def verdict(self, repository: Repository, staged: bool) -> list[str]:
        """The paths of the files the check reports, relative to the root."""
        module = self.module
        if self.check == "check_integration_test_modules":
            return [entry.split(":", 1)[0] for entry in module.orphans(repository.root, staged)]
        if self.check == "check_app_text_contract":
            found = module.audit(repository.root, staged)[0]
            return [Path(v.path).relative_to(repository.root).as_posix() for v in found]
        if self.check == "check_swallowed_failures":
            return [entry.split(":", 1)[0] for entry in module.audit(repository.root, staged)[0]]
        if self.check == "check_ui_host_inputs_contract":
            return [entry.split(":", 1)[0] for entry in module.audit(repository.root, staged)]
        if self.check == "check_ui_clock_contract":
            return [v.path for v in module.audit(repository.root, staged)]
        if self.check == "check_formats":
            return [v.path for v in module.audit(repository.root, staged)]
        if self.check == "check_settings_writes":
            return [v.path for v in module.audit(repository.root, staged)]
        if self.check == "check_too_many_arguments":
            return [entry.split(":", 1)[0] for entry in module.audit(repository.root, staged)]
        if self.check == "check_source_file_size":
            return [entry.split(":", 1)[0] for entry in module.problems(repository.root, staged)]
        raise AssertionError(f"no verdict for {self.check}")


# The runner files a fixture lists so the host-inputs check sees its inventory.
NATIVE_RUNNER_FILES = load("check_ui_host_inputs_contract").NATIVE_RUNNER_FILES

CASES = (
    Case(
        check="check_ui_clock_contract",
        files={
            "clippy.toml": "allow-unwrap-in-tests = true\n",
            "crates/sdk/ui-core/clippy.toml": "allow-unwrap-in-tests = true\n",
            "crates/sdk/ui-core/src/lib.rs": "pub fn run() {}\n",
        },
        violation={
            "crates/sdk/ui-core/src/wall.rs": (
                "pub fn deadline() -> std::time::Instant { std::time::Instant::now() }\n"
            ),
        },
    ),
    Case(
        check="check_formats",
        files={"crates/sdk/ui-core/src/lib.rs": "pub fn run() {}\n"},
        violation={
            "crates/sdk/ui-core/src/row.rs": (
                'pub fn stamp(when: Time) -> String { when.format("%b %d").to_string() }\n'
            ),
        },
    ),
    Case(
        check="check_settings_writes",
        files={"crates/settings-app/src/lib.rs": "pub fn run() {}\n"},
        violation={
            "crates/settings-app/src/input.rs": (
                'pub fn save(values: Map) { settings_store::update_section("input", values); }\n'
            ),
        },
    ),
    Case(
        check="check_ui_host_inputs_contract",
        files={
            "crates/sdk/ui-core/src/lib.rs": "pub fn run() {}\n",
            **{f"crates/sdk/ui-core/src/{name}": "" for name in NATIVE_RUNNER_FILES},
        },
        violation={
            "crates/sdk/ui-core/src/portable.rs": (
                'pub fn home() -> Option<String> { env::var("HOME").ok() }\n'
            ),
        },
    ),
    Case(
        check="check_app_text_contract",
        files={
            "apps/demo/Cargo.toml": (
                '[package]\nname = "demo"\nversion = "0.0.0"\n\n'
                "[dependencies]\nui = { workspace = true }\n"
            ),
            "apps/demo/src/lib.rs": "pub fn run() {}\n",
        },
        violation={
            "apps/demo/src/text.rs": "pub fn shape() { let _ = cosmic_text::Font::new(); }\n",
        },
    ),
    Case(
        check="check_swallowed_failures",
        files={"crates/session/session/src/lib.rs": "pub fn run() {}\n"},
        violation={
            "crates/session/session/src/store.rs": (
                "pub fn load(bytes: &[u8]) { let _ = Frame::decode(bytes); }\n"
            ),
        },
    ),
    Case(
        check="check_integration_test_modules",
        files={
            "Cargo.toml": '[workspace]\nmembers = ["crates/y"]\n',
            "crates/y/Cargo.toml": (
                '[package]\nname = "y"\nversion = "0.0.0"\nautotests = false\n\n'
                '[[test]]\nname = "it"\npath = "tests/it.rs"\n'
            ),
            "crates/y/tests/it.rs": "mod kept;\n",
            "crates/y/tests/kept.rs": "",
        },
        violation={"crates/y/tests/orphan.rs": ""},
    ),
    Case(
        check="check_source_file_size",
        files={
            "tools/policy/source_file_size_baseline.toml": "[ceilings]\n",
            "crates/x/src/lib.rs": "pub fn run() {}\n",
            "crates/x/src/huge.rs": "pub fn wide() {}\n",
        },
        violation={"crates/x/src/huge.rs": "pub fn wide() {}\n" * 3001},
    ),
    Case(
        check="check_too_many_arguments",
        files={"crates/x/src/lib.rs": "pub fn run() {}\n"},
        violation={
            "crates/x/src/wide.rs": "#[allow(clippy::too_many_arguments)]\npub fn wide() {}\n",
        },
    ),
)


def flagged_paths(case: Case) -> set[str]:
    return set(case.violation)


class StagedJudgementTests(unittest.TestCase):
    def repository(self, case: Case) -> Repository:
        repository = Repository(case.files)
        self.addCleanup(repository.cleanup)
        return repository

    def test_a_staged_violation_is_found_by_both_runs(self):
        for case in CASES:
            with self.subTest(check=case.check):
                repository = self.repository(case)
                repository.stage(case.violation)
                flagged = flagged_paths(case)
                self.assertLessEqual(
                    flagged, set(case.verdict(repository, staged=False)),
                    "the whole-tree run must report it",
                )
                self.assertLessEqual(
                    flagged, set(case.verdict(repository, staged=True)),
                    "the commit's run must report it",
                )

    def test_a_violation_only_in_the_working_tree_is_left_to_the_whole_tree_run(self):
        for case in CASES:
            with self.subTest(check=case.check):
                repository = self.repository(case)
                repository.write(case.unstaged)
                self.assertEqual(set(), repository.staged_names())
                self.assertLessEqual(flagged_paths(case), set(case.verdict(repository, staged=False)))
                self.assertEqual(set(), flagged_paths(case) & set(case.verdict(repository, staged=True)))

    def test_a_staged_file_is_judged_as_committed_not_as_edited(self):
        for case in CASES:
            with self.subTest(check=case.check):
                repository = self.repository(case)
                repository.stage(case.violation)
                # The same paths, fixed in the working tree and never staged:
                # the commit still carries what the index holds.
                clean = {path: "pub fn run() {}\n" for path in case.violation}
                if case.check == "check_integration_test_modules":
                    clean = {"crates/y/tests/orphan.rs": "mod kept;\n"}
                repository.write(clean)
                self.assertLessEqual(flagged_paths(case), set(case.verdict(repository, staged=True)))

    def test_the_commit_run_reads_no_rust_file_the_commit_does_not_stage(self):
        for case in CASES:
            with self.subTest(check=case.check):
                repository = self.repository(case)
                repository.stage(case.violation)
                staged = {name for name in repository.staged_names() if name.endswith(".rs")}
                read: list[Path] = []
                self.addCleanup(setattr, case.module, "judged", case.module.judged)
                case.module.judged = counting_reads(read)
                case.verdict(repository, staged=True)
                self.assertLessEqual(
                    {path.as_posix() for path in read if path.suffix == ".rs"},
                    staged,
                    "the commit's run read Rust files the commit does not stage",
                )


def counting_reads(read: list[Path]):
    """A ``judged`` factory whose judgement records the files it reads."""

    def factory(root: Path, staged: bool = False):
        judgement = judged_module.judged(root, staged)
        reading = judgement.read

        def counted(relative, errors="replace"):
            read.append(Path(relative))
            return reading(relative, errors)

        judgement.read = counted
        return judgement

    return factory


class JudgedTests(unittest.TestCase):
    def test_a_staged_file_is_read_from_the_index(self):
        repository = Repository({"crates/x/src/lib.rs": "committed\n"})
        self.addCleanup(repository.cleanup)
        repository.stage({"crates/x/src/lib.rs": "staged\n"})
        repository.write({"crates/x/src/lib.rs": "working tree\n"})
        files = judged_module.judged(repository.root, staged=True)
        self.assertEqual([Path("crates/x/src/lib.rs")], files.files("crates"))
        self.assertEqual("staged\n", files.read("crates/x/src/lib.rs"))
        self.assertEqual(
            "working tree\n",
            judged_module.judged(repository.root).read("crates/x/src/lib.rs"),
        )

    def test_a_file_the_commit_deletes_is_no_file_to_judge(self):
        repository = Repository({"crates/x/src/lib.rs": "committed\n"})
        self.addCleanup(repository.cleanup)
        self.assertEqual(
            [], list(judged_module.judged(repository.root, staged=True).sources("crates"))
        )
        repository.git("rm", "-q", "crates/x/src/lib.rs")
        self.assertEqual(
            [], list(judged_module.judged(repository.root, staged=True).sources("crates"))
        )

    def test_a_scope_narrows_the_commit_to_the_files_under_it(self):
        repository = Repository({"crates/x/src/lib.rs": "a\n", "apps/y/src/lib.rs": "b\n"})
        self.addCleanup(repository.cleanup)
        repository.stage({"crates/x/src/b.rs": "c\n", "apps/y/src/lib.rs": "d\n"})
        files = judged_module.judged(repository.root, staged=True)
        self.assertEqual([Path("crates/x/src/b.rs")], files.files("crates/x"))
        self.assertEqual(
            [Path("apps/y/src/lib.rs"), Path("crates/x/src/b.rs")],
            files.files("apps", "crates"),
        )

    def test_holding_keeps_only_the_files_that_can_match(self):
        repository = Repository(
            {
                "crates/x/src/a.rs": "pub fn decode() -> Result<()> {}\n",
                "crates/x/src/b.rs": "fn other() {}\n",
            }
        )
        self.addCleanup(repository.cleanup)
        whole = judged_module.judged(repository.root)
        narrowed = whole.holding("fn[[:space:]]+(decode)", "crates/x")
        self.assertEqual([Path("crates/x/src/a.rs")], narrowed.files("crates/x"))
        self.assertEqual([], whole.holding("fn[[:space:]]+(nowhere)", "crates/x").files("crates/x"))


class HookWiringTests(unittest.TestCase):
    """The hook judges the commit; the policy suite and CI judge the tree."""

    STAGED_IN_HOOK = (
        "check_app_text_contract",
        "check_ui_host_inputs_contract",
        "check_ui_clock_contract",
        "check_formats",
        "check_settings_writes",
        "check_swallowed_failures",
        "check_integration_test_modules",
        "check_too_many_arguments",
        "check_source_file_size",
    )

    def test_the_hook_judges_the_commit_it_is_committing(self):
        hook = HOOK.read_text(encoding="utf-8")
        steps = STEPS.read_text(encoding="utf-8")
        for check in self.STAGED_IN_HOOK:
            with self.subTest(check=check):
                lines = [
                    line
                    for line in hook.splitlines()
                    if f"tools/policy/{check}.py" in line and not line.lstrip().startswith("#")
                ]
                self.assertEqual(1, len(lines), f"the hook runs {check} once")
                self.assertIn("--staged", lines[0])
                self.assertIn(
                    f"python3 tools/policy/{check}.py",
                    steps,
                    f"{check} must still judge the whole tree in the policy suite",
                )
                self.assertNotIn(f"python3 tools/policy/{check}.py --staged", steps)


if __name__ == "__main__":
    unittest.main()
