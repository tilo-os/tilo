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
    def test_builder_plan_rebuilds_for_every_contract_input_and_fails_closed(self):
        with self.subTest(phase='pr97 qualification change reuses promoted builder'):
            changed = [plan.WORKFLOW, "tools/ci/suites.py",
                       "tools/tests/test_cargo_download_cache_action.py", "tools/tests/test_suites.py"]
            self.assertEqual(plan.choose("push", changed, workflow(), workflow("echo seed cache"))[0], "reuse")
        with self.subTest(phase='every image and publication input rebuilds'):
            for path in sorted(plan.REBUILD_PATHS) + [".forgejo/actions/install-cosign/action.yml"]:
                with self.subTest(path=path):
                    self.assertEqual(plan.choose("push", [path])[0], "build")
        with self.subTest(phase='aws cli installation change runs publication'):
            mode, reason = plan.choose("push", ["os/dev/install-aws-cli.sh"])
            self.assertEqual(mode, "build")
            self.assertIn("install-aws-cli.sh", reason)
        with self.subTest(phase='native build contract change rebuilds'):
            self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), workflow(build="echo changed"))[0], "build")
        with self.subTest(phase='admission only build job wiring does not change contract'):
            head = workflow().replace("    needs: signing-preflight\n", "    needs: [plan, signing-preflight]\n    if: needs.plan.outputs.mode == 'build'\n")
            self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), head)[0], "reuse")
        with self.subTest(phase='refresh and unknown events rebuild'):
            for event in ("schedule", "workflow_dispatch", "pull_request", ""):
                with self.subTest(event=event):
                    self.assertEqual(plan.choose(event, ["docs/change.md"])[0], "build")
        with self.subTest(phase='malformed workflow fails closed'):
            self.assertEqual(plan.choose("push", [plan.WORKFLOW], workflow(), "jobs: {}" )[0], "build")


if __name__ == "__main__":
    unittest.main()
