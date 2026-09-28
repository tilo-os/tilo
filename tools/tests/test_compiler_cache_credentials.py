"""Workflows do not pin a compiler-cache bucket.

The runner injects SCCACHE_* when it has them. tools/ci/compiler_cache_env.sh
fills this repository's bucket only when that set is incomplete. Workflows
pass the secret through; they do not name the bucket.
"""
from pathlib import Path
import os
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
FORBIDDEN = re.compile(
    r"tilo-ci-sccache-|SCCACHE_BUCKET:\s|(?<!FALLBACK_)AWS_ACCESS_KEY_ID:\s*\$\{\{"
)


class CompilerCacheCredentialTests(unittest.TestCase):
    def test_workflows_do_not_bind_the_compiler_cache(self):
        for path in sorted(WORKFLOWS.glob("*.y*ml")):
            with self.subTest(path.name):
                self.assertIsNone(FORBIDDEN.search(path.read_text()), path.name)

    def test_repository_cache_lives_in_one_script(self):
        script = (ROOT / "tools/ci/compiler_cache_env.sh").read_text()
        self.assertIn("tilo-ci-sccache-125650147312-us-west-2", script)
        self.assertIn("FALLBACK_AWS_ACCESS_KEY_ID", script)

    def test_sourcing_leaves_the_callers_variables_alone(self):
        # setup-windows-arm64-cross reads its own $name after sourcing.
        environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("SCCACHE_", "AWS_", "FALLBACK_", "GITHUB_"))
        }
        environment.update(FALLBACK_AWS_ACCESS_KEY_ID="id", FALLBACK_AWS_SECRET_ACCESS_KEY="secret")
        result = subprocess.run(
            ["bash", "-c", 'name=client value=kept ready=kept; source "$1" >/dev/null; '
             'printf "%s %s %s %s %s\\n" "$name" "$value" "$ready" "$SCCACHE_BUCKET" '
             '"$(type -t tilo_runner_compiler_cache_complete || echo none)"',
             "bash", str(ROOT / "tools/ci/compiler_cache_env.sh")],
            env=environment, check=True, capture_output=True, text=True,
        )
        self.assertEqual(
            result.stdout.split(),
            ["client", "kept", "kept", "tilo-ci-sccache-125650147312-us-west-2", "none"],
        )


if __name__ == "__main__":
    unittest.main()
