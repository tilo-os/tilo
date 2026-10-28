"""Keep shallow-history preparation free of background Git maintenance."""
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
JOBS = (("release-apps.yml", "plan"),)


class CiGitMaintenanceTests(unittest.TestCase):
    def preparation(self, workflow, job):
        source = (ROOT / ".forgejo/workflows" / workflow).read_text()
        body = source.split(f"\n  {job}:\n", 1)[1]
        body = re.split(r"\n  [a-zA-Z0-9_-]+:\n", body, maxsplit=1)[0]
        self.assertIn("\n    container:\n", body)
        settings, steps = body.split("\n    steps:\n", 1)
        self.assertNotIn("GIT_CONFIG_", settings)
        first, checkout = steps.split("\n      - ", 1)
        self.assertNotIn("if:", first)
        command = first.split("        run: ", 1)[-1].strip()
        self.assertEqual(
            shlex.split(command),
            ["git", "config", "--global", "maintenance.auto", "false"],
            "disable maintenance inside the disposable container before checkout",
        )
        self.assertIn(
            "uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
            checkout.split("\n      - ", 1)[0],
        )
        return command

    def test_history_jobs_disable_maintenance_before_checkout(self):
        for workflow, job in JOBS:
            with self.subTest(workflow=workflow, job=job):
                self.preparation(workflow, job)

    def test_setting_survives_config_copy_and_command_scoped_auth(self):
        for workflow, job in JOBS:
            with self.subTest(workflow=workflow, job=job), tempfile.TemporaryDirectory() as tmp:
                command = self.preparation(workflow, job)
                config = Path(tmp) / "job.gitconfig"
                environment = {
                    key: value for key, value in os.environ.items()
                    if not key.startswith("GIT_")
                }
                environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(config))
                subprocess.run(shlex.split(command), cwd=tmp, env=environment, check=True)
                # The pinned checkout copies the job's global config before its first fetch.
                copied = Path(tmp) / "checkout.gitconfig"
                shutil.copyfile(config, copied)
                for active in (config, copied):
                    environment.update(
                        GIT_CONFIG_GLOBAL=str(active),
                        GIT_CONFIG_COUNT="1",
                        GIT_CONFIG_KEY_0="http.https://code.tilo.so/.extraheader",
                        GIT_CONFIG_VALUE_0="test-only-header",
                    )
                    for key, expected in (
                        ("maintenance.auto", "false"),
                        (environment["GIT_CONFIG_KEY_0"], "test-only-header"),
                    ):
                        result = subprocess.run(
                            ["git", "config", "--get", key], cwd=tmp, env=environment,
                            check=True, capture_output=True, text=True,
                        )
                        self.assertEqual(result.stdout.strip(), expected)
                self.assertNotIn("test-only-header", config.read_text())
                self.assertNotIn("test-only-header", copied.read_text())


if __name__ == "__main__":
    unittest.main()
