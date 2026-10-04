"""ci-filesystem.yml builds with check.yml's Cargo environment."""

from __future__ import annotations

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"


def cargo_env(block: str, indent: int) -> dict[str, str]:
    """CARGO_* entries of an `env:` mapping at `indent` spaces."""
    pad = " " * indent
    return {
        name: value.strip().strip("'\"")
        for name, value in re.findall(rf"^{pad}(CARGO_[A-Z0-9_]+): (.+)$", block, re.M)
    }


def env_block(source: str, indent: int) -> str:
    pad = " " * indent
    match = re.search(rf"^{pad}env:\n((?:{pad}  .*\n|\s*\n)*)", source, re.M)
    assert match
    return match.group(1)


class FilesystemWorkflowCargoEnv(unittest.TestCase):
    def test_matches_the_x86_test_shards(self) -> None:
        check = (WORKFLOWS / "check.yml").read_text()
        expected = cargo_env(env_block(check, 0), 2)
        test_job = check.split("\n  test:\n", 1)[1].split("\n  check:\n", 1)[0]
        jobs = re.search(r"^      CARGO_BUILD_JOBS: .*'(\d+)' \}\}$", test_job, re.M)
        self.assertIsNotNone(jobs, "test job's x86 CARGO_BUILD_JOBS")
        expected["CARGO_BUILD_JOBS"] = jobs.group(1)

        filesystem = (WORKFLOWS / "ci-filesystem.yml").read_text()
        self.assertEqual(cargo_env(env_block(filesystem, 0), 2), expected)
        jobs_section = filesystem.split("\njobs:\n", 1)[1]
        self.assertNotRegex(jobs_section, r"(?m)^\s+CARGO_[A-Z0-9_]+:")


if __name__ == "__main__":
    unittest.main()
