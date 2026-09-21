import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("builder_plan", ROOT / "tools/ci/builder_workflow_plan.py")
plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(plan)


def workflow(cold="echo cold", build="echo build"):
    return f"""name: fixture
jobs:
  build:
    needs: signing-preflight
    runs-on: runner
    steps:
      - name: Build
        run: {build}
  cache-cold:
    runs-on: runner
    steps:
      - run: {cold}
  cache-warm:
    runs-on: runner
    steps:
      - run: echo warm
"""


class BuilderPlanTests(unittest.TestCase):
    def test_pr97_qualification_change_reuses_promoted_builder(self):
        changed = [plan.WORKFLOW, "tools/ci/affected_packages.py",
                   "tools/tests/test_cargo_download_cache_action.py", "tools/tests/test_ci_select.py"]
        self.assertEqual(plan.choose("push", changed, workflow(), workflow("echo seed cache"))[0], "reuse")

    def test_every_image_and_publication_input_rebuilds(self):
        for path in sorted(plan.REBUILD_PATHS) + [".forgejo/actions/install-cosign/action.yml"]:
            with self.subTest(path=path):
                self.assertEqual(plan.choose("push", [path])[0], "build")

    def test_replication_helper_rebuilds_for_landing(self):
        mode, reason = plan.choose("push", ["os/dev/replicate-builder-index.py"])
        self.assertEqual(mode, "build")
        self.assertIn("replicate-builder-index.py", reason)

    def test_aws_cli_installation_change_runs_publication(self):
        mode, reason = plan.choose("push", ["os/dev/install-aws-cli.sh"])
        self.assertEqual(mode, "build")
        self.assertIn("install-aws-cli.sh", reason)

    def test_native_build_contract_change_rebuilds(self):
        self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), workflow(build="echo changed"))[0], "build")

    def test_admission_only_build_job_wiring_does_not_change_contract(self):
        head = workflow().replace("    needs: signing-preflight\n", "    needs: [plan, signing-preflight]\n    if: needs.plan.outputs.mode == 'build'\n")
        self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), head)[0], "reuse")

    def test_refresh_and_unknown_events_rebuild(self):
        for event in ("schedule", "workflow_dispatch", "pull_request", ""):
            with self.subTest(event=event):
                self.assertEqual(plan.choose(event, ["docs/change.md"])[0], "build")

    def test_malformed_workflow_fails_closed(self):
        self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), "jobs: {}" )[0], "build")


if __name__ == "__main__":
    unittest.main()
