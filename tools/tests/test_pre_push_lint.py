import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
# What the hook runs, copied into a fixture workspace of its own.
HOOK_SOURCES = [
    ".githooks/pre-push",
    "tools/ci/pre_push_lint.py",
    "tools/ci/lint_inputs.py",
    "tools/ci/package_features.py",
    "tools/ci/package_sets.py",
    "tools/ci/wasm_clippy_conf.py",
    "tools/ci/workspace_graph.py",
    "tools/ci/suites.py",
    "tools/ci/suites.toml",
    "tools/ci/check-changed-rustfmt.sh",
    "tools/ci/comparison_range.py",
]
WORKSPACE = """[workspace]
members = ["native", "web", "fake-binding"]
resolver = "2"
"""
# The workspace's Clippy configuration: the page's own configuration is this one
# with std's clock reads added, so the page lint has one to read.
CLIPPY_TOML = """disallowed-methods = [
    { path = "std::env::set_var", reason = "injected" },
]
"""
CRATE = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
"""
LOCK = """version = 4

[[package]]
name = "fixture-native"
version = "0.1.0"

[[package]]
name = "fixture-web"
version = "0.1.0"
dependencies = ["wasm-bindgen"]

[[package]]
name = "wasm-bindgen"
version = "0.1.0"
"""
# A crate from outside the workspace, which the page crate reads and the native
# crate does not: main changing it is a change only one of them builds.
SERDE = """
[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "a"
"""
LOCKED_SERDE = LOCK.replace('dependencies = ["wasm-bindgen"]',
                            'dependencies = ["wasm-bindgen", "serde"]') + SERDE
PAGE_WITH_SERDE = (CRATE.format(name="fixture-web")
                   + '\n[dependencies]\nwasm-bindgen = { path = "../fake-binding" }\nserde = "1"\n')
LIB = """pub fn answer() -> u32 {{
    {answer}
}}
"""
# Cargo without a toolchain: metadata comes from the workspace this test
# describes, rustfmt is always happy, and Clippy takes a moment and then answers
# with the status the test asked for. Each compiling call records when it
# started and ended, so a test can tell whether two ran at the same time; the
# workspace remark is what a failure report must leave out.
CARGO_STUB = """#!/bin/sh
case "$1" in
  metadata) printf '%s\\n' "$STUB_METADATA"; exit 0 ;;
  fmt) printf 'fmt %s\\n' "$*" >> "$HOOK_LOG"; [ "$2" = "--version" ] && exit 0; exit "${STUB_FMT_STATUS:-0}" ;;
esac
printf 'start %s %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
sleep "${STUB_SLEEP:-0}"
printf 'warning: a/Cargo.toml: version requirement `1.0+meta` includes semver metadata\\n'
for n in $(seq 1 100); do printf '    Checking crate-%s v0.1.0\\n' "$n"; done
printf 'stub says: unreachable value in a page build\\n'
printf 'end %s %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
exit "${STUB_STATUS:-0}"
"""
# The same, named by $CARGO: a drop-in that may build somewhere else. It only
# builds: asked to format, it refuses, as a cargo that compiles on another
# machine does.
DROP_IN_STUB = (CARGO_STUB.replace("printf 'start ", "printf 'dropin ")
                .replace("printf 'end ", "printf 'dropin-end ")
                .replace('''  fmt) printf 'fmt %s\\n' "$*" >> "$HOOK_LOG"; [ "$2" = "--version" ] && exit 0; exit "${STUB_FMT_STATUS:-0}" ;;''',
                         '''  fmt) printf 'dropin-fmt %s\\n' "$*" >> "$HOOK_LOG"; exit 2 ;;'''))
assert "dropin-fmt" in DROP_IN_STUB, "the drop-in stub must refuse to format"
# The toolchain's installed targets, which say whether the page build can be linted here.
RUSTFMT_STUB = """#!/bin/sh
printf 'rustfmt %s\\n' "$*" >> "$HOOK_LOG"
# As rustfmt does: a difference names the file it is in.
if [ "${STUB_FMT_STATUS:-0}" != 0 ]; then
  for file in "$@"; do
    case "$file" in *.rs) printf 'Diff in %s:1:\\n-a\\n+b\\n' "$(realpath "$file")" ;; esac
  done
fi
exit "${STUB_FMT_STATUS:-0}"
"""
RUSTUP_STUB = """#!/bin/sh
case "$*" in
  "target list --installed") printf '%s\\n' "$STUB_TARGETS" ;;
esac
exit 0
"""
HOST_ONLY = "x86_64-unknown-linux-gnu"
WITH_PAGE = HOST_ONLY + "\nwasm32-unknown-unknown"
NATIVE_CLIPPY = "clippy --locked -p fixture-native --all-targets --keep-going -- -D warnings"


class PrePushLintTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.temporary = Path(temporary.name)
        self.root = self.temporary / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "main")
        for key, value in (("user.name", "Test"), ("user.email", "test@example.org"),
                           ("commit.gpgsign", "false")):
            self.git("config", key, value)
        for name in HOOK_SOURCES:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        (self.root / ".githooks/pre-push").chmod(0o755)
        # One host crate and one crate that builds for the page; the second says
        # so the way the workspace's page crates do, by depending on wasm-bindgen.
        self.write("Cargo.toml", WORKSPACE)
        self.write("Cargo.lock", LOCK)
        self.write("clippy.toml", CLIPPY_TOML)
        # Running the hook compiles its Python and writes the page's Clippy
        # configuration into the build directory; the repository ignores both.
        self.write(".gitignore", "__pycache__/\n.claude/\ntarget/\n")
        self.write("native/Cargo.toml", CRATE.format(name="fixture-native"))
        self.write("native/src/lib.rs", LIB.format(answer=42))
        self.write("web/Cargo.toml", CRATE.format(name="fixture-web")
                   + '\n[dependencies]\nwasm-bindgen = { path = "../fake-binding" }\n')
        self.write("web/src/lib.rs", LIB.format(answer=7))
        self.write("fake-binding/Cargo.toml", CRATE.format(name="wasm-bindgen"))
        self.write("fake-binding/src/lib.rs", "pub fn bind() {}\n")
        self.bin = self.temporary / "bin"
        self.bin.mkdir()
        self.stub("cargo", CARGO_STUB)
        self.stub("rustup", RUSTUP_STUB)
        self.stub("rustfmt", RUSTFMT_STUB)
        self.stub("drop-in", DROP_IN_STUB)
        self.log = self.temporary / "hook.log"
        # Whether the fixture has a page entry that compiles the host crate, and
        # which crates build on the host one.
        self.page_entry = False
        self.dependents = []
        self.commit("test: a workspace of one host crate and one page crate")
        self.base = self.head()

    def git(self, *args):
        return subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-C", str(self.root), *args],
                              capture_output=True, text=True, check=True).stdout

    def head(self):
        return self.git("rev-parse", "HEAD").strip()

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self, message):
        self.git("add", "--", ".")
        self.git("commit", "-qm", message)
        return self.head()

    def stub(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def metadata(self) -> str:
        """The Cargo metadata of the fixture (of the checkout the hook runs in): what it selects with."""
        root = getattr(self, "checkout", self.root)
        packages = [
            {"name": "fixture-native", "manifest_path": str(root / "native/Cargo.toml"),
             "dependencies": []},
            {"name": "fixture-web", "manifest_path": str(root / "web/Cargo.toml"),
             "dependencies": [{"name": "wasm-bindgen", "kind": None, "path": "fake-binding"}]},
            {"name": "wasm-bindgen", "manifest_path": str(root / "fake-binding/Cargo.toml"),
             "dependencies": []},
        ]
        for name in self.dependents:
            packages.append(
                {"name": name, "manifest_path": str(root / name / "Cargo.toml"),
                 "dependencies": [{"name": "fixture-native", "kind": None,
                                   "path": str(root / "native")}]})
        if self.page_entry:
            packages.append(
                {"name": "tilo-browser-runtime", "manifest_path": str(root / "page/Cargo.toml"),
                 "dependencies": [{"name": "fixture-web", "kind": None, "path": "web"},
                                  {"name": "fixture-native", "kind": None, "path": "native"}]})
        return json.dumps({"packages": packages})

    def environment(self, page=True, status=0, sleep=0, drop_in=False, unformatted=False,
                    agent=False) -> dict:
        """The hook's environment: only the stubs, never this machine's toolchain.

        `agent` pushes as a coding agent does, whose shell says so in its environment.
        """
        env = dict(os.environ)
        env.pop("CARGO", None)
        for marker in ("AI_AGENT", "CLAUDECODE"):
            env.pop(marker, None)
        if agent:
            env["AI_AGENT"] = "1"
        env["PATH"] = os.pathsep.join(
            [str(self.bin), *(entry for entry in env["PATH"].split(os.pathsep)
                               if not any(os.access(Path(entry) / tool, os.X_OK)
                                          for tool in ("cargo", "rustup")))])
        env.update(HOOK_LOG=str(self.log), STUB_STATUS=str(status), STUB_SLEEP=str(sleep),
                   STUB_METADATA=self.metadata(), STUB_FMT_STATUS="1" if unformatted else "0",
                   STUB_TARGETS=WITH_PAGE if page else HOST_ONLY)
        if drop_in:
            env["CARGO"] = str(self.bin / "drop-in")
        return env

    def push(self, head=None, remote_oid=None, ref="refs/heads/main", cwd=None, wide=False,
             also=(), **environment):
        """Run the hook as Git would, with the ref lines it writes for a push.

        `wide` runs the lint t's lint-head runs on main instead, over the same refs. `also`
        names more refs the same push sends the head to.
        """
        self.log.unlink(missing_ok=True)
        remote_oid = self.base if remote_oid is None else remote_oid
        cwd = cwd or self.root
        command = ([sys.executable, str(cwd / "tools/ci/pre_push_lint.py"), "--wide", "origin"] if wide
                   else [str(cwd / ".githooks/pre-push"), "origin", "file:///dev/null"])
        return subprocess.run(
            command, cwd=cwd,
            capture_output=True, text=True, env=self.environment(**environment), timeout=HANG_BOUND,
            input=f"{ref} {head or self.head()} {ref} {remote_oid}\n" + "".join(
                f"{other} {head or self.head()} {other} {'0' * 40}\n" for other in also),
        )

    def calls(self, prefix="start"):
        """The commands the stub was asked to compile."""
        if not self.log.exists():
            return []
        return [line.split(maxsplit=2)[2] for line in self.log.read_text().splitlines()
                if line.startswith(f"{prefix} ")]

    def test_a_push_without_a_rust_change_lints_nothing(self):
        self.write("docs/notes.md", "words\n")
        self.commit("docs: describe the fixture")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no Rust or Cargo file changed", result.stderr)
        self.assertFalse(self.log.exists(), "nothing should have been compiled")

    def add_dependent(self, name, source):
        """A crate that builds on the host one, with its own source."""
        self.dependents.append(name)
        self.write(f"{name}/Cargo.toml", CRATE.format(name=name)
                   + f'\n[dependencies]\nfixture-native = {{ path = "../native" }}\n')
        self.write(f"{name}/src/lib.rs", source)
        self.commit(f"feat: {name} uses the host crate")
        self.base = self.head()

    def test_a_dependent_that_names_what_the_change_removed_is_linted_too(self):
        # A rename in the declaring crate breaks the crates that spell the old
        # name, and those are the crates a lint of the declaring one never builds.
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.add_dependent("spare", "pub fn unrelated() -> u32 { 1 }\n")
        self.write("native/src/lib.rs", "pub fn solution() -> u32 {\n    42\n}\n")
        self.commit("refactor: the answer is the solution")
        result = self.push(wide=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("clippy --locked -p fixture-native -p user --all-targets "
                      "--keep-going -- -D warnings", self.calls())
        self.assertIn("built on them", result.stderr)

    def test_a_push_of_a_commit_that_is_not_checked_out_is_refused(self):
        # The lint reads the files on disk: sending a commit whose files those
        # are not would send it unlinted.
        self.write("native/src/lib.rs", LIB.format(answer=60))
        pushed = self.commit("feat: answer 60")
        self.write("docs/notes.md", "words\n")
        self.commit("docs: move on")
        result = self.push(head=pushed)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not the commit checked out here", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_push_to_another_branch_compiles_nothing(self):
        # Only main is linted; a branch of one's own is pushed as it is.
        self.write("native/src/lib.rs", LIB.format(answer=62))
        self.commit("feat: answer 62")
        result = self.push(ref="refs/heads/topic")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.log.exists(), "a push to a topic branch compiled something")

    def test_an_agent_pushes_main_directly(self):
        self.write("native/src/lib.rs", LIB.format(answer=64))
        self.commit("feat: answer 64")
        result = self.push(agent=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.calls(), "the push to main was not linted")

    def test_a_push_main_has_moved_past_is_refused_before_anything_is_compiled(self):
        # The remote reports main's head when the push begins; a push that does not contain it
        # is refused after the lint whatever the lint says, so the lint would only load the builder.
        self.write("native/src/lib.rs", LIB.format(answer=61))
        self.commit("feat: answer 61")
        result = self.push(remote_oid="1" * 40, ref="refs/heads/main")
        self.assertEqual(result.returncode, 1)
        self.assertIn("does not contain", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_lock_change_lints_the_members_it_reaches(self):
        # A resolved dependency that moved reaches the members that link it,
        # which the changed paths alone do not name.
        self.write("Cargo.lock", LOCK.replace(
            'name = "wasm-bindgen"\nversion = "0.1.0"', 'name = "wasm-bindgen"\nversion = "0.2.0"'))
        self.commit("chore: move the binding")
        result = self.push(wide=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        # The binding's own record is a member here, so it is linted as well.
        self.assertEqual(sorted(self.calls()), sorted([
            NATIVE_CLIPPY.replace("native", "web -p wasm-bindgen"),
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
        ]))

    def test_a_profile_section_for_one_member_lints_only_that_member(self):
        # `[profile.release.package.X]` decides how X alone is built, so a
        # section that moves from a package the workspace no longer has to a
        # member names that member, not every member of the workspace.
        self.write("Cargo.toml", WORKSPACE
                   + '\n[profile.release.package.fixture-gone]\nstrip = "symbols"\n')
        self.commit("chore: strip a package's symbols")
        self.base = self.head()
        self.write("Cargo.toml", WORKSPACE
                   + '\n[profile.release.package.fixture-web]\nstrip = "symbols"\n')
        self.commit("refactor: the web member is the one shipped stripped")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("workspace-wide lint", result.stderr)
        self.assertEqual([call for call in self.calls() if "-p fixture-native" in call], [],
                         "a section for another member did not reach the native member")
        self.assertTrue([call for call in self.calls() if "-p fixture-web" in call],
                        "the member whose section changed was not linted")

    def test_a_profile_change_for_every_member_lints_every_member(self):
        # A setting at the profile's own level is how every member is built,
        # so every member is linted, and the page with them.
        self.write("Cargo.toml", WORKSPACE + '\n[profile.release]\nstrip = "symbols"\n')
        self.commit("chore: strip every member")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("the whole workspace is linted", result.stderr)
        for member in ("fixture-native", "fixture-web"):
            self.assertTrue([call for call in self.calls() if f"-p {member}" in call],
                            f"{member} was not linted")

    def test_the_lint_gets_the_features_the_workspace_builds_with(self):
        # A member's code behind a feature no default build enables is compiled
        # only with the features the workspace's whole build resolves for it.
        self.write("crates/dev/workspace-unify/Cargo.toml",
                   '[package]\nname = "workspace-unify"\nversion = "0.0.0"\n\n'
                   "[target.'cfg(all(target_os = \"linux\", target_arch = \"x86_64\"))'.dependencies]\n"
                   '"fixture-native" = { path = "../../../native", default-features = false, '
                   'features = ["extra"] }\n')
        self.commit("chore: unify the features")
        self.base = self.head()
        self.write("native/src/lib.rs", LIB.format(answer=61))
        self.commit("feat: answer 61")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY.replace(
            "--all-targets", "--features fixture-native/extra --all-targets")])

    def test_a_host_crate_change_lints_the_host_only(self):
        self.write("native/src/lib.rs", LIB.format(answer=43))
        self.commit("feat: answer 43")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        self.assertIn("lint passed", result.stderr)

    def test_a_push_leaves_dependents_and_the_page_to_the_wide_lint(self):
        # Their rebuilds took minutes, and main moved on before they finished: a push lints
        # what it changes, and lint-head lints the rest on every commit of main.
        self.page_entry = True
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", "pub fn solution() -> u32 {\n    42\n}\n")
        self.commit("refactor: the answer is the solution")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_a_page_crate_change_lints_the_page_at_the_same_time(self):
        self.write("web/src/lib.rs", LIB.format(answer=8))
        self.commit("feat: answer 8 in the page")
        result = self.push(sleep=1, wide=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        # Which run started first is up to the scheduler; that both did is not.
        self.assertEqual(sorted(self.calls()), sorted([
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
            NATIVE_CLIPPY.replace("native", "web"),
        ]))
        moments = self.log.read_text().splitlines()
        started = [int(line.split()[1]) for line in moments if line.startswith("start ")]
        ended = [int(line.split()[1]) for line in moments if line.startswith("end ")]
        self.assertEqual(len(started), 2)
        self.assertLess(max(started), min(ended), "both lints must start before either finishes")
        self.assertIn("for the page", result.stderr)

    def test_a_host_crate_the_page_compiles_lints_the_page_too(self):
        # A crate with no wasm-bindgen of its own that the page still compiles:
        # a change linted only natively broke the page while it stayed green.
        self.page_entry = True
        self.write("native/src/lib.rs", LIB.format(answer=44))
        self.commit("feat: answer 44")
        result = self.push(wide=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(self.calls()), sorted([
            NATIVE_CLIPPY,
            "clippy --locked --target wasm32-unknown-unknown -p tilo-browser-runtime -- -D warnings",
        ]))

    def test_a_page_build_that_cannot_run_refuses_the_push(self):
        # A push that changes crates the page compiles is not linted for the page
        # by a toolchain that cannot build it: passing it would send the page's
        # side unlinted, which is how a wasm-only break reached main.
        self.write("web/src/lib.rs", LIB.format(answer=12))
        self.commit("feat: answer 12 in the page")
        result = self.push(page=False, wide=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.calls(), [], "nothing may be sent for a lint that cannot speak for it")
        self.assertIn("wasm32-unknown-unknown is not installed here", result.stderr)
        self.assertIn("rustup target add wasm32-unknown-unknown", result.stderr)

    def test_a_cargo_drop_in_compiles_instead_of_the_local_cargo(self):
        self.write("web/src/lib.rs", LIB.format(answer=13))
        self.commit("feat: answer 13 in the page")
        result = self.push(drop_in=True, page=False, wide=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(self.calls("dropin")), sorted([
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
            NATIVE_CLIPPY.replace("native", "web"),
        ]))
        self.assertEqual(self.calls(), [], "the local cargo must not compile at all")

    def test_formatting_is_checked_here_when_the_drop_in_only_builds(self):
        self.write("web/src/lib.rs", LIB.format(answer=14))
        self.commit("feat: answer 14 in the page")
        result = self.push(drop_in=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text().splitlines()
        self.assertIn("rustfmt --edition 2024 --check -- web/src/lib.rs", log,
                      "the rustfmt here must check the changed files")
        self.assertEqual([line for line in log if line.startswith("dropin-fmt ")], [],
                         "the drop-in builds; it must not be asked to format")
        self.assertNotIn("rustfmt is not installed", result.stderr)
        self.assertIn("clippy and rustfmt for", result.stderr)

    def test_a_formatting_difference_refuses_a_push_linted_through_a_drop_in(self):
        self.write("web/src/lib.rs", LIB.format(answer=15))
        self.commit("feat: answer 15 in the page")
        result = self.push(drop_in=True, unformatted=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: rustfmt failed - rerun: bash ", result.stderr)
        self.assertIn("check-changed-rustfmt.sh", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_lint_failure_refuses_the_push(self):
        self.write("web/src/lib.rs", LIB.format(answer=9))
        self.commit("feat: answer 9 in the page")
        result = self.push(status=1, wide=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: clippy (native) failed - rerun: cargo "
                      "clippy --locked -p fixture-web --all-targets --keep-going -- -D warnings",
                      result.stderr)
        self.assertIn("pre-push: clippy (wasm32) failed - rerun: cargo clippy --locked --target "
                      "wasm32-unknown-unknown -p fixture-web -- -D warnings", result.stderr)
        self.assertIn("unreachable value in a page build", result.stderr)
        # What Cargo says about the workspace itself is not the person's to fix.
        self.assertNotIn("includes semver metadata", result.stderr)
        # A wide lint's progress, a line per crate, would push the diagnostic out of the report.
        self.assertNotIn("Checking crate-", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def rebase(self, files):
        """Move main on by a commit writing `files`, and replay the last commit onto it.

        Returns the new main: what the remote holds when the push is retried.
        """
        mine = self.head()
        self.git("reset", "-q", "--hard", self.base)
        for name, text in files.items():
            self.write(name, text)
        main = self.commit("chore: someone else's change")
        self.git("cherry-pick", mine)
        return main

    def outlive_the_rebase_window(self):
        """Make every pass of pushed commits older than the window a rebased push reuses it in."""
        long_ago = time.time() - 24 * 3600
        for entry in (self.root / ".git/pre-push-lint").glob("change-*"):
            os.utime(entry, (long_ago, long_ago))

    def test_a_retry_after_rebasing_onto_unrelated_commits_lints_nothing_again(self):
        self.write("native/src/lib.rs", LIB.format(answer=50))
        self.commit("feat: answer 50")
        first = self.push()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        self.outlive_the_rebase_window()
        main = self.rebase({"web/src/lib.rs": LIB.format(answer=51), "docs/notes.md": "words\n"})
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], "nothing the lint reads changed")
        self.assertNotIn("fmt --check", self.log.read_text() if self.log.exists() else "")
        self.assertIn("clippy (native) passed before on the same inputs", retry.stderr)

    def test_a_retry_after_a_dependency_changed_lints_again(self):
        self.write("web/src/lib.rs", LIB.format(answer=52))
        self.commit("feat: answer 52 in the page")
        self.assertEqual(self.push(wide=True).returncode, 0)
        main = self.rebase({"fake-binding/src/lib.rs": "pub fn bind() {}\npub fn more() {}\n"})
        retry = self.push(remote_oid=main, wide=True)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(sorted(self.calls()), sorted([
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
            NATIVE_CLIPPY.replace("native", "web"),
        ]))
        # Formatting reads only the pushed package, which is as it was.
        self.assertNotIn("fmt --check", self.log.read_text())

    def test_a_rebase_onto_commits_that_do_not_reach_the_package_reuses_its_pass(self):
        # Main takes an app and a dependency of another member, which the pushed
        # package never reads: the push is the one that passed. Relinting it
        # here takes minutes, and main gains a commit every minute or two.
        self.write("Cargo.lock", LOCKED_SERDE)
        self.write("web/Cargo.toml", PAGE_WITH_SERDE)
        self.commit("chore: the page crate reads serde")
        self.base = self.head()
        self.write("native/src/lib.rs", LIB.format(answer=70))
        self.commit("feat: answer 70")
        first = self.push()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        self.outlive_the_rebase_window()
        main = self.rebase({
            "Cargo.toml": WORKSPACE.replace(
                'members = ["native", "web", "fake-binding"]',
                'members = ["native", "web", "fake-binding", "extra"]'),
            "Cargo.lock": LOCKED_SERDE.replace('name = "serde"\nversion = "1.0.0"',
                                               'name = "serde"\nversion = "1.0.1"'),
            "extra/Cargo.toml": CRATE.format(name="extra"),
            "extra/src/lib.rs": "pub fn extra() {}\n",
        })
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], "nothing the native crate builds changed")
        self.assertIn("passed before on the same inputs", retry.stderr)

    def test_a_retry_after_a_dependency_the_package_uses_changed_lints_again(self):
        # A resolved dependency of the pushed package moved: the code it
        # compiles against is not the code the earlier pass built.
        self.write("Cargo.lock", LOCKED_SERDE)
        self.write("web/Cargo.toml", PAGE_WITH_SERDE)
        self.commit("chore: the page crate reads serde")
        self.base = self.head()
        self.write("web/src/lib.rs", LIB.format(answer=72))
        self.commit("feat: answer 72 in the page")
        self.assertEqual(self.push(wide=True).returncode, 0)
        self.outlive_the_rebase_window()
        main = self.rebase({"Cargo.lock": LOCKED_SERDE.replace(
            'name = "serde"\nversion = "1.0.0"', 'name = "serde"\nversion = "1.0.1"')})
        retry = self.push(remote_oid=main, wide=True)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(sorted(self.calls()), sorted([
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
            NATIVE_CLIPPY.replace("native", "web"),
        ]))

    def test_a_retry_after_a_file_the_build_includes_changed_lints_again(self):
        self.write("docs/answer.txt", "42\n")
        self.commit("docs: the answer")
        self.base = self.head()
        self.write("native/src/lib.rs", 'pub const ANSWER: &str = include_str!("../../docs/answer.txt");\n')
        self.commit("feat: answer from the docs")
        self.assertEqual(self.push().returncode, 0)
        self.outlive_the_rebase_window()
        main = self.rebase({"docs/answer.txt": "43\n"})
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_commits_that_passed_are_pushed_again_unlinted_after_a_rebase_that_left_them_as_they_were(self):
        # Main gained a change to a file the lint reads, so the inputs are not the ones that
        # passed; the commits are. A relint here takes minutes, and main moves again meanwhile.
        self.write("docs/answer.txt", "42\n")
        self.commit("docs: the answer")
        self.base = self.head()
        self.write("native/src/lib.rs", 'pub const ANSWER: &str = include_str!("../../docs/answer.txt");\n')
        self.commit("feat: answer from the docs")
        self.assertEqual(self.push().returncode, 0)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        main = self.rebase({"docs/answer.txt": "43\n"})
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], retry.stderr)
        self.assertIn("passed the lint 0 min ago and are pushed as they were", retry.stderr)
        # A second rebase is the same push again, and so is every later one inside the window.
        self.base = main
        again = self.push(remote_oid=self.rebase({"docs/answer.txt": "44\n"}))
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.calls(), [])

    def test_commits_changed_since_they_passed_are_linted_again(self):
        self.write("native/src/lib.rs", LIB.format(answer=60))
        self.commit("feat: answer 60")
        self.assertEqual(self.push().returncode, 0)
        # Amended, as resolving a rebase conflict does: no longer the commit that passed.
        self.write("native/src/lib.rs", LIB.format(answer=61))
        self.git("commit", "-qa", "--amend", "--no-edit")
        retry = self.push()
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        # One more commit on top is another push too.
        self.write("native/src/more.rs", "pub fn more() {}\n")
        self.commit("feat: more")
        self.assertEqual(self.push().returncode, 0)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_commits_that_failed_are_linted_again_however_soon(self):
        self.write("native/src/lib.rs", LIB.format(answer=62))
        self.commit("feat: answer 62")
        self.assertEqual(self.push(status=1).returncode, 1)
        main = self.rebase({"docs/notes.md": "words\n"})
        retry = self.push(remote_oid=main, status=1)
        self.assertEqual(retry.returncode, 1)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_mains_own_lint_reads_every_commit_on_the_tree_it_landed_on(self):
        # The push went unlinted on the strength of an earlier pass; lint-head on main is what
        # says what main's other commits did to it, so it never takes that pass for its own.
        self.write("docs/answer.txt", "42\n")
        self.commit("docs: the answer")
        self.base = self.head()
        self.write("native/src/lib.rs", 'pub const ANSWER: &str = include_str!("../../docs/answer.txt");\n')
        self.commit("feat: answer from the docs")
        self.assertEqual(self.push().returncode, 0)
        main = self.rebase({"docs/answer.txt": "43\n"})
        wide = self.push(remote_oid=main, wide=True)
        self.assertEqual(wide.returncode, 0, wide.stderr)
        self.assertIn(NATIVE_CLIPPY, self.calls())

    def test_a_failed_lint_is_not_remembered(self):
        self.write("native/src/lib.rs", LIB.format(answer=53))
        self.commit("feat: answer 53")
        self.assertEqual(self.push(status=1).returncode, 1)
        retry = self.push(status=1)
        self.assertEqual(retry.returncode, 1)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_uncommitted_changes_are_linted_again(self):
        self.write("native/src/lib.rs", LIB.format(answer=54))
        self.commit("feat: answer 54")
        self.assertEqual(self.push().returncode, 0)
        self.write("native/src/extra.rs", "pub fn extra() {}\n")
        retry = self.push()
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_each_runs_whole_output_and_time_can_be_kept(self):
        self.write("native/src/lib.rs", LIB.format(answer=12))
        self.commit("feat: answer 12")
        kept = self.temporary / "kept"
        result = subprocess.run(
            ["python3", str(self.root / "tools/ci/pre_push_lint.py"), "--range",
             f"{self.base}..{self.head()}", "--keep-logs", str(kept)], cwd=self.root,
            capture_output=True, text=True, env=self.environment(), timeout=HANG_BOUND,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        native = (kept / "1-clippy-native.log").read_text()
        self.assertRegex(native, r"^\$ cargo clippy --locked -p fixture-native .*\n# exit 0 after \d+s\n")
        self.assertIn("unreachable value in a page build", native)

    def test_a_plan_names_the_packages_without_linting(self):
        self.write("web/src/lib.rs", LIB.format(answer=11))
        self.commit("feat: answer 11 in the page")
        result = subprocess.run(
            ["python3", str(self.root / "tools/ci/pre_push_lint.py"), "origin", "--plan", "--wide"],
            cwd=self.root,
            capture_output=True, text=True, env=self.environment(), timeout=HANG_BOUND,
            input=f"refs/heads/main {self.head()} refs/heads/main {self.base}\n",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["native: fixture-web", "wasm32: fixture-web"])


if __name__ == "__main__":
    unittest.main()
