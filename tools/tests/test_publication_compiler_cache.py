"""No remote compiler cache or credential reaches a kernel build.

A signed kernel compiled through a shared cache could carry planted compiler
output; the kernel's cache is compiled from empty by a check of the same run.
"""
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class KernelPublicationTests(unittest.TestCase):
    def test_no_remote_cache_or_credential_reaches_a_kernel_build(self):
        for path in ("tools/ci/publish/kernel.sh", "tools/ci/publish/kernel-env.sh",
                     "tools/ci/publish/kernel-compile-cache.sh", "os/kernel/Containerfile",
                     "os/kernel/build-support.sh"):
            text = (ROOT / path).read_text()
            for forbidden in ("--secret", "ACTIONS_CACHE_URL", "ACTIONS_RUNTIME_TOKEN", "SCCACHE_",
                              "CCACHE_REMOTE", "remote_storage", "aws_access_key_id",
                              "compiler_cache_env.sh", "--cache-from"):
                with self.subTest(path=path, forbidden=forbidden):
                    self.assertNotIn(forbidden, text)


if __name__ == "__main__":
    unittest.main()
