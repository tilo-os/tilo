import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"


class PreCommitMergeContentTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        # Run the real hook explicitly below, not while constructing history.
        hooks = self.root / "empty-hooks"
        hooks.mkdir()
        self.git("config", "core.hooksPath", str(hooks))
        self.git("config", "commit.gpgsign", "false")
        hook = HOOK.read_text(encoding="utf-8")
        real_checks = {
            "tools/policy/check_source_content.py",
            "tools/policy/check_public_references.py",
            "tools/policy/added_line_policy.py",
        }
        # Keep the production hook and content/changed-line security checks.
        # Workspace/UI/interpreter inventories are unrelated to this tiny repo.
        for name in re.findall(r'python3 "\$root/([^"]+)"', hook):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if name in real_checks:
                shutil.copy2(ROOT / name, path)
            else:
                path.write_text("# Unrelated domain policy fixture.\n", encoding="utf-8")
        # The Rust lexer the changed-line allow check imports.
        lexer = self.root / "tools/policy/rust_source.py"
        lexer.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "tools/policy/rust_source.py", lexer)
        path = self.root / "scripts/check-workspace-complete.sh"
        path.parent.mkdir(parents=True)
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
        path = self.root / ".githooks/pre-commit"
        path.parent.mkdir()
        shutil.copy2(HOOK, path)

        # The old branch predates the exceptions policy and still has an asset
        # that main removed. Filtering inherited changes resurrects that asset
        # and removes the manifest, producing a tree no one intends to commit.
        (self.root / "obsolete.bin").write_bytes(b"\0obsolete")
        self.commit("test: establish old source tree")
        base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("rm", "obsolete.bin")
        (self.root / "inherited.bin").write_bytes(b"\0pinned asset")
        self.git("add", "inherited.bin")
        oid = self.git("rev-parse", ":inherited.bin").stdout.strip()
        self.manifest([{"path": "inherited.bin", "oid": oid,
                        "reason": "Required upstream fixture"}])
        # An inherited changed-line violation must remain outside the task's
        # filtered diff, while the whole-tree content audit still includes it.
        (self.root / "inherited.rs").write_text('fn main() { dbg!(1); }\n')
        self.commit("test: introduce pinned asset policy")
        self.git("checkout", "-qb", "task", base)
        (self.root / "task.txt").write_text("task source\n")
        self.commit("test: change task source")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "merge", "--no-commit", "main")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args],
                              capture_output=True, text=True, check=True)

    def commit(self, message):
        self.git("add", "--", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "commit", "-qm", message)

    def manifest(self, entries):
        path = self.root / "tools/policy/source_content_exceptions.json"
        path.write_text(json.dumps(entries) + "\n", encoding="utf-8")
        self.git("add", "--", str(path.relative_to(self.root)))

    def run_hook(self):
        before = self.git("write-tree").stdout
        result = subprocess.run(["bash", str(self.root / ".githooks/pre-commit")],
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(self.git("write-tree").stdout, before,
                         "the real candidate index must not be modified")
        return result

    def test_merge_audits_real_tree_before_filtering_inherited_manifest(self):
        self.assertNotIn("tools/policy/source_content_exceptions.json",
                         self.git("ls-tree", "-r", "--name-only", "HEAD").stdout)
        self.assertIn("tools/policy/source_content_exceptions.json",
                      self.git("ls-files").stdout)
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_merge_rejects_new_staged_binary_even_with_exception(self):
        path = self.root / "new.bin"
        path.write_bytes(b"\0forbidden new asset")
        self.git("add", "new.bin")
        inherited = json.loads((self.root / "tools/policy/source_content_exceptions.json").read_text())
        oid = self.git("rev-parse", ":new.bin").stdout.strip()
        self.manifest(inherited + [{"path": "new.bin", "oid": oid,
                                    "reason": "A new pin cannot authorize new bytes"}])
        path.write_text("worktree bytes must not hide the staged binary\n")
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("new.bin: non-text content; new/modified non-text or non-LF blobs are forbidden",
                      result.stdout + result.stderr)

    def test_merge_still_rejects_task_changed_line_violations(self):
        path = self.root / "task.rs"
        path.write_text('fn main() { dbg!(2); }\n')
        self.git("add", "task.rs")
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dbg!", result.stdout + result.stderr)


# A stand-in for `tilo`. Like the real one it reads each app's surface from the
# checkout it stands in and has what it knows of the API itself written into it:
# one built before the API gained a call does not know that call.
OLDER_GENERATOR = r"""#!/bin/sh
case "$1" in --help) echo "tilo: sdk gen writes the SDK's types"; exit 0 ;; esac
lang=; out=
while [ $# -gt 0 ]; do
  case "$1" in --lang) lang=$2; shift ;; --out) out=$2; shift ;; esac
  shift
done
mkdir -p "$out"
actions() { sed -n 's/^id = "\(.*\)"$/\1/p' "$1"; }
case "$lang" in
  openapi)
    {
      printf '{\n  "the API itself": [\n    "listWorkspaces",\n    "readResource"\n  ],\n'
      printf '  "between": "them",\n  "the apps": [\n'
      for surface in apps/*/surface.toml; do
        for action in $(actions "$surface"); do printf '    "%s",\n' "$action"; done
      done
      printf '    "runAction"\n  ]\n}\n'
    } > "$out/openapi.json" ;;
  *)
    for surface in apps/*/surface.toml; do
      app=$(basename "$(dirname "$surface")")
      actions "$surface" > "$out/$app.$lang"
    done ;;
esac
"""

SURFACE = '[[resources]]\nkind = "notes.notebook"\n\n[[resources.verbs]]\nid = "rename"\n'
# What a generator as new as main wrote: main's API has gained `listHolders`.
CURRENT_DESCRIPTION = (
    '{\n  "the API itself": [\n    "listWorkspaces",\n    "listHolders",\n    "readResource"\n  ],\n'
    '  "between": "them",\n  "the apps": [\n    "rename",\n    "runAction"\n  ]\n}\n'
)


class PreCommitSdkTypesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        hooks = self.root / "empty-hooks"
        hooks.mkdir()
        self.git("config", "core.hooksPath", str(hooks))
        self.git("config", "commit.gpgsign", "false")
        hook = HOOK.read_text(encoding="utf-8")
        # The step under test is the hook's own; every policy script is another subject.
        for name in re.findall(r'python3 "\$root/([^"]+)"', hook):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("# Unrelated domain policy fixture.\n", encoding="utf-8")
        path = self.root / "scripts/check-workspace-complete.sh"
        path.parent.mkdir(parents=True)
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
        path = self.root / ".githooks/pre-commit"
        path.parent.mkdir()
        shutil.copy2(HOOK, path)
        self.write("apps/notes/surface.toml", SURFACE)
        self.write("apps/notes/tilo-app.toml", '[package]\nid = "so.tilo.notes"\nname = "Notes"\n')
        self.write("docs/api/openapi.json", CURRENT_DESCRIPTION)
        self.write("sdk/python/src/tilo/types/notes.python", "rename\n")
        self.write("sdk/typescript/src/types/notes.ts", "rename\n")
        self.write(".gitignore", "/target/\n")
        self.git("add", "--", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "commit", "-qm", "test: an app with its surface and the SDK's files")
        generator = self.root / "target/debug/tilo"
        generator.parent.mkdir(parents=True)
        generator.write_text(OLDER_GENERATOR, encoding="utf-8")
        generator.chmod(0o755)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args],
                              capture_output=True, text=True, check=True)

    def run_hook(self):
        # No builder of anyone's own: the generator is the one this tree holds.
        tools = {str(Path(sys.executable).parent), str(Path(shutil.which("git")).parent)}
        env = dict(os.environ, PATH=os.pathsep.join([*sorted(tools), "/usr/bin", "/bin"]))
        return subprocess.run(["bash", str(self.root / ".githooks/pre-commit")],
                              cwd=self.root, capture_output=True, text=True, env=env)

    def staged(self, name):
        return self.git("show", f":{name}").stdout

    def test_an_older_generator_adds_what_the_surface_changed_and_takes_nothing_out(self):
        self.write("apps/notes/surface.toml", SURFACE + '\n[[resources.verbs]]\nid = "archive"\n')
        # A file its person left untracked among the types, which no commit takes along.
        self.write("sdk/python/src/tilo/types/scratch.python", "mine\n")
        self.git("add", "apps/notes/surface.toml")
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        described = self.staged("docs/api/openapi.json")
        self.assertIn('"archive"', described, "the surface's new action is described")
        self.assertIn('"listHolders"', described,
                      "a call the generator is too old to know stays in the description")
        self.assertEqual(self.staged("sdk/python/src/tilo/types/notes.python"), "rename\narchive\n")
        self.assertEqual(self.staged("sdk/typescript/src/types/notes.ts"), "rename\narchive\n")
        self.assertNotIn("scratch", self.git("ls-files", "sdk").stdout)

    def test_a_new_app_is_described_beside_the_others_and_its_types_are_written(self):
        self.write("apps/tasks/surface.toml", SURFACE.replace("rename", "assign"))
        self.write("apps/tasks/tilo-app.toml", '[package]\nid = "so.tilo.tasks"\nname = "Tasks"\n')
        self.git("add", "apps")
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        described = self.staged("docs/api/openapi.json")
        for call in ('"assign"', '"rename"', '"listHolders"'):
            self.assertIn(call, described)
        # Its first types are in the commit, not only in the working tree.
        self.assertEqual(self.staged("sdk/python/src/tilo/types/tasks.python"), "assign\n")
        self.assertEqual(self.staged("sdk/typescript/src/types/tasks.ts"), "assign\n")


if __name__ == "__main__":
    unittest.main()
