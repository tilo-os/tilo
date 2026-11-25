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
    "tools/policy/derived_catalogue.py",
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
# t: `t cargo metadata` reads the workspace's metadata for a machine with no cargo of its own, and
# `t test` answers each check it is asked for as a streamed result line, then the whole run's
# document spread over lines, as `--json` prints them. STUB_T_OUTCOME and STUB_T_CACHED (the run a
# cached answer came from) say what it answers; with STUB_T_ERROR it cannot run them at all.
# STUB_T_INHERITED, STUB_T_UNKNOWN and STUB_T_NEW name checks whose result carries that blame;
# STUB_T_EXIT3 makes a failing run exit 3, as t does when every failure is not the change's.
T_STUB = """#!/bin/sh
case "$1 $2" in
  "cargo metadata") printf '%s\\n' "$STUB_METADATA"; exit 0 ;;
esac
case "$1" in
  scope)
    [ "${STUB_T_SCOPE_STATUS:-0}" = 0 ] || exit "$STUB_T_SCOPE_STATUS"
    printf '%s\\n' "${STUB_T_SCOPE:-}"; exit 0 ;;
  main)
    [ "${STUB_BOARD_STATUS:-0}" = 0 ] || exit "$STUB_BOARD_STATUS"
    if [ -n "${STUB_BOARD:-}" ]; then printf '%s\\n' "$STUB_BOARD"; else printf '{"items":[]}\\n'; fi
    exit 0 ;;
  log)
    [ "${STUB_LOG_STATUS:-0}" = 0 ] || exit "$STUB_LOG_STATUS"
    printf '%s\\n' "${STUB_LOG:-}"
    exit 0 ;;
  cargo)
    shift
    printf 'start %s t cargo %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
    [ -n "${STUB_T_EXCERPT:-}" ] && printf '%s\\n' "$STUB_T_EXCERPT" >&2
    [ "${STUB_T_OUTCOME:-pass}" = pass ]; exit $? ;;
  test)
    shift
    printf 'start %s t test %s\\n' "$(date +%s%N)" "$*" >> "$HOOK_LOG"
    if [ -n "$STUB_T_ERROR" ]; then
      printf '{"hint":null,"message":"%s","type":"error"}\\n' "$STUB_T_ERROR"
      exit 2
    fi
    for check in "$@"; do
      case "$check" in --*) continue ;; esac
      blame=""
      for named in ${STUB_T_INHERITED:-}; do
        [ "$named" = "$check" ] && blame='"blame":{"inherited":{"since":"abcdef0123456789","claimed_by":"someone"}},'
      done
      for named in ${STUB_T_UNKNOWN:-}; do
        [ "$named" = "$check" ] && blame='"blame":"unknown",'
      done
      for named in ${STUB_T_NEW:-}; do
        [ "$named" = "$check" ] && blame='"blame":"new",'
      done
      printf '{"seq":2,"body":{"finished":{%s"check":"%s","outcome":"%s","cached":%s,"log":"5eed","excerpt":"%s","findings":%s,"duration_ms":61000}}}\\n' \\
        "$blame" "$check" "${STUB_T_OUTCOME:-pass}" "${STUB_T_CACHED:-null}" "${STUB_T_EXCERPT:-}" "${STUB_T_FINDINGS:-[]}"
    done
    printf '{\\n  "results": [],\\n  "run": "r1"\\n}\\n'
    [ "${STUB_T_OUTCOME:-pass}" = pass ] && exit 0
    [ "${STUB_T_EXIT3:-}" = 1 ] && exit 3
    exit 1 ;;
esac
exit 2
"""
# t's families as t.toml declares them: the workspace's clippy over every member but the page
# crate, which a per-architecture family lints, the page's own wasm32 lint, which builds without
# workspace-unify's features and is no lint a push gets, and the compile-only families over the
# same members, which a push never asks: its dependants compile in one `t cargo check`.
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
name = "check"
tool = "check"
unify = "workspace-unify"
exclude = ["fixture-web", "wasm-bindgen"]

[[cargo]]
name = "test"
tool = "nextest"
unify = "workspace-unify"

# The gate over the generated fleet, which declares the fix that writes it back.
[[check]]
name = "app-surfaces"
run = "fixture"
fix = "fixture"
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
# A dependant's compile: no `-D warnings`, as its lints are not the push's.
USER_CHECK = "check --locked -p user --all-targets --keep-going"


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
                    agent=False, t_outcome="pass", t_cached=None, t_excerpt="", t_error="",
                    t_scope=None, t_scope_status=2, t_inherited="", t_unknown="", t_new="",
                    t_exit3=False, t_findings=None, t_board=None, t_board_status=0,
                    t_log="", t_log_status=0) -> dict:
        """The hook's environment: only the stubs, never this machine's toolchain.

        `agent` pushes as a coding agent does, whose shell says so in its environment.
        `t_scope` is what `t scope --names` answers, and `t_scope_status` whether it can say
        (2: it cannot, which a machine without its scope cache is). `t_inherited`, `t_unknown`
        and `t_new` are checks whose t result carries that blame, `t_findings` its `findings`,
        and `t_exit3` makes a failing t run exit 3, as t does when every failure is main's.
        `t_board` is what `t main --json` answers and `t_log` what `t log --verbose` does.
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
                   STUB_T_EXCERPT=t_excerpt, STUB_T_ERROR=t_error,
                   STUB_T_SCOPE=t_scope or "", STUB_T_SCOPE_STATUS=str(t_scope_status),
                   STUB_T_INHERITED=t_inherited, STUB_T_UNKNOWN=t_unknown, STUB_T_NEW=t_new,
                   STUB_T_EXIT3="1" if t_exit3 else "",
                   STUB_T_FINDINGS=json.dumps(t_findings or []),
                   STUB_BOARD=t_board or "", STUB_BOARD_STATUS=str(t_board_status),
                   STUB_LOG=t_log, STUB_LOG_STATUS=str(t_log_status))
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

    def test_a_push_to_another_branch_lints_what_it_adds_to_the_branch(self):
        # The branch already holds the web change: only what this push adds to it is linted.
        self.write("web/src/lib.rs", LIB.format(answer=70))
        branch = self.commit("feat: answer 70 in the page")
        self.write("native/src/lib.rs", LIB.format(answer=62))
        self.commit("feat: answer 62")
        result = self.push(ref="refs/heads/topic", remote_oid=branch)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"linting topic against topic's {branch[:12]}", result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_a_branch_push_that_adds_no_rust_says_what_it_compared(self):
        self.write("native/src/lib.rs", LIB.format(answer=71))
        branch = self.commit("feat: answer 71")
        self.write("docs/notes.md", "words\n")
        self.commit("docs: describe the fixture")
        result = self.push(ref="refs/heads/topic", remote_oid=branch)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"against topic's {branch[:12]}", result.stderr)
        self.assertIn("no Rust or Cargo file changed", result.stderr)
        self.assertFalse(self.log.exists(), "nothing should have been compiled")

    def test_a_new_branch_is_linted_from_where_it_forked_from_main(self):
        # Main moved on past the fork with a change of its own; the branch adds the native change.
        self.write("web/src/lib.rs", LIB.format(answer=72))
        main = self.commit("feat: main's answer 72 in the page")
        self.git("update-ref", "refs/remotes/origin/main", main)
        self.git("checkout", "-q", "-b", "topic", self.base)
        self.write("native/src/lib.rs", LIB.format(answer=63))
        self.commit("feat: answer 63")
        result = self.push(ref="refs/heads/topic", remote_oid="0" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"linting topic against its merge-base with main {self.base[:12]}",
                      result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_a_new_branch_without_the_remotes_main_lints_nothing_and_says_why(self):
        self.write("native/src/lib.rs", LIB.format(answer=73))
        self.commit("feat: answer 73")
        result = self.push(ref="refs/heads/topic", remote_oid="0" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("there is no refs/remotes/origin/main", result.stderr)
        self.assertIn("no pushed ref has a range to lint", result.stderr)
        self.assertNotIn("no Rust or Cargo file changed", result.stderr)
        self.assertFalse(self.log.exists(), "nothing should have been compiled")

    def test_a_force_pushed_branch_is_linted_from_where_old_and_new_forked(self):
        # The old head's web change is replaced; the new head adds the native change. A branch
        # may be force-pushed, so this is no refusal, as it would be for main.
        self.write("docs/notes.md", "words\n")
        fork = self.commit("docs: where the two heads fork")
        self.write("web/src/lib.rs", LIB.format(answer=74))
        old = self.commit("feat: answer 74 in the page")
        self.git("reset", "-q", "--hard", fork)
        self.write("native/src/lib.rs", LIB.format(answer=65))
        self.commit("feat: answer 65")
        result = self.push(ref="refs/heads/topic", remote_oid=old)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"topic is force-pushed; linting it against its merge-base with "
                      f"topic's {old[:12]}: {fork[:12]}", result.stderr)
        self.assertNotIn("does not contain", result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])

    def test_deleting_a_branch_lints_nothing(self):
        self.write("native/src/lib.rs", LIB.format(answer=66))
        self.commit("feat: answer 66")
        result = self.push(head="0" * 40, ref="refs/heads/topic")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("topic is deleted", result.stderr)
        self.assertNotIn("no Rust or Cargo file changed", result.stderr)
        self.assertFalse(self.log.exists(), "deleting a branch compiled something")

    def test_a_branch_pass_is_reused_after_a_rebase_too(self):
        # The pass is remembered by the push's patch, whichever ref it went to: the same change
        # replayed onto main's newer head and pushed onto a branch that holds that head reuses it.
        self.write("native/src/lib.rs", LIB.format(answer=67))
        self.commit("feat: answer 67")
        first = self.push(ref="refs/heads/topic")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertNotEqual(self.calls(), [], "the first push is linted")
        main = self.rebase({"docs/notes.md": "words\n"})
        retry = self.push(ref="refs/heads/topic", remote_oid=main)
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(self.calls(), [], retry.stderr)
        self.assertIn("reusing the clippy pass for patch ", retry.stderr)

    def test_a_push_to_main_says_nothing_of_branches(self):
        self.write("native/src/lib.rs", LIB.format(answer=68))
        self.commit("feat: answer 68")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [NATIVE_CLIPPY])
        self.assertNotIn("linting main against", result.stderr)
        self.assertNotIn("merge-base", result.stderr)

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

    def test_a_push_lints_what_it_changes_and_compiles_what_builds_on_it(self):
        # A dependant constructs what the changed package declares, so its build can break
        # while the changed package compiles clean: it is compiled, and only an error there
        # refuses the push. Its lints are not the push's, so it is never linted; a package that
        # shares no path dependency with the changed one stays out.
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", "pub fn solution() -> u32 {\n    42\n}\n")
        self.commit("refactor: the answer is the solution")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertCountEqual(self.calls(), [NATIVE_CLIPPY, USER_CHECK])
        self.assertIn("linting 1 package and compiling 1 direct dependant(s) of what changed",
                      result.stderr)

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

    def test_a_manifest_change_runs_the_page_hack_policy_step(self):
        # An ungated workspace-hack leaves the host lint green and fails every page clippy job
        # on main; the policy step names the cause, and the push runs it for a manifest.
        self.with_t()
        self.write("native/Cargo.toml", CRATE.format(name="fixture-native")
                   + '\n[dependencies]\nserde = "1"\n')
        self.commit("feat: the host crate names a dependency")
        result = self.push(t_scope="policy/wasm-workspace-hack", t_scope_status=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("policy/wasm-workspace-hack", self.calls()[0].split())

    def test_a_rust_change_does_not_run_the_page_hack_policy_step(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=31))
        self.commit("feat: answer 31")
        result = self.push(t_scope="policy/wasm-workspace-hack", t_scope_status=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("policy/wasm-workspace-hack", self.calls()[0].split())

    def test_a_manifest_change_does_not_ask_for_a_policy_step_the_tree_lacks(self):
        self.with_t()
        self.write("native/Cargo.toml", CRATE.format(name="fixture-native")
                   + '\n[dependencies]\nserde = "1"\n')
        self.commit("feat: the host crate names a dependency")
        result = self.push(t_scope="policy/source-content", t_scope_status=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("policy/wasm-workspace-hack", self.calls()[0].split())

    def test_a_push_of_an_app_surface_alone_runs_the_freshness_checks(self):
        # No crate changes, so the lint would otherwise say there is nothing to lint, and the
        # generated fleet lands stale for the next person's push to refuse.
        self.with_t()
        self.write("apps/tl/fixture/app.ids", 'id "so.tilo.fixture"\n')
        self.commit("feat: an app's surface file")
        result = self.push(t_scope="app-surfaces sdk-policy sdk-types", t_scope_status=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test app-surfaces sdk-policy sdk-types --json"])

    def test_a_crate_change_does_not_run_the_fleet_gate(self):
        # The fleet gate builds a compiler and an app collection; a crate change moves the SDK's
        # types without it, though t's read sets name both for any change.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=33))
        self.commit("feat: answer 33")
        result = self.push(t_scope="app-surfaces sdk-types", t_scope_status=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        checks = self.calls()[0].split()
        self.assertIn("sdk-types", checks)
        self.assertNotIn("app-surfaces", checks)

    def test_a_stale_fleet_refuses_the_push_and_names_its_fix(self):
        self.with_t()
        self.write("apps/tl/fixture/app.ids", 'id "so.tilo.fixture"\n')
        self.commit("feat: an app's surface file")
        result = self.push(t_scope="app-surfaces sdk-policy sdk-types", t_scope_status=0,
                           t_outcome="fail",
                           t_excerpt="crates/desktop/app-fleet/src/generated.rs is stale")
        self.assertEqual(result.returncode, 1)
        self.assertIn("app-surfaces: fail", result.stderr)
        self.assertIn("write the generated files back with: t fix app-surfaces", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_without_a_cargo_t_names_the_packages_and_lints_them(self):
        self.with_t(cargo=False)
        self.write("native/src/lib.rs", LIB.format(answer=16))
        self.commit("feat: answer 16")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["t test clippy/fixture-native --json"])

    def test_with_t_the_dependants_compile_in_one_cargo_check(self):
        # One build of every dependant shares the crates they build on; a check of each was a
        # job of its own building them again. Only a compile error there refuses.
        self.with_t()
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.add_dependent("other", "pub fn uses() -> u32 { fixture_native::answer() + 1 }\n")
        self.write("native/src/lib.rs", LIB.format(answer=24))
        self.commit("feat: answer 24")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(self.calls()), [
            "t cargo check --locked -p other -p user --all-targets --keep-going",
            "t test clippy/fixture-native --json"])

    def test_a_dependant_that_no_longer_compiles_refuses_the_push(self):
        self.with_t()
        self.add_dependent("user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", "pub fn solution() -> u32 {\n    42\n}\n")
        self.commit("refactor: the answer is the solution")
        result = self.push(t_outcome="fail", t_excerpt="error[E0425]: cannot find function `answer`")
        self.assertEqual(result.returncode, 1)
        self.assertIn("pre-push: dependants' compile (t) failed", result.stderr)
        self.assertIn("cannot find function `answer`", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_dependant_the_page_family_names_is_compiled_without_the_page_lint(self):
        # The page family lints; a dependant is only compiled, so it gets no page check. Its
        # wasm32 build is t's page family's on main.
        self.with_t()
        self.add_dependent("page-user", "pub fn uses() -> u32 { fixture_native::answer() }\n")
        self.write("native/src/lib.rs", LIB.format(answer=25))
        self.commit("feat: answer 25")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(self.calls()), [
            "t cargo check --locked -p page-user --all-targets --keep-going",
            "t test clippy/fixture-native --json"])

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

    def test_a_red_main_already_has_passes_and_is_named(self):
        # Main is red on this crate for the same finding, so t blames it inherited and exits 3:
        # the lane's push passes, and the line names the red and its owner.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=41))
        self.commit("feat: answer 41")
        result = self.push(t_outcome="fail", t_inherited="clippy/fixture-native", t_exit3=True,
                           t_excerpt="warning: unused import: `std::fmt`")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("pre-push: pushing past 1 red main already has:", result.stderr)
        self.assertIn("clippy/fixture-native: main has failed it since abcdef012345, "
                      "claimed by someone", result.stderr)
        self.assertIn("lint passed", result.stderr)

    def test_a_red_the_push_introduces_is_refused_and_named(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=52))
        self.commit("feat: answer 52")
        result = self.push(t_outcome="fail", t_new="clippy/fixture-native",
                           t_excerpt="error: unused import: `std::fmt`")
        self.assertEqual(result.returncode, 1)
        self.assertIn("clippy/fixture-native: fail - whole log: t log 5eed", result.stderr)
        self.assertIn("new: passes on your base", result.stderr)
        self.assertIn("unused import", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_new_lint_in_a_check_main_is_red_on_is_refused(self):
        # t compares findings one by one: main's red for another finding does not hide this one,
        # so a check red on main is still the push's when it names a finding main does not have.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=43))
        self.commit("feat: answer 43")
        result = self.push(t_outcome="fail", t_new="clippy/fixture-native",
                           t_excerpt="error: this variable does not need to be mutable")
        self.assertEqual(result.returncode, 1)
        self.assertIn("new: passes on your base", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_board_with_no_verdict_at_the_base_is_refused_with_why(self):
        # t could not compare the failure with the base, and the board cannot be read either:
        # fail closed, and say both.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=44))
        self.commit("feat: answer 44")
        result = self.push(t_outcome="fail", t_unknown="clippy/fixture-native",
                           t_excerpt="error: unused import", t_board_status=1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("may be yours: main has no result at your base", result.stderr)
        self.assertIn("main's board cannot be read, so whose failure this is cannot be told",
                      result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def board(self, since, check="clippy/fixture-native", test=None, log="deadbeef",
              owner=None) -> str:
        """Main's board naming `check` red since `since`, as `t main --json` prints it."""
        claim = {"session": owner, "note": None} if owner else None
        return json.dumps({"head": since, "items": [
            {"check": check, "test": test, "first_bad": since, "log": log, "claim": claim,
             "culprit": since, "family": None, "id": f"{check}@{since[:12]}",
             "last_good": None, "qualified": False}]})

    def test_a_board_red_at_the_base_passes_and_is_named(self):
        # t has no stored verdict for the check at the base, but main's board says it was red
        # there: the push passes, naming the red and who claimed it.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=45))
        self.commit("feat: answer 45")
        result = self.push(t_outcome="fail", t_unknown="clippy/fixture-native",
                           t_excerpt="error: unused import",
                           t_board=self.board(self.base, owner="someone"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("pre-push: pushing past 1 red main already has:", result.stderr)
        self.assertIn(f"clippy/fixture-native: main has failed it since "
                      f"{self.base[:12]}, claimed by someone", result.stderr)

    def test_a_new_finding_in_a_check_main_is_red_on_is_refused(self):
        # Main's red names another finding; the one this push adds is not in main's own log, so
        # the check is the push's despite being red on main, and the finding is named.
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=46))
        self.commit("feat: answer 46")
        main_log = json.dumps({"reason": "compiler-message", "message": {
            "code": {"code": "dead_code"},
            "spans": [{"is_primary": True, "file_name": "native/src/other.rs"}]}})
        result = self.push(t_outcome="fail", t_unknown="clippy/fixture-native",
                           t_findings=["unused_imports native/src/lib.rs"],
                           t_excerpt="error: unused import",
                           t_board=self.board(self.base), t_log=main_log)
        self.assertEqual(result.returncode, 1)
        self.assertIn("new: unused_imports native/src/lib.rs", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_finding_main_also_has_passes(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=47))
        self.commit("feat: answer 47")
        main_log = json.dumps({"reason": "compiler-message", "message": {
            "code": {"code": "unused_imports"},
            "spans": [{"is_primary": True, "file_name": "native/src/lib.rs"}]}})
        result = self.push(t_outcome="fail", t_unknown="clippy/fixture-native",
                           t_findings=["unused_imports native/src/lib.rs"],
                           t_excerpt="error: unused import",
                           t_board=self.board(self.base), t_log=main_log)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("pushing past 1 red main already has:", result.stderr)

    def test_a_t_that_cannot_run_the_checks_refuses_the_push(self):
        self.with_t()
        self.write("native/src/lib.rs", LIB.format(answer=19))
        self.commit("feat: answer 19")
        result = self.push(t_error="cannot reach the server")
        self.assertEqual(result.returncode, 1)
        self.assertIn("t: cannot reach the server", result.stderr)
        self.assertIn("refusing the push", result.stderr)

    def test_a_package_no_family_lints_is_named_and_its_dependant_is_compiled(self):
        self.with_t()
        self.write("fake-binding/src/lib.rs", "pub fn bind() {}\npub fn more() {}\n")
        self.commit("feat: the binding binds more")
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no clippy family of t's lints wasm-bindgen", result.stderr)
        self.assertEqual(self.calls(), ["t cargo check --locked -p fixture-web --all-targets --keep-going"])

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
            "t: clippy/fixture-native",
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


class BuilderLicenseGateTests(unittest.TestCase):
    """A push whose manifests put AGPL code under a published builder crate is refused."""

    def metadata(self, leaf_license: str) -> dict:
        policy = tomllib.loads((ROOT / "tools/sdk/license-policy.toml").read_text())
        def package(name, license, deps=()):
            return {"name": name, "source": None, "license": license,
                    "description": name, "repository": "https://code.tilo.so/tilo/tilo",
                    "manifest_path": str(ROOT / "Cargo.toml"),
                    "readme": "docs/crates/supporting-crate.md", "dependencies": [
                {"name": dep, "kind": None, "req": "^0.1.0", "path": f"/x/{dep}"} for dep in deps]}
        roots = [package(name, "MIT") for name in policy["builder_roots"]]
        roots[0]["dependencies"].append(
            {"name": "tilo-leaf", "kind": None, "req": "^0.1.0", "path": "/x/tilo-leaf", "optional": True})
        return {"packages": [*roots, package("tilo-leaf", leaf_license)]}

    def test_an_agpl_crate_under_a_builder_crate_is_found_and_an_mit_one_is_not(self):
        module = pre_push_lint()
        found = module.builder_license_findings(ROOT, self.metadata("AGPL-3.0-only"))
        self.assertEqual(len(found), 1)
        self.assertIn("tilo-leaf", found[0])
        self.assertEqual(module.builder_license_findings(ROOT, self.metadata("MIT")), [])

    def test_an_unpublishable_crate_under_a_builder_crate_is_found(self):
        module = pre_push_lint()
        metadata = self.metadata("MIT")
        metadata["packages"][-1]["publish"] = []
        found = module.builder_license_findings(ROOT, metadata)
        self.assertEqual(len(found), 1)
        self.assertIn("tilo-leaf", found[0])
        self.assertIn("publish = false", found[0])

    def test_a_push_that_changes_no_manifest_is_not_asked(self):
        module = pre_push_lint()
        self.assertEqual(module.builder_licenses_range(ROOT, module.Push({"crates/x/src/lib.rs"}, [], [], [])), 0)


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


class PolicyGateTests(unittest.TestCase):
    """A push that changes a manifest runs t's policy step for the page's wasm32 build."""

    def names(self, named, *files: str, root: Path = ROOT) -> list[str]:
        module = pre_push_lint()
        return module.policy_checks(root, module.Push(set(files), [], [], []), named)

    def test_a_manifest_change_names_the_page_hack_step(self):
        self.assertEqual(self.names({"policy/wasm-workspace-hack"}, "crates/x/Cargo.toml"),
                         ["policy/wasm-workspace-hack"])

    def test_a_lock_change_names_it_too(self):
        self.assertEqual(self.names({"policy/wasm-workspace-hack"}, "Cargo.lock"),
                         ["policy/wasm-workspace-hack"])

    def test_a_rust_change_names_nothing(self):
        # t's read sets name it for any change (a job lists the workspace's directories), so
        # the lint gates it on the manifest itself: no other push pays for it.
        self.assertEqual(self.names({"policy/wasm-workspace-hack"}, "crates/x/src/lib.rs"), [])

    def test_a_step_the_tree_lacks_is_not_asked_for(self):
        self.assertEqual(self.names({"policy/source-content"}, "crates/x/Cargo.toml"), [])

    def test_without_ts_answer_a_manifest_change_still_names_it(self):
        self.assertEqual(self.names(None, "crates/x/Cargo.toml"), ["policy/wasm-workspace-hack"])
        self.assertEqual(self.names(None, "crates/x/src/lib.rs"), [])

    def test_a_tree_that_does_not_declare_the_step_is_not_asked(self):
        with tempfile.TemporaryDirectory() as empty:
            self.assertEqual(self.names(None, "Cargo.toml", root=Path(empty)), [])

    def test_the_step_runs_in_the_same_t_batch_as_the_clippy_checks(self):
        module = pre_push_lint()
        selection = module.Selection(["fixture-native"], {})
        runs = module.lint_runs(Path("/"), selection, [], [
            "clippy/fixture-native", "policy/wasm-workspace-hack"])
        self.assertEqual([run.command for run in runs if run.t], [
            [module.T_CARGO[0], "test", "clippy/fixture-native",
             "policy/wasm-workspace-hack", "--json"]])


class SurfaceGateTests(unittest.TestCase):
    """A push that changes an app's published surface names t's generated-file checks."""

    def names(self, named, *files: str) -> list[str]:
        module = pre_push_lint()
        return module.surface_checks(module.Push(set(files), [], [], []), named)

    def test_an_app_source_names_the_fleet_and_sdk_checks(self):
        self.assertEqual(self.names({"app-surfaces", "sdk-policy", "sdk-types"},
                                    "apps/tl/shell/asking.tl"),
                         ["app-surfaces", "sdk-policy", "sdk-types"])

    def test_a_tilolang_source_under_a_crate_names_them_too(self):
        self.assertEqual(self.names({"app-surfaces", "sdk-policy", "sdk-types"},
                                    "crates/lang/tilolang/std/std.tl"),
                         ["app-surfaces", "sdk-policy", "sdk-types"])

    def test_a_surface_file_names_them(self):
        self.assertEqual(self.names({"app-surfaces"}, "apps/discord/surface.json"),
                         ["app-surfaces"])

    def test_a_crate_change_outside_a_surface_does_not_name_the_fleet_gate(self):
        # sdk-types follows every crate's actions and records; the fleet gate builds a compiler
        # and an app collection, so only an app's published surface asks for it.
        self.assertEqual(self.names({"app-surfaces", "sdk-types"},
                                    "crates/core/tilo-actions/src/lib.rs"),
                         ["sdk-types"])

    def test_without_ts_answer_a_surface_change_names_the_checks(self):
        self.assertEqual(self.names(None, "apps/tl/shell/asking.tl"),
                         ["app-surfaces", "sdk-policy", "sdk-types"])
        self.assertEqual(self.names(None, "crates/core/tilo-actions/src/lib.rs"), [])


class InheritedRedTests(unittest.TestCase):
    """A failure main has at the push's base is not the push's to fix."""

    def result(self, blame=None, fixed=None, check="clippy/x", outcome="fail") -> dict:
        result = {"check": check, "outcome": outcome}
        if blame is not None:
            result["blame"] = blame
        if fixed is not None:
            result["fixed"] = fixed
        return result

    def test_only_inherited_flaky_and_fixed_failures_are_not_the_pushes(self):
        module = pre_push_lint()
        inherited = self.result({"inherited": {"since": "abcdef0123456789", "claimed_by": None}})
        for result in (inherited, self.result("flaky"), self.result(fixed={"run": "r1"})):
            self.assertTrue(module.not_yours(result), result)
        for result in (self.result("new"), self.result("unknown"),
                       self.result({"uncompared": None}), self.result()):
            self.assertFalse(module.not_yours(result), result)

    def test_an_inherited_red_names_its_since_and_owner(self):
        module = pre_push_lint()
        result = self.result({"inherited": {"since": "abcdef0123456789", "claimed_by": "quinten"}})
        self.assertEqual(module.blame_words(result),
                         "main has failed it since abcdef012345, claimed by quinten")
        unclaimed = self.result({"inherited": {"since": "abcdef0123456789", "claimed_by": None}})
        self.assertEqual(module.blame_words(unclaimed),
                         "main has failed it since abcdef012345, unclaimed")

    def test_a_board_with_no_verdict_says_why_it_may_be_yours(self):
        module = pre_push_lint()
        self.assertEqual(module.blame_words(self.result("unknown")),
                         "may be yours: main has no result at your base")

    def test_a_finding_count_is_not_a_new_finding(self):
        module = pre_push_lint()
        self.assertEqual(module.finding_key("dead_code a/src/lib.rs (2)"), "dead_code a/src/lib.rs")

    BASE = "b" * 40

    def item(self, since: str, **extra) -> dict:
        return {"check": "clippy/x", "test": None, "first_bad": since, "log": "l",
                "claim": {"session": "someone", "note": None}, **extra}

    def board(self, module, items, logs=None, ancestor=None):
        return module.Board(ROOT, module.Push(set(), [(self.BASE, self.BASE)], [], []),
                            items=items, logs=logs, ancestor=ancestor)

    def test_the_board_inherits_a_whole_check_red_at_the_base(self):
        module = pre_push_lint()
        item = self.item("a" * 40)
        found, why = self.board(module, [item], ancestor=lambda since, of: True).judge(
            {"check": "clippy/x", "outcome": "fail"})
        self.assertEqual((found, why), (item, ""))
        self.assertEqual(module.item_blame(found)["inherited"]["claimed_by"], "someone")

    def test_the_board_refuses_a_finding_main_does_not_name(self):
        module = pre_push_lint()
        board = self.board(module, [self.item("a" * 40)], logs={"l": {"dead_code a.rs"}},
                           ancestor=lambda since, of: True)
        found, why = board.judge({"check": "clippy/x", "outcome": "fail",
                                  "findings": ["unused_imports b.rs"]})
        self.assertIsNone(found)
        self.assertEqual(why, "new: unused_imports b.rs")

    def test_the_board_passes_a_finding_main_names(self):
        module = pre_push_lint()
        board = self.board(module, [self.item("a" * 40)], logs={"l": {"unused_imports b.rs"}},
                           ancestor=lambda since, of: True)
        found, why = board.judge({"check": "clippy/x", "outcome": "fail",
                                  "findings": ["unused_imports b.rs (2)"]})
        self.assertIsNotNone(found)
        self.assertEqual(why, "")

    def test_the_board_refuses_a_red_whose_commit_is_not_here(self):
        module = pre_push_lint()
        found, why = self.board(module, [self.item("a" * 40)],
                                ancestor=lambda since, of: None).judge(
            {"check": "clippy/x", "outcome": "fail"})
        self.assertIsNone(found)
        self.assertIn("does not have", why)

    def test_the_board_refuses_a_red_that_began_after_the_base(self):
        module = pre_push_lint()
        found, why = self.board(module, [self.item("a" * 40)],
                                ancestor=lambda since, of: False).judge(
            {"check": "clippy/x", "outcome": "fail"})
        self.assertIsNone(found)
        self.assertIn("after this push's base", why)

    def test_a_result_told_twice_is_judged_once(self):
        module = pre_push_lint()
        first = json.dumps({"type": "event", "body": {"finished": {
            "check": "x", "outcome": "fail", "blame": "unknown"}}})
        second = json.dumps({"type": "event", "body": {"finished": {
            "check": "x", "outcome": "fail",
            "blame": {"inherited": {"since": "a", "claimed_by": None}}}}})
        finished, errors = module.t_outcomes(first + "\n" + second + "\n")
        self.assertEqual(errors, [])
        self.assertEqual(len(finished), 1)
        self.assertEqual(finished[0]["blame"],
                         {"inherited": {"since": "a", "claimed_by": None}})

    def test_report_passes_a_run_t_blamed_on_main(self):
        module = pre_push_lint()
        run = module.Lint("clippy (t)", ["t", "test", "clippy/x", "--json"], True)
        output = json.dumps({"type": "document", "body": {"finished": {
            "check": "clippy/x", "outcome": "fail",
            "blame": {"inherited": {"since": "abcdef0123456789", "claimed_by": None}},
            "excerpt": "warning: unused import"}}}) + "\n"
        push = module.Push(set(), [(self.BASE, self.BASE)], [], [])
        self.assertEqual(module.report([(run, module.EXIT_ONLY_INHERITED, output)], 1.0,
                                       "one check", set(), ROOT, push), 0)


if __name__ == "__main__":
    unittest.main()
