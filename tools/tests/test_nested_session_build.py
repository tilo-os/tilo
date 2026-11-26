"""The nested-session build refuses to resolve the env-runtime by name."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MESSAGE = (
    "the env-runtime tree comes from the image-env-runtime-dev-tree check; "
    "run through `t test nested-session` or `t exec --needs nested-session`"
)


class NestedSessionBuildEnvRuntimeTests(unittest.TestCase):
    def test_a_t_job_without_trees_names_the_check_that_publishes_them(self):
        # Without the trees the script used to resolve registry.tilo.so by name,
        # which no check's egress pins: a t job stops before any build instead,
        # and never fetches what its needs did not hand it.
        environment = dict(os.environ)
        environment.pop("TILO_ENV_RUNTIME_TREES", None)
        environment["T_RUN_ID"] = "a-run"
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                ["bash", "dist/dev/nested-session-build.sh", directory],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(MESSAGE, result.stderr)


if __name__ == "__main__":
    unittest.main()
