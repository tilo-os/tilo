"""Regression checks for the temporary, reversible CI capacity policy.

These inspect the repository's block-style workflow sections, not a general YAML
parser. Live scheduler behavior is qualified separately; this suite protects the
trigger and job boundaries that caused the main-push build storm.
"""
from pathlib import Path
import re
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
import t_matrix  # noqa: E402


def workflow(name):
    return (ROOT / ".forgejo/workflows" / (name + ".yml")).read_text(encoding="utf-8")


def section(text, key, indent=0):
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines)
                 if line == " " * indent + key + ":")
    result = []
    for line in lines[start + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


class QueueReliefTests(unittest.TestCase):
    def test_pr_gates_stay_light_while_contract_coverage_is_kept(self):
        with self.subTest(phase='desktop prs keep preflight without packaging'):
            text = workflow("release-desktop")
            self.assertRegex(section(text, "on"), r"(?m)^  pull_request:")
            self.assertIn("steps:", section(text, "validate", 2))
            self.assertRegex(section(text, "package", 2),
                             r"if: .*github.event_name != 'pull_request'")
        with self.subTest(phase='app prs start no release jobs and never package'):
            text = workflow("release-apps")
            self.assertNotRegex(section(text, "on"), r"(?m)^  pull_request(?:_target)?:")
            self.assertRegex(section(text, "build", 2),
                             r"if: .*github.event_name != 'pull_request'")
        checks = {check["name"]: check for check in t_matrix.checks(ROOT)}
        with self.subTest(phase='specialized contract checks keep periodic and on-demand coverage'):
            for name in ("legacy-apps", "cloud-servers"):
                with self.subTest(check=name):
                    check = checks[name]
                    self.assertIs(check.get("default", True), False)
                    self.assertIn("dispatch", check["on"])
                    self.assertTrue(any(on.startswith("schedule:") for on in check["on"]), check["on"])
            # The cloud servers gate release candidates; the legacy apps run nightly and on demand.
            self.assertIn("stable", checks["cloud-servers"]["on"])
            self.assertNotIn("main", checks["legacy-apps"]["on"])
        with self.subTest(phase='builder recipe does not run e2e against the old image'):
            self.assertNotIn('os/Containerfile.builder', checks["legacy-apps"]["inputs"])
            for arch in ("x86_64", "aarch64"):
                self.assertIn('os/Containerfile.builder', checks[f"image-ci-builder-{arch}"]["inputs"])

    def test_component_release_tags_and_manual_runs_remain(self):
        with self.subTest(phase='component release tags and manual runs remain'):
            for name, tags in (("release-desktop", '["desktop-v*"]'),
                               ("release-apps", '["apps-v*", "app-*-v*"]')):
                with self.subTest(workflow=name):
                    events = section(workflow(name), "on")
                    push = section(events, "push", 2)
                    self.assertIn("tags: " + tags, push)
                    if name == "release-apps":
                        self.assertRegex(push, r"(?m)^    branches: \[main\]$")
                    else:
                        self.assertNotRegex(push, r"(?m)^    branches(?:-ignore)?:")
                    self.assertRegex(events, r"(?m)^  workflow_dispatch:")


if __name__ == "__main__":
    unittest.main()
