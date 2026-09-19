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
    def test_builder_resolution_keeps_native_context_fixture_in_control_lane(self):
        builder = section(workflow("check"), "builder", 2)
        self.assertIn("runs-on: runner-quality", builder)
        container = section(builder, "container", 4)
        self.assertRegex(container, r"image: code\.podesta\.ai/qos/ci-control@sha256:[0-9a-f]{64}")
        self.assertIn("password: ${{ secrets.REGISTRY_TOKEN }}", container)
        self.assertIn("image: ${{ steps.resolve.outputs.image }}", builder)
        self.assertIn('DOCKER_BUILDKIT: "1"', builder)
        self.assertIn("run: bash os/tests/named_public_trust_context.sh docker", builder)

    def test_final_gates_use_qualified_lightweight_image(self):
        quality = section(workflow("lint"), "quality", 2)
        expected = section(quality, "container", 4)
        self.assertRegex(expected, r"image: code\.podesta\.ai/qos/ci-fast@sha256:[0-9a-f]{64}")
        for name in ("select", "checks"):
            with self.subTest(job=name):
                job = section(workflow("check"), name, 2)
                self.assertEqual(section(job, "container", 4), expected)
                self.assertIn("runs-on: runner-quality", job)

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
        for name in ("legacy-app-e2e",):
            with self.subTest(workflow=name):
                events = section(workflow(name), "on")
                self.assertNotRegex(events, r"(?m)^  push:")
                for event in ("pull_request", "schedule", "workflow_dispatch"):
                    self.assertRegex(events, rf"(?m)^  {event}:")
                self.assertTrue(section(workflow(name), "jobs"))

        filesystem = section(workflow("fs-fuse"), "on")
        self.assertRegex(filesystem, r"(?m)^  pull_request_target:")
        self.assertNotRegex(filesystem, r"(?m)^  (?:schedule|workflow_dispatch):")
        executor = section(workflow("fs-fuse-executor"), "on")
        self.assertNotRegex(executor, r"(?m)^  pull_request:")
        for event in ("schedule", "workflow_dispatch"):
            self.assertRegex(executor, rf"(?m)^  {event}:")

    def test_filesystem_selection_runs_before_the_exclusive_lane(self):
        parent = workflow("fs-fuse")
        executor = workflow("fs-fuse-executor")
        self.assertIn("group: fs-fuse-pr-${{ github.event.pull_request.number }}",
                      section(parent, "concurrency"))
        self.assertIn("cancel-in-progress: true", section(parent, "concurrency"))
        self.assertIn("group: fs-fuse", section(executor, "concurrency"))
        self.assertIn("cancel-in-progress: false", section(executor, "concurrency"))

        applicability = section(parent, "applicability", 2)
        self.assertIn("runs-on: runner-quality", applicability)
        self.assertIn("fetch-depth: 2", applicability)
        self.assertIn("tools/ci/prepare_pr_history.py", applicability)
        contract = section(parent, "contract", 2)
        self.assertIn("needs: applicability", contract)
        self.assertIn("needs.applicability.result == 'success'", contract)
        self.assertIn("needs.applicability.outputs.applicable == 'true'", contract)
        self.assertIn("python3 -I tools/ci/run_fuse_contract.py", contract)

    def test_filesystem_executor_keeps_exact_admission_and_both_suites(self):
        executor = workflow("fs-fuse-executor")
        admission = section(executor, "admission", 2)
        self.assertIn('[[ $SOURCE_SHA =~ ^[0-9a-f]{40}$ ]]', admission)
        self.assertIn('test "$SOURCE_SHA" = "$GITHUB_SHA"', admission)
        for name in ("real-fuse", "files-ux"):
            with self.subTest(job=name):
                job = section(executor, name, 2)
                self.assertIn("needs: admission", job)
                self.assertIn("needs.admission.result == 'success'", job)
                self.assertIn("ref: ${{ github.event.inputs.source_sha || github.sha }}", job)

    def test_filesystem_pr_trigger_covers_orchestrator_inputs(self):
        events = section(workflow("fs-fuse"), "on")
        pull = section(events, "pull_request_target", 2)
        for path in (".forgejo/workflows/fs-fuse.yml",
                     ".forgejo/workflows/fs-fuse-executor.yml",
                     "tools/ci/run_fuse_contract.py",
                     "tools/ci/workflow_applicability.py",
                     "tools/ci/prepare_pr_history.py"):
            self.assertIn(f'      - "{path}"', pull)

    def test_filesystem_dispatch_token_never_enters_pr_controlled_code(self):
        parent = workflow("fs-fuse")
        self.assertNotRegex(section(parent, "on"), r"(?m)^  pull_request:")
        self.assertIn("ref: ${{ github.event.pull_request.base.sha }}", parent)
        self.assertNotIn("FORGEJO_TOKEN: ${{ secrets.REGISTRY_TOKEN }}", parent)
        self.assertEqual(parent.count("secrets.FUSE_CONTRACT_TOKEN"), 1)
        contract = section(parent, "contract", 2)
        self.assertLess(contract.index("ref: ${{ github.event.pull_request.base.sha }}"),
                        contract.index("FORGEJO_TOKEN: ${{ secrets.FUSE_CONTRACT_TOKEN }}"))

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
