from pathlib import Path
import os
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / "ci/check-unused-dependencies.sh"


class UnusedDependencyTests(unittest.TestCase):
    def test_manifest_scope_keeps_standalone_and_nested_first_party(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            paths = ["Cargo.toml", "apps/a/Cargo.toml", "tools/space name/Cargo.toml",
                     "crates/a/vendor/nested/Cargo.toml", "vendor/codec/Cargo.toml"]
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("[package]\nname = 'fixture'\nversion = '0.1.0'\n")
            # Include tracked and untracked first-party manifests for local use.
            subprocess.run(["git", "add", "Cargo.toml"], cwd=root, check=True)
            binary = root / "bin"
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\nexit 1\n')
            cargo.chmod(0o755)
            output = root / "arguments"
            env = dict(os.environ, TILO_REPO_ROOT=str(root), ARGUMENTS=str(output),
                       PATH=str(binary) + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(SCRIPT)], env=env)
            self.assertEqual(result.returncode, 1, "unused-dependency failures must propagate")
            args = output.read_text().splitlines()
            self.assertEqual(args[:2], ["machete", "--with-metadata"])
            self.assertCountEqual(args[2:], ["Cargo.toml", "apps", "tools", "crates"])

    def test_metadata_runs_in_writable_snapshot_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            (root / "Cargo.toml").write_text("[workspace]\nmembers = []\n")
            lock = root / "Cargo.lock"
            lock.write_text("original lock\n")
            lock.chmod(0o444)
            standalone = root / "standalone"
            standalone.mkdir()
            manifest = standalone / "Cargo.toml"
            manifest.write_text("[workspace]\n[package]\nname = 'standalone'\nversion = '0.1.0'\n")
            (root / ".gitignore").write_text("ignored\n")
            (root / "ignored").write_text("not a source input\n")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            # A deleted tracked file must stay deleted in the snapshot.
            manifest.unlink()
            binary = root / "bin"
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text(
                '#!/bin/sh\nset -eu\n'
                'test "$PWD" != "$TILO_REPO_ROOT"\n'
                'test -f Cargo.toml\n'
                'test ! -e standalone/Cargo.toml\n'
                'test ! -e ignored\n'
                'printf "%s" "$PWD" > "$SNAPSHOT"\n'
                'printf "metadata lock\\n" > Cargo.lock\n'
                'exit 1\n'
            )
            cargo.chmod(0o755)
            snapshot = root / "snapshot-path"
            env = dict(os.environ, TILO_REPO_ROOT=str(root), SNAPSHOT=str(snapshot),
                       PATH=str(binary) + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(SCRIPT)], env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertTrue(snapshot.exists(), "the metadata command must run")
            self.assertEqual(lock.read_text(), "original lock\n")
            self.assertFalse(Path(snapshot.read_text()).exists(), "snapshot must be cleaned on failure")

    def test_the_hakari_generated_crate_is_never_scanned(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            identity = {"GIT_AUTHOR_NAME": "CI", "GIT_AUTHOR_EMAIL": "ci@example.invalid",
                        "GIT_COMMITTER_NAME": "CI", "GIT_COMMITTER_EMAIL": "ci@example.invalid"}
            git_env = dict(os.environ, **identity)

            def git(*args):
                return subprocess.run(["git", *args], cwd=root, env=git_env, check=True,
                                      capture_output=True, text=True).stdout.strip()

            manifest = "[package]\nname = 'fixture'\nversion = '0.1.0'\n"
            files = {
                "Cargo.toml": manifest,
                "apps/a/Cargo.toml": manifest,
                "crates/sdk/ui/Cargo.toml": manifest,
                "crates/system/fs/Cargo.toml": manifest,
                "crates/workspace-hack/Cargo.toml":
                    manifest + "\n### BEGIN HAKARI SECTION\n[dependencies]\ntokio = '1'\n"
                    "\n### END HAKARI SECTION\n",
                "crates/workspace-hack/src/lib.rs": "// stub\n",
                "crates/system/fs/src/lib.rs": "\n",
            }
            for name, text in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            git("init", "-q")
            git("add", "-A")
            git("commit", "-qm", "base")

            binary = root.parent / (root.name + "-bin")
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\n')
            cargo.chmod(0o755)
            output = root.parent / (root.name + "-arguments")
            env = dict(os.environ, TILO_REPO_ROOT=str(root), ARGUMENTS=str(output),
                       PATH=str(binary) + os.pathsep + os.environ["PATH"])

            with self.subTest(phase="full scan covers the generated crate's siblings only"):
                result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                args = output.read_text().splitlines()
                self.assertEqual(args[:2], ["machete", "--with-metadata"])
                self.assertCountEqual(args[2:], ["Cargo.toml", "apps", "crates/sdk", "crates/system"])

            with self.subTest(phase="a change inside the generated crate scans nothing"):
                base = git("rev-parse", "HEAD")
                for name in ("crates/workspace-hack/src/lib.rs", "crates/system/fs/src/lib.rs"):
                    (root / name).write_text("changed\n")
                git("commit", "-qam", "change")
                output.unlink()
                result = subprocess.run(["bash", str(SCRIPT), "--changed", "", base], env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_text().splitlines()[2:], ["crates/system/fs"])

    def test_changed_scope_scans_only_crates_that_own_a_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            identity = {"GIT_AUTHOR_NAME": "CI", "GIT_AUTHOR_EMAIL": "ci@example.invalid",
                        "GIT_COMMITTER_NAME": "CI", "GIT_COMMITTER_EMAIL": "ci@example.invalid"}
            git_env = dict(os.environ, **identity)

            def git(*args):
                return subprocess.run(["git", *args], cwd=root, env=git_env, check=True,
                                      capture_output=True, text=True).stdout.strip()

            def write(name, text="fixture\n"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)

            git("init", "-q")
            manifest = "[package]\nname = 'fixture'\nversion = '0.1.0'\n"
            for name in ("Cargo.toml", "apps/a/Cargo.toml", "crates/b/Cargo.toml",
                         "crates/b/fuzz/Cargo.toml", "vendor/codec/Cargo.toml"):
                write(name, manifest)
            for name in ("apps/a/src/lib.rs", "crates/b/src/gone.rs", "crates/b/src/moved.rs",
                         "crates/b/fuzz/src/main.rs", "vendor/codec/src/lib.rs", "docs/a.md"):
                write(name)
            git("add", "-A")
            git("commit", "-qm", "base")

            binary = root.parent / (root.name + "-bin")
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\n')
            cargo.chmod(0o755)
            output = root.parent / (root.name + "-arguments")
            env = dict(os.environ, TILO_REPO_ROOT=str(root), ARGUMENTS=str(output),
                       PATH=str(binary) + os.pathsep + os.environ["PATH"])

            def scanned(*paths):
                base = git("rev-parse", "HEAD")
                for name in paths:
                    write(name, "changed\n")
                git("add", "-A")
                git("commit", "-qm", "change", "--allow-empty")
                output.unlink(missing_ok=True)
                result = subprocess.run(["bash", str(SCRIPT), "--changed", "", base], env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                if not output.exists():
                    return None
                args = output.read_text().splitlines()
                self.assertEqual(args[:2], ["machete", "--with-metadata"])
                return args[2:]

            # Prose, vendored code and files outside every crate scan nothing.
            self.assertIsNone(scanned("docs/a.md", "vendor/codec/src/lib.rs", "README.md"))
            # The nearest owning crate is scanned, nested crates on their own.
            self.assertEqual(scanned("apps/a/src/lib.rs", "crates/b/fuzz/src/main.rs"),
                             ["apps/a", "crates/b/fuzz"])
            # A deletion, or a rename away, can remove a dependency's last use.
            (root / "crates/b/src/gone.rs").unlink()
            self.assertEqual(scanned(), ["crates/b"])
            base = git("rev-parse", "HEAD")
            (root / "crates/b/src/moved.rs").rename(root / "apps/a/src/moved.rs")
            git("add", "-A")
            git("commit", "-qm", "move")
            result = subprocess.run(["bash", str(SCRIPT), "--changed", "", base], env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_text().splitlines()[2:], ["apps/a", "crates/b"])
            # Crates inherit from the workspace manifest: every crate is scanned.
            self.assertCountEqual(scanned("Cargo.toml"), ["Cargo.toml", "apps", "crates"])

    def test_workspace_member_additions_preserve_conservative_selection(self):
        original = "[workspace]\nmembers = ['apps/a', 'crates/b']\nresolver = '3'\n"
        # Preserve both existing members while adding the new one.
        added = original.replace("'crates/b']", "'crates/b', 'crates/new']")
        cases = [
            ("only addition", added, [], ["crates/new"]),
            ("addition plus existing source", added, ["apps/a/src/lib.rs"],
             ["apps/a", "crates/new"]),
            ("addition of existing unlisted crate", added.replace("crates/new", "tools/old"),
             [], ["tools/old"]),
            ("inherited dependencies", added + "[workspace.dependencies]\nserde = '1'\n", [], None),
            ("inherited package", added + "[workspace.package]\nedition = '2024'\n", [], None),
            ("unknown structural setting", added + "[profile.dev]\nopt-level = 1\n", [], None),
            ("member removal", original.replace("'apps/a', ", ""), [], None),
            ("member reorder", original.replace("'apps/a', 'crates/b'", "'crates/b', 'apps/a', 'crates/new'"), [], None),
            ("wildcard addition", added.replace("crates/new", "crates/*"), [], None),
            ("missing member", added.replace("crates/new", "crates/missing"), [], None),
            ("duplicate member", added.replace("crates/new", "apps/a"), [], None),
            ("unchanged structure", original + "# changed comment\n", [], None),
            ("invalid TOML", "[workspace\n", [], None),
            ("checker changed", added, ["tools/ci/workspace_member_additions.py"], None),
        ]
        for label, changed_manifest, source_changes, expected in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                git_env = dict(os.environ, GIT_AUTHOR_NAME="CI", GIT_AUTHOR_EMAIL="ci@example.invalid",
                               GIT_COMMITTER_NAME="CI", GIT_COMMITTER_EMAIL="ci@example.invalid")

                def git(*args):
                    return subprocess.check_output(["git", *args], cwd=root, env=git_env,
                                                   text=True).strip()

                def write(name, text):
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(text)

                git("init", "-q")
                write("Cargo.toml", original)
                for name in ("apps/a", "crates/b", "tools/old"):
                    write(name + "/Cargo.toml", "[package]\nname = 'fixture'\nversion = '0.1.0'\n")
                    write(name + "/src/lib.rs", "// original\n")
                git("add", "-A")
                git("commit", "-qm", "base")
                base = git("rev-parse", "HEAD")
                write("Cargo.toml", changed_manifest)
                if "crates/new" in changed_manifest:
                    write("crates/new/Cargo.toml", "[package]\nname = 'new'\nversion = '0.1.0'\n")
                    write("crates/new/src/lib.rs", "// new\n")
                for name in source_changes:
                    write(name, "// changed\n")
                git("add", "-A")
                git("commit", "-qm", "change")
                binary = root / "bin"
                binary.mkdir()
                cargo = binary / "cargo"
                cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\n')
                cargo.chmod(0o755)
                output = root / "arguments"
                env = dict(os.environ, TILO_REPO_ROOT=str(root), ARGUMENTS=str(output),
                           PATH=str(binary) + os.pathsep + os.environ["PATH"])
                result = subprocess.run(["bash", str(SCRIPT), "--changed", "", base],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                args = output.read_text().splitlines()
                self.assertEqual(args[:2], ["machete", "--with-metadata"])
                if expected is None:
                    expected = ["Cargo.toml", "apps", "crates", "tools"]
                self.assertCountEqual(args[2:], expected)


if __name__ == "__main__":
    unittest.main()
