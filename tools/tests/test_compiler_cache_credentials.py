"""Workflows do not pin a compiler-cache bucket.

The runner injects SCCACHE_* when it has them. tools/ci/compiler_cache_env.sh
fills this repository's bucket only when that set is incomplete. Workflows
pass the secret through; they do not name the bucket.
"""
from pathlib import Path
import re
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


if __name__ == "__main__":
    unittest.main()
