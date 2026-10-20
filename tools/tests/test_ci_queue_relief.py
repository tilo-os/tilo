"""Regression checks for the temporary, reversible CI capacity policy.

These inspect the repository's block-style workflow sections, not a general YAML
parser. Live scheduler behavior is qualified separately; this suite protects the
trigger and job boundaries that caused the main-push build storm.
"""
from pathlib import Path
import re
import os
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


def workflow(name):
    return (ROOT / ".forgejo/workflows" / (name + ".yml")).read_text(encoding="utf-8")


def section(text, key, indent=0):
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines)
                 if line == " " * indent + key + ":")
    result = []
    for line in lines[start + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


class QueueReliefTests(unittest.TestCase):
    def test_pr_gates_stay_light_while_contract_coverage_is_kept(self):
        with self.subTest(phase='os full build is not a pr gate'):
            events = section(workflow("build-image"), "on")
            self.assertNotRegex(events, r"(?m)^  pull_request(?:_target)?:")
            # OS releases are tags + workflow_dispatch only (no nightly schedule).
            self.assertNotRegex(events, r"(?m)^  schedule:")
            self.assertRegex(events, r"(?m)^  workflow_dispatch:")
            self.assertRegex(events, r"(?m)^  push:")
        with self.subTest(phase='desktop prs keep preflight without packaging'):
            text = workflow("release-desktop")
            self.assertRegex(section(text, "on"), r"(?m)^  pull_request:")
            self.assertIn("steps:", section(text, "validate", 2))
            self.assertRegex(section(text, "package", 2),
                             r"if: .*github.event_name != 'pull_request'")
        with self.subTest(phase='app prs start no release jobs and never package'):
            text = workflow("release-apps")
            self.assertNotRegex(section(text, "on"), r"(?m)^  pull_request(?:_target)?:")
            self.assertRegex(section(text, "build", 2),
                             r"if: .*github.event_name != 'pull_request'")
        with self.subTest(phase='specialized contract checks keep pr and periodic coverage'):
            for name in ("ci-legacy-apps", "ci-filesystem"):
                with self.subTest(workflow=name):
                    events = section(workflow(name), "on")
                    if name == "ci-legacy-apps":
                        self.assertNotRegex(events, r"(?m)^  push:")
                    for event in ("pull_request", "schedule", "workflow_dispatch"):
                        self.assertRegex(events, rf"(?m)^  {event}:")
                    self.assertTrue(section(workflow(name), "jobs"))
        with self.subTest(phase='builder recipe does not run e2e against the old image'):
            legacy_events = section(workflow("ci-legacy-apps"), "on")
            self.assertNotIn('os/Containerfile.builder', legacy_events)
            self.assertIn('os/Containerfile.builder', section(workflow("build-ci-builder"), "on"))
        with self.subTest(phase='os candidate can run after skipped signing preflight'):
            build = section(workflow("build-image"), "build-and-push", 2)
            self.assertIn("always()", build)
            self.assertIn("needs.plan.result == 'success'", build)
            self.assertIn("needs.signing-preflight.result == 'skipped'", build)

    def test_integration_promotion_uses_control_pool_with_publication_guards(self):
        job = section(workflow("build-image"), "promote-integration", 2)
        self.assertRegex(job, r"(?m)^    runs-on: runner-quality$")
        self.assertIn("needs: [plan, build-and-push]", job)
        for guard in ("needs.plan.outputs.publish != 'true'",
                      "needs.plan.outputs.signed-development-board != 'true'",
                      "needs.plan.outputs.skip != 'true'",
                      "github.ref == 'refs/heads/main'"):
            self.assertIn(guard, job)
        # Exercise the production publication section: routing must retain
        # source binding, exact native children, monotonicity and provenance.
        for contract in ('org.opencontainers.image.revision',
                         '[[ "$actual" == "$expected" ]]',
                         'merge-base --is-ancestor "$current" "$TILO_IMAGE_SOURCE"',
                         '--multi-arch index-only --preserve-digests',
                         "name: tilo-os-integration-index",
                         "if-no-files-found: error"):
            self.assertIn(contract, job)
        self.assertEqual(job.count("if: steps.lanes.outputs.both == 'true'"), 3)

    def test_older_pinned_image_cannot_move_main_ci_backward(self):
        job = section(workflow("build-image"), "promote-integration", 2)
        block = job.split("      - name: Move tilo-vm:main-ci to this commit's dev images\n", 1)[1]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1].split("      - uses:", 1)[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "repository.git"
            repo.mkdir()
            env = {**os.environ, "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "commit.gpgsign",
                   "GIT_CONFIG_VALUE_0": "false"}
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=repo, env=env, text=True,
                                               stderr=subprocess.DEVNULL).strip()
            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (repo / "payload").write_text("old")
            git("add", "payload")
            git("commit", "-qm", "old")
            old = git("rev-parse", "HEAD")
            (repo / "payload").write_text("new")
            git("commit", "-qam", "new")
            new = git("rev-parse", "HEAD")
            fake = root / "skopeo"
            fake.write_text('#!/bin/sh\ncase "$*" in\n  *--raw*) printf \'%s\\n\' \'{"manifests":[{"platform":{"architecture":"amd64"}},{"platform":{"architecture":"arm64"}}]}\' ;;\n  inspect*) printf \'{"Labels":{"org.opencontainers.image.revision":"%s"}}\\n\' "$CURRENT_SOURCE" ;;\n  copy*) printf copied > "$COPY_LOG" ;;\n  *) exit 1 ;;\nesac\n')
            fake.chmod(0o755)
            copied = root / "copied"
            env.update({"PATH": f"{root}:{os.environ['PATH']}", "CURRENT_SOURCE": new,
                        "GITHUB_SHA": new, "TILO_IMAGE_SOURCE": old, "GIT_TOKEN": "fixture",
                        "GITHUB_SERVER_URL": root.as_uri(), "GITHUB_REPOSITORY": repo.name,
                        "RUNNER_TEMP": str(root), "COPY_LOG": str(copied)})
            result = subprocess.run(["bash", "-c", script], env=env, cwd=root, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("leaving it", result.stdout)
            self.assertFalse(copied.exists(), "An older source moved main-ci backward")

    def test_queue_relief_cancels_only_superseded_heavy_pr_runs(self):
        with self.subTest(phase='component release tags and manual runs remain'):
            for name, tags in (("build-image", '["v*"]'),
                               ("release-desktop", '["desktop-v*"]'),
                               ("release-apps", '["apps-v*", "app-*-v*"]')):
                with self.subTest(workflow=name):
                    events = section(workflow(name), "on")
                    push = section(events, "push", 2)
                    self.assertIn("tags: " + tags, push)
                    if name == "release-apps":
                        self.assertRegex(push, r"(?m)^    branches: \[main\]$")
                    else:
                        self.assertNotRegex(push, r"(?m)^    branches(?:-ignore)?:")
                    self.assertRegex(events, r"(?m)^  workflow_dispatch:")
        with self.subTest(phase='only heavy fuse pr runs replace their own previous run'):
            concurrency = section(workflow("ci-filesystem"), "concurrency")
            group = re.search(r"(?m)^  group: (.+)$", concurrency).group(1)
            cancel = re.search(r"(?m)^  cancel-in-progress: (.+)$", concurrency).group(1)
            self.assertEqual(cancel, "${{ github.event_name == 'pull_request' }}")

            def render(event, sha, number=""):
                context = {"github.event_name": event, "github.sha": sha,
                           "github.event.pull_request.number": number}
                def expression(match):
                    values = [context[key.strip()] for key in match.group(1).split("||")]
                    return next(value for value in values if value)
                return re.sub(r"\$\{\{\s*(.*?)\s*\}\}", expression, group)

            first = render("pull_request", "a" * 40, "172")
            # A new head of this PR must supersede the old native run.
            self.assertEqual(first, render("pull_request", "b" * 40, "172"))
            # Every other run, pushes to main included, keeps one run per commit.
            others = [render("pull_request", "a" * 40, "173"),
                      render("workflow_dispatch", "a" * 40),
                      render("schedule", "a" * 40),
                      render("push", "a" * 40),
                      render("push", "b" * 40)]
            self.assertEqual(len({first, *others}), 6)


if __name__ == "__main__":
    unittest.main()
