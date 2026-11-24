import json
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
# What the hook runs, copied into a fixture workspace of its own.
HOOK_SOURCES = [
    ".githooks/pre-push",
    "tools/ci/pre_push_lint.py",
    "tools/ci/package_features.py",
    "tools/ci/package_sets.py",
    "tools/ci/wasm_clippy_conf.py",
    "tools/ci/workspace_graph.py",
    "tools/ci/suites.py",
    "tools/ci/suites.toml",
    "tools/ci/check-changed-rustfmt.sh",
    "tools/ci/check-rustfmt-pin.sh",
    "rust-toolchain.toml",
    "tools/ci/comparison_range.py",
    # The catalogue judgement the push makes over its range, and what it reads.
    "tools/policy/check_catalogue_coverage.py",
    "tools/policy/check_copy_language.py",
    "tools/policy/judged.py",
    "tools/policy/rust_source.py",
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
# t: `t cargo metadata` reads the workspace's metadata for a machine with no cargo of its own, and
# `t test` answers each check it is asked for as a streamed result line, then the whole run's
# document spread over lines, as `--json` prints them. STUB_T_OUTCOME and STUB_T_CACHED (the run a
# cached answer came from) say what it answers; with STUB_T_ERROR it cannot run them at all.
T_STUB = """#!/bin/sh
case "$1 $2" in
  "cargo metadata") printf '%s\\n' "$STUB_METADATA"; exit 0 ;;
esac
case "$1" in
  test)
    shift
    printf 'start %s t test %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
    if [ -n "$STUB_T_ERROR" ]; then
      printf '{"hint":null,"message":"%s","type":"error"}\\n' "$STUB_T_ERROR"
      exit 2
    fi
    for check in "$@"; do
      case "$check" in --*) continue ;; esac
      printf '{"seq":2,"body":{"finished":{"check":"%s","outcome":"%s","cached":%s,"log":"5eed","excerpt":"%s","duration_ms":61000}}}\\n' \\
        "$check" "${STUB_T_OUTCOME:-pass}" "${STUB_T_CACHED:-null}" "${STUB_T_EXCERPT:-}"
    done
    printf '{\\n  "results": [],\\n  "run": "r1"\\n}\\n'
    [ "${STUB_T_OUTCOME:-pass}" = pass ]; exit $? ;;
esac
exit 2
"""
# t's families as t.toml declares them: the workspace's clippy over every member but the page
# crate, which a per-architecture family lints, and the page's own wasm32 lint, which builds without
# workspace-unify's features and is no lint a push gets.
T_TOML = """[[cargo]]
name = "clippy"
tool = "clippy"
unify = "workspace-unify"
exclude = ["fixture-web", "wasm-bindgen"]

[[cargo]]
name = "clippy-{arch}"
tool = "clippy"
unify = "workspace-unify"
packages = ["fixture-web"]
matrix = { arch = ["x86_64", "aarch64"] }

[[cargo]]
name = "clippy-page"
tool = "clippy"
packages = ["fixture-web", "page-user"]

[[cargo]]
name = "test"
tool = "nextest"
unify = "workspace-unify"
"""
ARCH = {"amd64": "x86_64", "arm64": "aarch64"}.get(platform.machine().lower(), platform.machine().lower())
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
        # The rustfmt on PATH is the one the pinned toolchain installs, as check-rustfmt-pin.sh
        # requires of it.
        self.home = self.temporary / "home"
        channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
        toolchain = self.home / f".rustup/toolchains/{channel}-x86_64-unknown-linux-gnu/bin"
        toolchain.mkdir(parents=True)
        (toolchain / "rustfmt").symlink_to(self.bin / "rustfmt")
        self.stub("drop-in", DROP_IN_STUB)
        self.log = self.temporary / "hook.log"
        # Whether the fixture has a page entry that compiles the host crate, and
        # which crates build on the host one.
        self.page_entry = False
        self.dependents = []
        # Whether the fixture has the generated package that only unifies features, which a
        # member depends on without using anything of it.
        self.hack = False
        # Whether the fixture has those packages building on the host crate, as their generated
        # manifests do on every member.
        self.hack_dependants = False
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
        native = ([{"name": "workspace-hack", "kind": None,
                    "path": str(root / "workspace-hack")}] if self.hack else [])
        packages = [
            {"name": "fixture-native", "manifest_path": str(root / "native/Cargo.toml"),
             "dependencies": native},
            {"name": "fixture-web", "manifest_path": str(root / "web/Cargo.toml"),
             "dependencies": [{"name": "wasm-bindgen", "kind": None, "path": "fake-binding"}]},
            {"name": "wasm-bindgen", "manifest_path": str(root / "fake-binding/Cargo.toml"),
             "dependencies": []},
        ]
        if self.hack:
            packages.append(
                {"name": "workspace-hack", "manifest_path": str(root / "workspace-hack/Cargo.toml"),
                 "targets": [{"kind": ["lib"]}], "dependencies": []})
        if self.hack_dependants:
            # Generated manifests name every member, so the host crate is one of their
            # dependencies; a lib and a bin target so neither is skipped for its target kinds.
            for name, kinds in (("workspace-hack", ["lib"]), ("workspace-unify", ["bin"])):
                packages.append(
                    {"name": name, "manifest_path": str(root / name / "Cargo.toml"),
                     "targets": [{"kind": kinds}],
                     "dependencies": [{"name": "fixture-native", "kind": None,
                                       "path": str(root / "native")}]})
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
                    agent=False, t_outcome="pass", t_cached=None, t_excerpt="", t_error="") -> dict:
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
                                          for tool in ("cargo", "rustup", "t")))])
        env.update(HOME=str(self.home), HOOK_LOG=str(self.log), STUB_STATUS=str(status), STUB_SLEEP=str(sleep),
                   STUB_METADATA=self.metadata(), STUB_FMT_STATUS="1" if unformatted else "0",
                   STUB_TARGETS=WITH_PAGE if page else HOST_ONLY)
        if drop_in:
            env["CARGO"] = str(self.bin / "drop-in")
        env.update(STUB_T_OUTCOME=t_outcome, STUB_T_CACHED=json.dumps(t_cached),
                   STUB_T_EXCERPT=t_excerpt, STUB_T_ERROR=t_error)
        return env

    def push(self, head=None, remote_oid=None, ref="refs/heads/main", cwd=None,
             also=(), **environment):
        """Run the hook as Git would, with the ref lines it writes for a push.

        `also` names more refs the same push sends the head to.
        """
        self.log.unlink(missing_ok=True)
        remote_oid = self.base if remote_oid is None else remote_oid
        cwd = cwd or self.root
        command = [str(cwd / ".githooks/pre-push"), "origin", "file:///dev/null"]
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

    def test_a_push_whose_range_adds_an_untranslated_key_is_refused(self):
        # The commit hook judges each commit's catalogues as it is made, and a cherry-pick or a
        # rebase writes commits it never sees: the push judges the range, before any lint.
        self.write("app/locales/en-US.ftl", "a = Title\n")
        self.write("app/locales/de-DE.ftl", "a = Titel\n")
        self.base = self.commit("feat: an app's catalogue")
        self.write("app/locales/en-US.ftl", "a = Title\n\nb = Settings\n")
        self.commit("feat: a word without its German")
        result = self.push()
        self.assertEqual(result.returncode, 1)
        self.assertIn("app/locales/en-US.ftl:3: b has no de-DE text", result.stderr)
        self.assertFalse(self.log.exists(), "nothing should have been compiled")

    def test_a_catalogue_push_with_whole_translations_lints_no_rust(self):
        self.write("app/locales/en-US.ftl", "a = Title\n")
        self.write("app/locales/de-DE.ftl", "a = Titel\n")
        self.base = self.commit("feat: an app's catalogue")
        self.write("app/locales/en-US.ftl", "a = Title\n\nb = Settings\n")
        self.write("app/locales/de-DE.ftl", "a = Titel\n\nb = Einstellungen\n")
        self.commit("feat: a word in both languages")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no Rust or Cargo file changed", result.stderr)

    def test_a_key_a_later_commit_translates_is_not_drift(self):
        # The push judges the range as one change: what an early commit left behind and a
        # later one finished is whole at the head the range sends.
        self.write("app/locales/en-US.ftl", "a = Title\n")
        self.write("app/locales/de-DE.ftl", "a = Titel\n")
        self.base = self.commit("feat: an app's catalogue")
        self.write("app/locales/en-US.ftl", "a = Title\n\nb = Settings\n")
        self.commit("feat: a word without its German yet")
        self.write("app/locales/de-DE.ftl", "a = Titel\n\nb = Einstellungen\n")
        self.commit("feat: its German")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no Rust or Cargo file changed", result.stderr)

    def add_dependent(self, name, source):
        """A crate that builds on the host one, with its own source."""
        self.dependents.append(name)
        self.write(f"{name}/Cargo.toml", CRATE.format(name=name)
                   + f'\n[dependencies]\nfixture-native = {{ path = "../native" }}\n')
        self.write(f"{name}/src/lib.rs", source)
        self.commit(f"feat: {name} uses the host crate")
        self.base = self.head()

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
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        # The binding's own record is a member here, so it is linted as well.
        self.assertEqual(self.calls(),
                         [NATIVE_CLIPPY.replace("native", "web -p wasm-bindgen")])

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
        self.write("tools/workspace-unify/Cargo.toml",
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

    def test_a_push_lints_what_it_changes_and_what_builds_on_it(self):
        # A dependant constructs what the changed package declares, so its build can break
        # while the changed package compiles clean; a package that shares no path dependency
        # with it stays out.
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", "pub fn solution() -> u32 {\n    42\n}\n")
        self.commit("refactor: the answer is the solution")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY.replace(
            "-p fixture-native", "-p fixture-native -p user")])
        self.assertIn("1 of them direct dependants of what changed", result.stderr)

    def test_a_cargo_drop_in_compiles_instead_of_the_local_cargo(self):
        self.write("web/src/lib.rs", LIB.format(answer=13))
        self.commit("feat: answer 13 in the page")
        result = self.push(drop_in=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls("dropin"),
                         [NATIVE_CLIPPY.replace("native", "web")])
        self.assertEqual(self.calls(), [], "the local cargo must not compile at all")

    def with_t(self, cargo=True):
        """This machine with t installed, and t.toml's families committed; without `cargo`, as a
        build server is, with no cargo of its own."""
        if not cargo:
            (self.bin / "cargo").unlink()
        self.stub("t", T_STUB)
        self.write("t.toml", T_TOML)
        self.base = self.commit("ci: t's families")

    def test_with_t_the_lint_is_ts_checks_of_the_changed_packages(self):
        # t keys each per-package check by what that package's build reads, dependencies
        # included, and answers a key it has seen from its cache: the lint asks t, not cargo.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=20))
        self.write("web/src/lib.rs", LIB.format(answer=21))
        self.commit("feat: answers 20 and 21")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [
            f"t test clippy/fixture-native clippy-{ARCH}/fixture-web clippy-page/fixture-web --json"])
        self.assertIn("pre-push: clippy/fixture-native passed in 61s", result.stderr)
        self.assertIn("clippy and rustfmt for 2 packages", result.stderr)

    def test_without_a_cargo_t_names_the_packages_and_lints_them(self):
        self.with_t(cargo=False)
        self.write("native/src/lib.rs", LIB.format(answer=16))
        self.commit("feat: answer 16")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test clippy/fixture-native --json"])

    def test_with_t_a_dependant_is_linted_through_its_check(self):
        # t keys each per-package check by what that package's build reads, so a dependant is
        # asked for as a seed is, and answered from t's cache when nothing it reads moved.
        self.with_t()
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", LIB.format(answer=24))
        self.commit("feat: answer 24")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test clippy/fixture-native clippy/user --json"])

    def test_a_dependant_the_page_family_names_is_linted_for_the_page(self):
        # The host lint cannot see a wasm32 break, so a dependant the page family names gets
        # that family's check beside its host one.
        self.with_t()
        self.add_dependent("page-user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", LIB.format(answer=25))
        self.commit("feat: answer 25")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [
            "t test clippy/fixture-native clippy/page-user clippy-page/page-user --json"])

    def test_a_change_to_the_hack_crate_lints_no_dependants(self):
        # workspace-hack only unifies third-party features: its dependants build on it without
        # naming anything of it, so its own change stands for no rebuild of theirs.
        self.hack = True
        self.write("workspace-hack/Cargo.toml", CRATE.format(name="workspace-hack"))
        self.write("workspace-hack/src/lib.rs", "pub fn nothing() {}\n")
        self.write("native/Cargo.toml", CRATE.format(name="fixture-native")
                   + '\n[dependencies]\nworkspace-hack = { path = "../workspace-hack" }\n')
        self.commit("test: a workspace of a hack crate")
        self.base = self.head()
        self.with_t()
        self.write("workspace-hack/src/lib.rs", "pub fn nothing() {}\npub fn more() {}\n")
        self.commit("chore: the hack crate moves")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test clippy/workspace-hack --json"])

    def test_the_feature_unification_crates_are_never_dependants(self):
        # They hold no code of their own, only feature resolution: a member's change cannot
        # break them, so selecting them as dependants would lint nothing.
        self.hack_dependants = True
        for name in ("workspace-hack", "workspace-unify"):
            self.write(f"{name}/Cargo.toml", CRATE.format(name=name)
                       + '\n[dependencies]\nfixture-native = { path = "../native" }\n')
            self.write(f"{name}/src/lib.rs", "pub fn nothing() {}\n")
        self.commit("test: the unification crates build on the host crate")
        self.base = self.head()
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=27))
        self.commit("feat: answer 27")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test clippy/fixture-native --json"])

    def test_a_check_t_answered_before_is_answered_from_its_cache(self):
        # After a rebase onto commits that reach nothing the package's build reads, t's key is
        # the one it already answered: the push says so and waits on nothing.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=22))
        self.commit("feat: answer 22")
        result = self.push(t_cached="r0ld")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("pre-push: clippy/fixture-native passed on these inputs before (t's run r0ld)",
                      result.stderr)

    def test_a_failed_t_check_refuses_the_push_with_its_diagnostics(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=18))
        self.commit("feat: answer 18")
        result = self.push(t_outcome="fail", t_excerpt="error: this let-binding has unit value")
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: clippy (t) failed - rerun: t test clippy/fixture-native\n",
                      result.stderr)
        self.assertIn("clippy/fixture-native: fail - whole log: t log 5eed", result.stderr)
        self.assertIn("error: this let-binding has unit value", result.stderr)
        self.assertNotIn('"results"', result.stderr, "t's run document is no diagnostic")
        self.assertIn("refusing the push", result.stderr)

    def test_a_t_that_cannot_run_the_checks_refuses_the_push(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=19))
        self.commit("feat: answer 19")
        result = self.push(t_error="cannot reach the server")
        self.assertEqual(result.returncode, 1)
        self.assertIn("t: cannot reach the server", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_package_no_family_lints_is_named_and_its_dependant_is_linted(self):
        self.with_t()
        self.write("fake-binding/src/lib.rs", "pub fn bind() {}\npub fn more() {}\n")
        self.commit("feat: the binding binds more")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no clippy family of t's lints wasm-bindgen", result.stderr)
        self.assertEqual(self.calls(), [
            f"t test clippy-{ARCH}/fixture-web clippy-page/fixture-web --json"])

    def test_a_cargo_named_in_cargo_lints_instead_of_t(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=23))
        self.commit("feat: answer 23")
        result = self.push(drop_in=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls("dropin"), [NATIVE_CLIPPY])
        self.assertEqual(self.calls(), [], "t must not be asked")

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
        result = self.push(status=1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: clippy (native) failed - rerun: cargo "
                      "clippy --locked -p fixture-web --all-targets --keep-going -- -D warnings",
                      result.stderr)
        self.assertIn("unreachable value in a page build", result.stderr)
        # What Cargo says about the workspace itself is not the person's to fix.
        self.assertNotIn("includes semver metadata", result.stderr)
        # A lint's progress, a line per crate, would push the diagnostic out of the report.
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

    def stands_after_a_rebase(self):
        """Main moved while the lint ran: the push is replayed onto it unchanged, and main's new
        commits touch another package and the docs, not the pushed package's own directory. The
        retry is sent without linting again."""
        self.write("native/src/lib.rs", LIB.format(answer=50))
        self.commit("feat: answer 50")
        first = self.push()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertNotEqual(self.calls(), [], "the first push is linted")
        passed_at = self.head()
        main = self.rebase({"docs/notes.md": "words\n", "web/src/lib.rs": LIB.format(answer=8)})
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], retry.stderr)
        self.assertIn("pre-push: reusing the clippy pass for patch ", retry.stderr)
        self.assertIn(f"(passed at {passed_at[:12]})", retry.stderr)
        self.assertRegex(retry.stderr, r"reusing the clippy pass for patch \S+ from \d{4}-\d{2}-\d{2} ")

    def test_a_passed_lint_stands_after_a_rebase_onto_commits_that_leave_its_packages_alone(self):
        self.stands_after_a_rebase()

    def test_a_pass_of_ts_checks_stands_after_such_a_rebase_too(self):
        self.with_t()
        self.stands_after_a_rebase()

    def test_a_rebase_onto_a_change_to_the_pushed_package_reuses_the_pass(self):
        # t's clippy families lint the push with main's changes on main, so a clean
        # rebase onto commits that touch the pushed package reuses the pass.
        self.write("native/src/lib.rs", LIB.format(answer=51))
        self.commit("feat: answer 51")
        self.assertEqual(self.push().returncode, 0)
        self.assertNotEqual(self.calls(), [], "the first push is linted")
        main = self.rebase({"native/src/extra.rs": "pub fn extra() {}\n"})
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], retry.stderr)
        self.assertIn("reusing the clippy pass for patch ", retry.stderr)

    def test_a_rebase_that_changes_the_pushed_patch_lints_again(self):
        # A conflict resolved in the rebase makes the push another change than the one linted.
        self.write("native/src/lib.rs", LIB.format(answer=52))
        self.commit("feat: answer 52")
        self.assertEqual(self.push().returncode, 0)
        main = self.rebase({"docs/notes.md": "words\n"})
        self.write("native/src/lib.rs", LIB.format(answer=53))
        self.git("commit", "-qam", "feat: answer 53 instead")
        self.git("reset", "-q", "--soft", main)
        self.git("commit", "-qm", "feat: answer 53")
        retry = self.push(remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_a_failed_lint_is_not_stood_on(self):
        self.write("native/src/lib.rs", LIB.format(answer=54))
        self.commit("feat: answer 54")
        self.assertEqual(self.push(status=1).returncode, 1)
        main = self.rebase({"docs/notes.md": "words\n"})
        retry = self.push(remote_oid=main)
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
        native = (kept / "2-clippy-native.log").read_text()
        self.assertRegex(native, r"^\$ cargo clippy --locked -p fixture-native .*\n# exit 0 after \d+s\n")
        self.assertIn("unreachable value in a page build", native)

    def test_a_plan_names_the_packages_without_linting(self):
        self.write("web/src/lib.rs", LIB.format(answer=11))
        self.commit("feat: answer 11 in the page")
        result = subprocess.run(
            ["python3", str(self.root / "tools/ci/pre_push_lint.py"), "origin", "--plan"],
            cwd=self.root,
            capture_output=True, text=True, env=self.environment(), timeout=HANG_BOUND,
            input=f"refs/heads/main {self.head()} refs/heads/main {self.base}\n",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "native: fixture-web",
            f"catalogue: --range {self.base[:12]}..{self.head()[:12]}",
        ])

    def test_a_plan_names_the_dependants_too(self):
        # A dependant is part of what the push lints though no commit of it changed, and a plan
        # says which packages are there for that.
        self.with_t()
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", LIB.format(answer=26))
        self.commit("feat: answer 26")
        result = subprocess.run(
            ["python3", str(self.root / "tools/ci/pre_push_lint.py"), "origin", "--plan"],
            cwd=self.root,
            capture_output=True, text=True, env=self.environment(), timeout=HANG_BOUND,
            input=f"refs/heads/main {self.head()} refs/heads/main {self.base}\n",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "native: fixture-native user",
            "dependants: user",
            "t: clippy/fixture-native clippy/user",
            f"catalogue: --range {self.base[:12]}..{self.head()[:12]}",
        ])


def pre_push_lint():
    """The check as a module, for the rules a push asks of a path without a repository."""
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        "pre_push_lint", ROOT / "tools/ci/pre_push_lint.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CatalogueGateTests(unittest.TestCase):
    """A push that reaches what the component catalogue is made from names t's ui-graph."""

    def names(self, *files: str) -> list[str]:
        module = pre_push_lint()
        return module.catalogue_checks(module.Push(set(files), [], [], []))

    def test_a_widget_change_names_the_catalogue_check(self):
        self.assertEqual(self.names("crates/ui/ui-core/src/widget/button.rs"), ["ui-graph"])

    def test_a_rich_edit_change_names_it_too(self):
        self.assertEqual(self.names("crates/ui/tilo-rich-edit/src/editor.rs"), ["ui-graph"])

    def test_the_windows_registry_names_it_too(self):
        self.assertEqual(
            self.names("crates/sdk/tilo-app-window/src/components.rs"), ["ui-graph"])

    def test_an_edit_of_the_catalogue_names_it(self):
        self.assertEqual(self.names("crates/ui/ui-graph/components.json"), ["ui-graph"])

    def test_a_change_elsewhere_names_nothing(self):
        self.assertEqual(self.names("crates/core/tilo-server/src/web/tickets.rs"), [])

    def test_the_check_runs_in_the_same_t_batch_as_the_clippy_checks(self):
        module = pre_push_lint()
        selection = module.Selection(["fixture-native"], {})
        runs = module.lint_runs(Path("/"), selection, [], ["clippy/fixture-native", "ui-graph"])
        batches = [run.command for run in runs if run.t]
        self.assertEqual(batches, [
            [module.T_CARGO[0], "test", "clippy/fixture-native", "ui-graph", "--json"]])


if __name__ == "__main__":
    unittest.main()
