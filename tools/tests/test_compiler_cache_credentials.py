"""Workflows carry no compiler-cache bucket or credential.

The runners give every job its compiler cache (sccache over the job's
Actions cache, infra docs/ci-caching.md). A workflow that names a bucket or
binds an S3 key would bypass the per-repository scoping.
"""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
FORBIDDEN = re.compile(
    r"tilo-ci-sccache-|SCCACHE_BUCKET|SCCACHE_AWS|FALLBACK_AWS|compiler_cache_env"
)


class CompilerCacheCredentialTests(unittest.TestCase):
    def test_workflows_do_not_bind_a_compiler_cache(self):
        paths = sorted((ROOT / ".forgejo").rglob("*.y*ml"))
        self.assertTrue(paths)
        for path in paths:
            with self.subTest(path.relative_to(ROOT)):
                self.assertIsNone(FORBIDDEN.search(path.read_text()))


if __name__ == "__main__":
    unittest.main()
