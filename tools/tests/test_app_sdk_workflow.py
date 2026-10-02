"""Contract of .forgejo/workflows/app-sdk.yml."""

from __future__ import annotations

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE = (ROOT / ".forgejo/workflows/app-sdk.yml").read_text()


def block(name: str, indent: int) -> str:
    """The lines of a mapping key at `indent` spaces, up to the next sibling."""
    pad = " " * indent
    match = re.search(rf"^{pad}{re.escape(name)}:\n((?:{pad} .*\n|\s*\n)*)", SOURCE, re.M)
    assert match, name
    return match.group(1)


TRIGGERS = block("on", 0)
JOBS = block("jobs", 0)


def job(name: str) -> str:
    match = re.search(rf"^  {name}:\n((?:    .*\n|\s*\n)*)", JOBS, re.M)
    assert match, name
    return match.group(1)


class AppSdkWorkflow(unittest.TestCase):
    def test_every_pull_request_reports_policy_and_web(self) -> None:
        pull_request = re.search(r"^  pull_request:\n((?:    .*\n)*)", TRIGGERS, re.M).group(1)
        push = re.search(r"^  push:\n((?:    .*\n|      .*\n)*)", TRIGGERS, re.M).group(1)
        self.assertNotIn("paths:", pull_request)
        self.assertIn("paths:", push)
        for name in ("policy", "builder"):
            self.assertIn("    needs: [changes]\n", job(name))
            self.assertIn("    if: needs.changes.outputs.relevant == 'true'\n", job(name))

    def test_jobs_test_the_same_main(self) -> None:
        for name in ("policy", "linux", "web"):
            body = job(name)
            self.assertEqual(body.count("uses: ./.forgejo/actions/merge-main"), 1, name)
            self.assertIn("main: ${{ needs.changes.outputs.main }}", body, name)

    def test_macos_stays_disabled_until_a_runner_serves_it(self) -> None:
        self.assertIn("    if: false\n", job("macos"))


if __name__ == "__main__":
    unittest.main()
