"""Regression checks for the temporary, reversible CI capacity policy.

These inspect the repository's block-style workflow sections, not a general YAML
parser. Live scheduler behavior is qualified separately; this suite protects the
trigger and job boundaries that caused the main-push build storm.
"""
from pathlib import Path
import re
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
        with self.subTest(phase='builder resolution keeps native context fixture in control lane'):
            builder = section(workflow("check"), "builder", 2)
            self.assertIn("runs-on: runner-quality", builder)
            container = section(builder, "container", 4)
            self.assertRegex(container, r"image: registry\.tilo\.so/tilo/ci-control@sha256:[0-9a-f]{64}")
            # Public image: a forge credential would only fail the pull.
            self.assertNotIn("credentials:", container)
            self.assertIn("image: ${{ steps.resolve.outputs.image }}", builder)
            self.assertIn('DOCKER_BUILDKIT: "1"', builder)
            self.assertIn("run: bash os/tests/named_public_trust_context.sh docker", builder)
        with self.subTest(phase='final gates use qualified lightweight image'):
            quality = section(workflow("lint"), "quality", 2)
            expected = section(quality, "container", 4)
            self.assertRegex(expected, r"image: registry\.tilo\.so/tilo/ci-fast@sha256:[0-9a-f]{64}")
            for name in ("select", "checks"):
                with self.subTest(job=name):
                    job = section(workflow("check"), name, 2)
                    self.assertEqual(section(job, "container", 4), expected)
                    self.assertIn("runs-on: runner-quality", job)
        with self.subTest(phase='os full build is not a pr gate'):
            events = section(workflow("build-image"), "on")
            self.assertNotRegex(events, r"(?m)^  pull_request(?:_target)?:")
            # OS releases are tags + workflow_dispatch only (no nightly schedule).
            self.assertNotRegex(events, r"(?m)^  schedule:")
            self.assertRegex(events, r"(?m)^  workflow_dispatch:")
            self.assertRegex(events, r"(?m)^  push:")
        with self.subTest(phase='app and desktop prs keep preflight without packaging'):
            for name, plan, build in (("release-apps", "plan", "build"),
                                      ("release-desktop", "validate", "package")):
                with self.subTest(workflow=name):
                    text = workflow(name)
                    self.assertRegex(section(text, "on"), r"(?m)^  pull_request:")
                    self.assertIn("steps:", section(text, plan, 2))
                    self.assertRegex(section(text, build, 2),
                                     r"if: .*github.event_name != 'pull_request'")
        with self.subTest(phase='specialized contract checks keep pr and periodic coverage'):
            for name in ("ci-legacy-apps", "ci-filesystem"):
                with self.subTest(workflow=name):
                    events = section(workflow(name), "on")
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

    def test_queue_relief_cancels_only_superseded_heavy_pr_runs(self):
        with self.subTest(phase='fast checks replace only a superseded run of the same pull request'):
            for name in ("check", "lint"):
                with self.subTest(workflow=name):
                    text = workflow(name)
                    events = section(text, "on")
                    self.assertRegex(events, r"(?m)^  push:")
                    self.assertRegex(events, r"(?m)^  pull_request:")
                    concurrency = section(text, "concurrency")
                    self.assertIn(f"  group: {name}-${{{{ github.event_name }}}}-"
                                  "${{ github.event.pull_request.number || github.sha }}", concurrency)
                    self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
                                  concurrency)
                    self.assertNotRegex(section(text, "jobs"), r"(?m)^\s+concurrency:")
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
            self.assertNotIn("github.sha", group)

            def render(event, ref, number=""):
                context = {"github.event_name": event, "github.ref": ref,
                           "github.event.pull_request.number": number}
                def expression(match):
                    values = [context[key.strip()] for key in match.group(1).split("||")]
                    return next(value for value in values if value)
                return re.sub(r"\$\{\{\s*(.*?)\s*\}\}", expression, group)

            first = render("pull_request", "refs/pull/172/head", "172")
            # A new head/ref spelling on this PR must supersede the old native run.
            self.assertEqual(first, render("pull_request", "refs/pull/172/merge", "172"))
            others = [render("pull_request", "refs/pull/173/head", "173"),
                      render("workflow_dispatch", "refs/heads/main"),
                      render("schedule", "refs/heads/main"),
                      render("push", "refs/heads/main"),
                      render("workflow_dispatch", "refs/heads/hardware-bringup")]
            self.assertEqual(len({first, *others}), 6)


if __name__ == "__main__":
    unittest.main()
