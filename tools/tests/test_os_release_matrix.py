from pathlib import Path
import json
import os
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"


def matrix_script() -> str:
    workflow = WORKFLOW.read_text()
    marker = "      - name: Select validation or native publication lanes\n"
    block = workflow.split(marker, 1)[1].split("\n\n  signing-preflight:", 1)[0]
    return textwrap.dedent(block.split("        run: |\n", 1)[1])


SHA = "0123456789abcdef0123456789abcdef01234567"


def plan(
    ref: str,
    event: str,
    requested: str,
    published: str = "",
    configured: str = "true",
    integration: str = "",
) -> tuple[bool, bool, list[dict[str, str]]]:
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "output"
        environment = os.environ.copy()
        environment.update(
            {
                "GITHUB_REF": ref,
                "GITHUB_EVENT_NAME": event,
                "GITHUB_OUTPUT": str(output),
                "GITHUB_SHA": SHA,
                "REQUESTED_PUBLISH": requested,
                "PUBLISHED_REVISION": published,
                "PUBLICATION_CONFIGURED": configured,
                "INTEGRATION_REVISION": integration,
            }
        )
        subprocess.run(["bash", "-c", matrix_script()], env=environment, check=True)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return (
            values["publish"] == "true",
            values["skip"] == "true",
            json.loads(values["build-matrix"])["include"],
        )


def select_matrix(ref: str, event: str, requested: str) -> tuple[bool, list[dict[str, str]]]:
    publish, skip, lanes = plan(ref, event, requested)
    assert not skip
    return publish, lanes


class OsReleaseMatrixTests(unittest.TestCase):
    def test_version_tags_keep_the_dedicated_release_fleet(self):
        publish, lanes = select_matrix("refs/tags/v1.2.3", "push", "false")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "imager-linux-x86_64"},
                {"arch": "aarch64", "runner": "imager-linux-aarch64"},
            ],
        )

    def test_protected_manual_main_publish_uses_native_large_disk_fleet(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "true")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "runner-disk-100g"},
                {"arch": "aarch64", "runner": "runner-disk-100g-arm64"},
            ],
        )

    def test_manual_nonpublication_remains_a_single_native_smoke_lane(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "false")
        self.assertFalse(publish)
        self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-disk-100g"}])

    def test_nightly_main_publishes_through_the_manual_main_fleet(self):
        publish, skip, lanes = plan("refs/heads/main", "schedule", "", "f" * 40)
        self.assertTrue(publish)
        self.assertFalse(skip)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "runner-disk-100g"},
                {"arch": "aarch64", "runner": "runner-disk-100g-arm64"},
            ],
        )

    def test_nightly_publishes_when_no_published_revision_is_readable(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "")
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_nightly_skips_the_build_when_main_is_already_published(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", SHA)
        self.assertFalse(publish)
        self.assertTrue(skip)

    def test_schedule_never_publishes_from_another_ref(self):
        publish, skip, lanes = plan("refs/heads/feature", "schedule", "", "")
        self.assertFalse(publish)
        self.assertFalse(skip)
        self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-disk-100g"}])

    def test_unconfigured_nightly_runs_a_main_integration_build(self):
        publish, skip, lanes = plan("refs/heads/main", "schedule", "", "", "false")
        self.assertFalse(publish)
        self.assertFalse(skip)
        self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-disk-100g"}])

    def test_unconfigured_nightly_skips_an_already_built_integration_image(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "false", SHA)
        self.assertFalse(publish)
        self.assertTrue(skip)

    def test_configured_nightly_publishes_despite_an_integration_image(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "true", SHA)
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_explicit_publication_ignores_the_published_revision(self):
        publish, skip, _ = plan("refs/heads/main", "workflow_dispatch", "true", SHA)
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_build_and_publication_jobs_honour_the_nightly_skip(self):
        workflow = WORKFLOW.read_text()
        build = workflow.split("\n  build-and-push:\n", 1)[1].split("\n    strategy:", 1)[0]
        self.assertIn("needs.plan.outputs.skip != 'true'", build)
        self.assertIn("PLANNED_PUBLISH: ${{ needs.plan.outputs.publish }}", workflow)
        self.assertIn("docker://code.podesta.ai/qos/tilo:latest", workflow)
        self.assertIn('.Labels["org.opencontainers.image.revision"]', workflow)

    def test_every_publishing_run_holds_the_generation_series_lock(self):
        cases = [
            ("refs/tags/v1.2.3", "push", ""),
            ("refs/heads/main", "workflow_dispatch", "true"),
            ("refs/heads/main", "workflow_dispatch", "false"),
            ("refs/heads/feature", "workflow_dispatch", "true"),
            ("refs/heads/main", "schedule", ""),
            ("refs/heads/feature", "schedule", ""),
            ("refs/heads/main", "push", ""),
        ]
        for ref, event, requested in cases:
            with self.subTest(ref=ref, event=event, requested=requested):
                # A scheduled main run that skips must still hold the lock, so
                # judge the lock against the unskipped rule.
                publish, _, _ = plan(ref, event, requested)
                group = concurrency_group(ref, event, requested)
                if publish:
                    self.assertEqual(group, "tilo-os-image-series")
                else:
                    self.assertEqual(group, f"tilo-os-image-validate-{ref}")


def concurrency_group(ref: str, event: str, requested: str) -> str:
    """Evaluate the workflow's concurrency expression for one event."""
    line = next(
        line for line in WORKFLOW.read_text().splitlines() if line.startswith("  group: ${{")
    )
    expression = line.split("${{", 1)[1].rsplit("}}", 1)[0]
    for source, target in [
        ("startsWith(github.ref, 'refs/tags/v')", "ref.startswith('refs/tags/v')"),
        ("format('tilo-os-image-validate-{0}', github.ref)", "('tilo-os-image-validate-' + ref)"),
        ("github.event.inputs.publish", "requested"),
        ("github.event_name", "event"),
        ("github.ref", "ref"),
        ("&&", " and "),
        ("||", " or "),
    ]:
        expression = expression.replace(source, target)
    return eval(expression, {}, {"ref": ref, "event": event, "requested": requested})


if __name__ == "__main__":
    unittest.main()
