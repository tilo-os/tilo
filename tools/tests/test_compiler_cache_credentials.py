"""Checks carry no compiler-cache bucket or credential.

t gives every job its compiler cache. A check of t.toml that names a bucket or
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
    def test_checks_do_not_bind_a_compiler_cache(self):
        self.assertIsNone(FORBIDDEN.search((ROOT / "t.toml").read_text()))


if __name__ == "__main__":
    unittest.main()
