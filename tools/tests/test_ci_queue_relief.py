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
    return (ROOT / ".github/workflows" / (name + ".yml")).read_text(encoding="utf-8")


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
    def test_builder_resolution_keeps_native_context_fixture_in_control_lane(self):
        builder = section(workflow("check"), "builder", 2)
        self.assertIn("runs-on: blacksmith-4vcpu-ubuntu-2404", builder)
        self.assertNotIn("    container:", builder)
        self.assertIn("image: ${{ steps.resolve.outputs.image }}", builder)
        self.assertIn('DOCKER_BUILDKIT: "1"', builder)
        self.assertIn("run: bash os/tests/named_public_trust_context.sh docker", builder)

    def test_fast_gates_use_blacksmith_runners(self):
        quality = section(workflow("lint"), "quality", 2)
        self.assertNotIn("    container:", quality)
        for name in ("select", "checks"):
            with self.subTest(job=name):
                job = section(workflow("check"), name, 2)
                self.assertNotIn("    container:", job)
                self.assertIn("runs-on: blacksmith-4vcpu-ubuntu-2404", job)

    def test_fast_checks_never_cancel_another_commit_or_event(self):
        for name in ("check", "lint"):
            with self.subTest(workflow=name):
                text = workflow(name)
                events = section(text, "on")
                self.assertRegex(events, r"(?m)^  push:")
                self.assertRegex(events, r"(?m)^  pull_request:")
                concurrency = section(text, "concurrency")
                self.assertIn("github.event_name", concurrency)
                self.assertIn("github.sha", concurrency)
                self.assertIn("cancel-in-progress: false", concurrency)
                self.assertNotRegex(section(text, "jobs"), r"(?m)^\s+concurrency:")

    def test_component_release_tags_and_manual_runs_remain(self):
        for name, tags in (("build-image", '["v*"]'),
                           ("package-desktop", '["desktop-v*"]'),
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

    def test_os_full_build_is_not_a_pr_gate(self):
        events = section(workflow("build-image"), "on")
        self.assertNotRegex(events, r"(?m)^  pull_request(?:_target)?:")
        self.assertRegex(events, r"(?m)^  schedule:")

    def test_app_and_desktop_prs_keep_preflight_without_packaging(self):
        for name, plan, build in (("release-apps", "plan", "build"),
                                  ("package-desktop", "validate", "package")):
            with self.subTest(workflow=name):
                text = workflow(name)
                self.assertRegex(section(text, "on"), r"(?m)^  pull_request:")
                self.assertIn("steps:", section(text, plan, 2))
                self.assertRegex(section(text, build, 2),
                                 r"if: .*github.event_name != 'pull_request'")

    def test_specialized_contract_checks_keep_pr_and_periodic_coverage(self):
        for name in ("legacy-app-e2e", "fs-fuse"):
            with self.subTest(workflow=name):
                events = section(workflow(name), "on")
                self.assertNotRegex(events, r"(?m)^  push:")
                for event in ("pull_request", "schedule", "workflow_dispatch"):
                    self.assertRegex(events, rf"(?m)^  {event}:")
                self.assertTrue(section(workflow(name), "jobs"))

    def test_builder_recipe_does_not_run_e2e_against_the_old_image(self):
        legacy_events = section(workflow("legacy-app-e2e"), "on")
        self.assertNotIn('os/Containerfile.builder', legacy_events)
        self.assertIn('os/Containerfile.builder', section(workflow("build-builder-image"), "on"))

    def test_os_candidate_can_run_after_skipped_signing_preflight(self):
        build = section(workflow("build-image"), "build-and-push", 2)
        self.assertIn("always()", build)
        self.assertIn("needs.plan.result == 'success'", build)
        self.assertIn("needs.signing-preflight.result == 'skipped'", build)


if __name__ == "__main__":
    unittest.main()
