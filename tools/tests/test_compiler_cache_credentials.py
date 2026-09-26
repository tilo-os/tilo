"""The compiler cache key comes from this repository's secrets.

The CI runners serve every repository on the forge. A cache key in their
environment would let any repository's job write objects that tilo builds
then compile from, so the runners carry none: each workflow that compiles
through the cache binds it at workflow level from repository secrets.
"""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
USES_CACHE = re.compile(
    r"sccache|SCCACHE_|AWS_ACCESS_KEY_ID|prepare-rust-build|setup-windows-arm64-cross")
BINDING = {
    "SCCACHE_BUCKET": "tilo-ci-sccache-125650147312-us-west-2",
    "SCCACHE_REGION": "us-west-2",
    "SCCACHE_ENDPOINT": "https://s3.us-west-2.amazonaws.com",
    "SCCACHE_S3_USE_SSL": '"true"',
    "SCCACHE_S3_KEY_PREFIX": "forgejo-runners/v1",
    "AWS_ACCESS_KEY_ID": "${{ secrets.SCCACHE_AWS_ACCESS_KEY_ID }}",
    "AWS_SECRET_ACCESS_KEY": "${{ secrets.SCCACHE_AWS_SECRET_ACCESS_KEY }}",
}


def workflow_env(text: str) -> dict[str, str]:
    match = re.search(r"^env:\n((?:(?:  .*|#.*)?\n)*)", text, re.M)
    if not match:
        return {}
    return dict(re.findall(r"^  ([A-Z0-9_]+): (.+)$", match.group(1), re.M))


class CompilerCacheCredentialTests(unittest.TestCase):
    def workflows(self):
        return sorted(WORKFLOWS.glob("*.y*ml"))

    def test_every_workflow_using_the_cache_binds_it_from_repository_secrets(self):
        users = [path for path in self.workflows() if USES_CACHE.search(path.read_text())]
        self.assertGreaterEqual(len(users), 15)
        for path in users:
            with self.subTest(path.name):
                env = workflow_env(path.read_text())
                for name, value in BINDING.items():
                    self.assertEqual(env.get(name), value, name)

    def test_the_key_is_bound_only_from_the_cache_secrets(self):
        for path in self.workflows():
            for line in path.read_text().splitlines():
                match = re.match(r"\s+(AWS_(?:ACCESS_KEY_ID|SECRET_ACCESS_KEY)):\s*(.+)", line)
                if match:
                    with self.subTest(path.name, line=line.strip()):
                        self.assertEqual(match.group(2), BINDING[match.group(1)])


if __name__ == "__main__":
    unittest.main()
