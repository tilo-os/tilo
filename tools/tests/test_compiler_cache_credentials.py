"""Workflows do not bind a compiler-cache bucket or credential.

The runner injects SCCACHE_* and the bucket credential. A workflow that sets
them pins this repository to one CI fleet and overrides the runner.
"""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
FORBIDDEN = re.compile(
    r"secrets\.SCCACHE_|tilo-ci-sccache-|SCCACHE_BUCKET:\s|AWS_ACCESS_KEY_ID:\s*\$\{\{"
)


class CompilerCacheCredentialTests(unittest.TestCase):
    def test_workflows_do_not_bind_the_compiler_cache(self):
        for path in sorted(WORKFLOWS.glob("*.y*ml")):
            with self.subTest(path.name):
                self.assertIsNone(FORBIDDEN.search(path.read_text()), path.name)


if __name__ == "__main__":
    unittest.main()
