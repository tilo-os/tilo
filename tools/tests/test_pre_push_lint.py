import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
# What the hook runs, copied into a fixture workspace of its own.
HOOK_SOURCES = [
    ".githooks/pre-push",
    "tools/ci/pre_push_lint.py",
    "tools/ci/package_sets.py",
    "tools/ci/workspace_graph.py",
]
WORKSPACE = """[workspace]
members = ["native", "web", "fake-binding"]
resolver = "2"
"""
CRATE = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
"""
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
  fmt) printf 'fmt %s\\n' "$*" >> "$HOOK_LOG"; exit 0 ;;
esac
printf 'start %s %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
sleep "${STUB_SLEEP:-0}"
printf 'warning: a/Cargo.toml: version requirement `1.0+meta` includes semver metadata\\n'
printf 'stub says: unreachable value in a page build\\n'
printf 'end %s %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
exit "${STUB_STATUS:-0}"
"""
# The same, named by $CARGO: a drop-in that may build somewhere else.
DROP_IN_STUB = (CARGO_STUB.replace("printf 'start ", "printf 'dropin ")
                .replace("printf 'end ", "printf 'dropin-end "))
# The toolchain's installed targets, which say whether the page build can be linted here.
RUSTUP_STUB = """#!/bin/sh
case "$*" in
  "target list --installed") printf '%s\\n' "$STUB_TARGETS" ;;
esac
exit 0
"""
HOST_ONLY = "x86_64-unknown-linux-gnu"
WITH_PAGE = HOST_ONLY + "\nwasm32-unknown-unknown"
MISSING_REF = "0" * 40
NATIVE_CLIPPY = "clippy --locked -p fixture-native --all-targets -- -D warnings"


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
        self.stub("drop-in", DROP_IN_STUB)
        self.log = self.temporary / "hook.log"
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
        """The Cargo metadata of the fixture: what the hook selects packages with."""
        packages = [
            {"name": "fixture-native", "manifest_path": str(self.root / "native/Cargo.toml"),
             "dependencies": []},
            {"name": "fixture-web", "manifest_path": str(self.root / "web/Cargo.toml"),
             "dependencies": [{"name": "wasm-bindgen", "kind": None, "path": "fake-binding"}]},
            {"name": "wasm-bindgen", "manifest_path": str(self.root / "fake-binding/Cargo.toml"),
             "dependencies": []},
        ]
        return json.dumps({"packages": packages})

    def environment(self, page=True, status=0, sleep=0, drop_in=False) -> dict:
        """The hook's environment: only the stubs, never this machine's toolchain."""
        env = dict(os.environ)
        env.pop("CARGO", None)
        env["PATH"] = os.pathsep.join(
            [str(self.bin), *(entry for entry in env["PATH"].split(os.pathsep)
                               if not any(os.access(Path(entry) / tool, os.X_OK)
                                          for tool in ("cargo", "rustup")))])
        env.update(HOOK_LOG=str(self.log), STUB_STATUS=str(status), STUB_SLEEP=str(sleep),
                   STUB_METADATA=self.metadata(),
                   STUB_TARGETS=WITH_PAGE if page else HOST_ONLY)
        if drop_in:
            env["CARGO"] = str(self.bin / "drop-in")
        return env

    def push(self, head=None, remote_oid=None, **environment):
        """Run the hook as Git would, with the ref lines it writes for a push."""
        self.log.unlink(missing_ok=True)
        remote_oid = self.base if remote_oid is None else remote_oid
        return subprocess.run(
            [str(self.root / ".githooks/pre-push"), "origin", "file:///dev/null"], cwd=self.root,
            capture_output=True, text=True, env=self.environment(**environment), timeout=HANG_BOUND,
            input=f"refs/heads/main {head or self.head()} refs/heads/main {remote_oid}\n",
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

    def test_a_host_crate_change_lints_the_host_only(self):
        self.write("native/src/lib.rs", LIB.format(answer=43))
        self.commit("feat: answer 43")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        self.assertIn("lint passed", result.stderr)

    def test_a_page_crate_change_lints_the_page_at_the_same_time(self):
        self.write("web/src/lib.rs", LIB.format(answer=8))
        self.commit("feat: answer 8 in the page")
        result = self.push(sleep=1)
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

    def test_a_page_build_is_skipped_without_its_target(self):
        self.write("web/src/lib.rs", LIB.format(answer=12))
        self.commit("feat: answer 12 in the page")
        result = self.push(page=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY.replace("native", "web")])
        self.assertIn("wasm32-unknown-unknown is not installed here", result.stderr)

    def test_a_cargo_drop_in_compiles_instead_of_the_local_cargo(self):
        self.write("web/src/lib.rs", LIB.format(answer=13))
        self.commit("feat: answer 13 in the page")
        result = self.push(drop_in=True, page=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(self.calls("dropin")), sorted([
            "clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
            NATIVE_CLIPPY.replace("native", "web"),
        ]))
        self.assertEqual(self.calls(), [], "the local cargo must not compile at all")

    def test_a_lint_failure_refuses_the_push(self):
        self.write("web/src/lib.rs", LIB.format(answer=9))
        self.commit("feat: answer 9 in the page")
        result = self.push(status=1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: clippy (native) failed - rerun: cargo "
                      "clippy --locked -p fixture-web --all-targets -- -D warnings", result.stderr)
        self.assertIn("pre-push: clippy (wasm32) failed - rerun: cargo clippy --locked --target "
                      "wasm32-unknown-unknown -p fixture-web -- -D warnings", result.stderr)
        self.assertIn("unreachable value in a page build", result.stderr)
        # What Cargo says about the workspace itself is not the person's to fix.
        self.assertNotIn("includes semver metadata", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_new_branch_is_compared_with_the_main_it_forked_from(self):
        self.write("web/src/lib.rs", LIB.format(answer=10))
        self.commit("feat: answer 10 in the page")
        self.git("update-ref", "refs/remotes/origin/main", self.base)
        result = self.push(remote_oid=MISSING_REF)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("clippy --locked --target wasm32-unknown-unknown -p fixture-web -- -D warnings",
                      self.calls())

    def test_a_plan_names_the_packages_without_linting(self):
        self.write("web/src/lib.rs", LIB.format(answer=11))
        self.commit("feat: answer 11 in the page")
        result = subprocess.run(
            ["python3", str(self.root / "tools/ci/pre_push_lint.py"), "origin", "--plan"], cwd=self.root,
            capture_output=True, text=True, env=self.environment(), timeout=HANG_BOUND,
            input=f"refs/heads/main {self.head()} refs/heads/main {self.base}\n",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["native: fixture-web", "wasm32: fixture-web"])


if __name__ == "__main__":
    unittest.main()
