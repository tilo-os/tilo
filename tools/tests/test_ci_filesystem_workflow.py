"""Contract of .forgejo/workflows/ci-filesystem.yml."""

from __future__ import annotations

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"


class FilesystemWorkflowTriggers(unittest.TestCase):
    def test_pushes_to_main_run_the_pull_request_selection(self) -> None:
        source = (WORKFLOWS / "ci-filesystem.yml").read_text()
        triggers = source.split("\non:\n", 1)[1].split("\nconcurrency:", 1)[0]

        def paths(event: str) -> list[str]:
            body = re.search(rf"^  {event}:\n((?:    .*\n|      .*\n)*)", triggers, re.M)
            self.assertIsNotNone(body, event)
            return re.findall(r'^      - "(.+)"$', body.group(1), re.M)

        self.assertIn("crates/system/fs/**", paths("push"))
        self.assertEqual(paths("push"), paths("pull_request"))


if __name__ == "__main__":
    unittest.main()
