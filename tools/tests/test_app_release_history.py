"""Targeted app-release history fetches against real shallow repositories."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import app_packaging_contract as contract
import app_packaging_qualification as qualification

SPEC = importlib.util.spec_from_file_location(
    "app_release_history", ROOT / "tools/ci/app_release_history.py"
)
history = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(history)


class AppReleaseHistory(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.remote = self.root / "remote.git"
        self.command("git", "init", "-q", "-b", "main", str(self.source))
        self.git(self.source, "config", "user.name", "Fixture")
        self.git(self.source, "config", "user.email", "fixture@example.invalid")

    def command(self, *args, cwd=None):
        return subprocess.check_output(
            args, cwd=cwd, stderr=subprocess.PIPE, text=True
        ).strip()

    def git(self, repository, *args):
        return self.command("git", "-C", str(repository), *args)

    def commit(self, text):
        (self.source / "file").write_text(text, encoding="utf-8")
        self.git(self.source, "add", "file")
        self.git(self.source, "commit", "-qm", text)
        return self.git(self.source, "rev-parse", "HEAD")

    def publish(self):
        self.command("git", "clone", "-q", "--bare", str(self.source), str(self.remote))

    def shallow_clone(self, ref):
        clone = self.root / ("clone-" + ref.replace("/", "-"))
        self.command(
            "git",
            "clone",
            "-q",
            "--depth=1",
            "--branch",
            ref,
            self.remote.as_uri(),
            str(clone),
        )
        return clone

    def test_shallow_checkouts_fetch_exactly_the_history_planner_and_guard_need(self):
        with self.subTest(phase='exact fetch adds only the requested commit'):
            self.doCleanups()
            self.setUp()
            parent = self.commit("parent")
            self.git(self.source, "tag", "older", parent)
            base = self.commit("base")
            self.commit("head")
            self.publish()
            clone = self.shallow_clone("main")
            head = self.git(clone, "rev-parse", "HEAD")

            history.fetch_exact(clone, base, history.BASE_REF)

            self.assertEqual(self.git(clone, "rev-parse", history.BASE_REF), base)
            self.assertEqual(self.git(clone, "rev-parse", "HEAD"), head)
            refs = self.git(clone, "for-each-ref", "--format=%(refname)").splitlines()
            self.assertNotIn("refs/tags", "\n".join(refs))
            self.assertIn(history.BASE_REF, refs)
            self.assertNotEqual(
                subprocess.run(
                    ["git", "-C", str(clone), "cat-file", "-e", f"{parent}^{{commit}}"],
                    capture_output=True,
                ).returncode,
                0,
            )
            self.assertNotEqual(
                subprocess.run(
                    ["git", "-C", str(clone), "rev-parse", "--verify", "refs/tags/older"],
                    capture_output=True,
                ).returncode,
                0,
            )
        with self.subTest(phase='cli dispatches both commands and reports fetch failures'):
            self.doCleanups()
            self.setUp()
            base = self.commit("base")
            source = self.commit("source")
            self.publish()
            clone = self.shallow_clone("main")
            script = ROOT / "tools/ci/app_release_history.py"

            fetched = subprocess.run(
                [sys.executable, str(script), "fetch-base", base, source],
                cwd=clone,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertIn("release comparison ready", fetched.stdout)
            accepted = subprocess.run(
                [sys.executable, str(script), "require-main-ancestor", source],
                cwd=clone,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertIn("release source is on observed main", accepted.stdout)
            current = subprocess.run(
                [sys.executable, str(script), "require-main-tip", source],
                cwd=clone,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertIn("release source is current observed main", current.stdout)

            self.commit("newer")
            self.git(self.source, "push", "-q", str(self.remote), "main")
            stale = subprocess.run(
                [sys.executable, str(script), "require-main-tip", source],
                cwd=clone,
                capture_output=True,
                text=True,
            )
            self.assertEqual(stale.returncode, 1)
            self.assertIn("is not the current main tip", stale.stderr)

            rejected = subprocess.run(
                [sys.executable, str(script), "fetch-base", "0" * 40, source],
                cwd=clone,
                capture_output=True,
                text=True,
            )
            self.assertEqual(rejected.returncode, 1)
            self.assertIn("targeted release comparison fetch failed", rejected.stderr)
        with self.subTest(phase='comparison history is bounded and fails closed'):
            self.doCleanups()
            self.setUp()
            ancestor = self.commit("ancestor")
            self.git(self.source, "checkout", "-qb", "candidate")
            for number in range(5):
                self.commit(f"candidate-{number}")
            self.git(self.source, "checkout", "-q", "main")
            for number in range(5):
                self.commit(f"main-{number}")
            base = self.git(self.source, "rev-parse", "HEAD")
            self.publish()
            clone = self.shallow_clone("candidate")

            with self.assertRaisesRegex(RuntimeError, "after depth 3"):
                history.prepare_comparison(
                    clone, base, self.git(clone, "rev-parse", "HEAD"), (2, 3)
                )
        with self.subTest(phase='real planner resolves changes from a depth one checkout'):
            self.doCleanups()
            self.setUp()
            (self.source / "apps/notes/src").mkdir(parents=True)
            (self.source / "dist/apps").mkdir(parents=True)
            (self.source / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["apps/notes"]\nresolver = "2"\n',
                encoding="utf-8",
            )
            (self.source / "Cargo.lock").write_text(
                'version = 4\n\n[[package]]\nname = "tilo-app-notes"\nversion = "1.0.0"\n',
                encoding="utf-8",
            )
            (self.source / "apps/notes/Cargo.toml").write_text(
                '[package]\nname = "tilo-app-notes"\nversion = "1.0.0"\n'
                'edition = "2024"\n\n[[bin]]\nname = "tilo-notes"\n'
                'path = "src/main.rs"\n',
                encoding="utf-8",
            )
            (self.source / "apps/notes/tilo-app.toml").write_text(
                '[package]\nid = "org.tilo.notes"\nversion = "1.0.0"\n'
                'runtime = "1"\n\n[entrypoint]\nbin = "tilo-notes"\n',
                encoding="utf-8",
            )
            (self.source / "dist/apps/app-collections.toml").write_text(
                '[collections.default]\ninclude = ["notes"]\n', encoding="utf-8"
            )
            (self.source / "apps/notes/src/main.rs").write_text(
                'fn main() {}\n', encoding="utf-8"
            )
            self.git(self.source, "add", ".")
            self.git(self.source, "commit", "-qm", "base")
            base = self.git(self.source, "rev-parse", "HEAD")
            (self.source / "apps/notes/src/main.rs").write_text(
                'fn main() { println!("changed"); }\n', encoding="utf-8"
            )
            self.git(self.source, "add", "apps/notes/src/main.rs")
            self.git(self.source, "commit", "-qm", "head")
            self.publish()
            clone = self.shallow_clone("main")

            head = self.git(clone, "rev-parse", "HEAD")
            self.assertEqual(history.prepare_comparison(clone, base, head, (2,)), base)
            result = json.loads(
                self.command(
                    sys.executable,
                    str(ROOT / "tools/apps/app_release.py"),
                    "--root",
                    str(clone),
                    "--config",
                    str(clone / "dist/apps/app-collections.toml"),
                    "--base",
                    base,
                )
            )

            self.assertEqual(result["apps"], ["notes"])
            self.assertEqual(result["reason"], "release inputs changed: binary")
            self.assertEqual(
                self.git(clone, "rev-parse", "HEAD"),
                self.git(clone, "rev-parse", history.HEAD_REF),
            )
        with self.subTest(phase='depth one coordinator guard uses prepared exact history'):
            self.doCleanups()
            self.setUp()
            baseline = self.commit("baseline")
            source = self.commit("source")
            self.publish()
            clone = self.shallow_clone("main")
            self.assertEqual(self.git(clone, "rev-parse", "HEAD"), source)

            history.prepare_comparison(clone, baseline, source, (2,))
            desired = {
                "source_sha": baseline,
                "builder_image": "registry.tilo.so/tilo/tilo-builder@sha256:" + "d" * 64,
                "collection": "default",
                "architectures": ["x86_64", "aarch64"],
                "policy_sha256": "f" * 64,
            }
            proof = {
                "schema": "tilo.coordinator-verification/v1",
                "run_id": 7,
                "desired": desired,
                "backend": "canonical-final-artifact-v1",
                "receipts": {"x86_64": "1" * 64, "aarch64": "2" * 64},
            }
            env = {
                "COORDINATOR_INTENT": "a9eb3ab4-4321-4321-8321-123456789abc",
                "COORDINATOR_ENABLED": "true",
                "GITHUB_EVENT_NAME": "workflow_dispatch",
                "GITHUB_SHA": source,
                "GITHUB_REF": source,
                "COORDINATOR_POLICY_SHA256": desired["policy_sha256"],
                "REQUESTED_COLLECTION": "default",
                "REQUESTED_ARCHITECTURES": "x86_64,aarch64",
                "REQUESTED_PUBLISH": "false",
                "REQUESTED_BUILDER": desired["builder_image"],
                "BASELINE_SHA": baseline,
                "BASELINE_RECEIPT_SHA256": contract.receipt_key(proof),
                "BASELINE_RUN_ID": "7",
                "BASELINE_EVIDENCE": contract.canonical(proof),
            }
            real_run = subprocess.run

            def run_in_clone(arguments, **kwargs):
                return real_run(arguments, cwd=clone, **kwargs)

            with mock.patch.object(qualification.subprocess, "run", run_in_clone):
                self.assertTrue(qualification.validate(env))

            # Selection reuses the graph prepared for the guard without fetching.
            with mock.patch.object(history, "fetch_comparison") as fetch:
                self.assertEqual(history.prepare_comparison(clone, baseline, source), baseline)
            fetch.assert_not_called()

    def test_release_source_ancestry_is_proven_on_observed_main_or_rejected(self):
        with self.subTest(phase='tag ancestry deepens only the observed main tip'):
            self.doCleanups()
            self.setUp()
            candidate = ""
            for number in range(20):
                sha = self.commit(f"main-{number}")
                if number == 4:
                    candidate = sha
                    self.git(self.source, "tag", "app-notes-v1", candidate)
            main = self.git(self.source, "rev-parse", "HEAD")
            self.git(self.source, "checkout", "-qb", "unrelated", candidate)
            self.commit("unrelated")
            self.git(self.source, "checkout", "-q", "main")
            self.publish()
            clone = self.shallow_clone("app-notes-v1")

            self.assertEqual(history.require_main_ancestor(clone, candidate, (4, 32)), main)
            self.assertEqual(self.git(clone, "rev-parse", history.MAIN_REF), main)
            refs = self.git(clone, "for-each-ref", "--format=%(refname)")
            self.assertNotIn("refs/remotes/origin/unrelated", refs)
            with mock.patch.object(history, "fetch_exact") as fetch:
                self.assertEqual(history.require_main_ancestor(clone, candidate), main)
            fetch.assert_not_called()
        with self.subTest(phase='diverged tag is rejected after main history is complete'):
            self.doCleanups()
            self.setUp()
            ancestor = self.commit("ancestor")
            self.git(self.source, "checkout", "-qb", "release", ancestor)
            candidate = self.commit("diverged release")
            self.git(self.source, "tag", "app-notes-v2", candidate)
            self.git(self.source, "checkout", "-q", "main")
            for number in range(6):
                self.commit(f"main-{number}")
            self.publish()
            clone = self.shallow_clone("app-notes-v2")

            with self.assertRaisesRegex(RuntimeError, "is not an ancestor of main"):
                history.require_main_ancestor(clone, candidate, (2, 4, 16))
        with self.subTest(phase='abandoned main run is rejected after force push'):
            self.doCleanups()
            self.setUp()
            ancestor = self.commit("ancestor")
            abandoned = self.commit("abandoned")
            self.publish()
            clone = self.shallow_clone("main")
            self.git(self.source, "reset", "--hard", ancestor)
            self.commit("replacement")
            self.git(self.source, "push", "-q", "--force", str(self.remote), "main")

            with self.assertRaisesRegex(RuntimeError, "is not an ancestor of main"):
                history.require_main_ancestor(clone, abandoned, (2, 4, 16))
        with self.subTest(phase='invalid or mismatched release source fails closed'):
            self.doCleanups()
            self.setUp()
            head = self.commit("head")
            self.publish()
            clone = self.shallow_clone("main")
            with self.assertRaisesRegex(ValueError, "immutable full commit ID"):
                history.require_main_ancestor(clone, "main")
            other = "0" * len(head)
            with self.assertRaisesRegex(ValueError, "does not match release source"):
                history.require_main_ancestor(clone, other)
            with self.assertRaisesRegex(ValueError, "immutable full commit ID"):
                history.prepare_comparison(clone, "main", head)
            with self.assertRaisesRegex(ValueError, "does not match release source"):
                history.prepare_comparison(clone, head, other)
        with self.subTest(phase='incomplete main never turns into a negative ancestry decision'):
            self.doCleanups()
            self.setUp()
            candidate = "a" * 40
            observed_main = "b" * 40
            completed = mock.Mock(stdout=candidate + "\n", stderr="", returncode=0)
            fetches = []

            with mock.patch.object(history, "git", return_value=completed), \
                    mock.patch.object(history, "remote_main", return_value=observed_main), \
                    mock.patch.object(
                        history, "fetch_exact", side_effect=lambda *args: fetches.append(args)
                    ), \
                    mock.patch.object(history, "is_ancestor", return_value=False), \
                    mock.patch.object(history, "main_has_shallow_boundary", return_value=True):
                with self.assertRaisesRegex(RuntimeError, "within bounded depth 8"):
                    history.require_main_ancestor(Path("."), candidate, (8,))

            self.assertEqual([call[-1] if len(call) == 4 else 1 for call in fetches],
                             [8])
        with self.subTest(phase='last bounded fetch can prove an old main ancestor'):
            self.doCleanups()
            self.setUp()
            candidate = "a" * 40
            observed_main = "b" * 40
            completed = mock.Mock(stdout=candidate + "\n", stderr="", returncode=0)
            fetches = []

            with mock.patch.object(history, "git", return_value=completed), \
                    mock.patch.object(history, "remote_main", return_value=observed_main), \
                    mock.patch.object(
                        history, "fetch_exact", side_effect=lambda *args: fetches.append(args)
                    ), \
                    mock.patch.object(
                        history, "is_ancestor", side_effect=(False, True)
                    ), \
                    mock.patch.object(
                        history, "main_has_shallow_boundary", return_value=True
                    ):
                self.assertEqual(
                    history.require_main_ancestor(Path("."), candidate, (8,)), observed_main
                )

            self.assertEqual([call[-1] for call in fetches], [8])
        with self.subTest(phase='tag at observed main needs no history fetch'):
            self.doCleanups()
            self.setUp()
            source = self.commit("source")
            self.publish()
            clone = self.shallow_clone("main")

            with mock.patch.object(history, "fetch_exact") as fetch:
                self.assertEqual(history.require_main_ancestor(clone, source), source)

            fetch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
