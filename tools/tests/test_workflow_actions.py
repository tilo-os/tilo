from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[1] / "policy/check_workflow_actions.py"
SPEC = importlib.util.spec_from_file_location("check_workflow_actions", MODULE_PATH)
assert SPEC and SPEC.loader
workflow_actions = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = workflow_actions
SPEC.loader.exec_module(workflow_actions)


class WorkflowActionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.workflows = Path(self.temporary.name)

    def write(self, name: str, text: str) -> None:
        path = self.workflows / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_remote_actions_must_be_pinned_to_full_commits_everywhere(self) -> None:
        with self.subTest(phase='full commit remote action is accepted'):
            self.doCleanups()
            self.setUp()
            self.write(
                "quality.yml",
                "steps:\n  - uses: actions/checkout@" + "a" * 40 + " # v4\n",
            )
            self.assertEqual(workflow_actions.audit(self.workflows), [])
        with self.subTest(phase='tags branches and missing revisions are rejected'):
            self.doCleanups()
            self.setUp()
            self.write(
                "quality.yaml",
                "steps:\n"
                "  - uses: actions/checkout@v4\n"
                "  - uses: owner/action@main\n"
                "  - uses: owner/action\n",
            )
            violations = workflow_actions.audit(self.workflows)
            self.assertEqual([item.line for item in violations], [2, 3, 4])
            self.assertEqual(
                [item.reference for item in violations],
                ["actions/checkout@v4", "owner/action@main", "owner/action"],
            )
        with self.subTest(phase='local and docker actions are outside remote pin policy'):
            self.doCleanups()
            self.setUp()
            self.write(
                "build.yml",
                "steps:\n"
                "  - uses: ./.forgejo/actions/setup\n"
                "  - uses: docker://alpine:3.22\n",
            )
            self.assertEqual(workflow_actions.audit(self.workflows), [])
        with self.subTest(phase='nested composite action dependencies are audited'):
            self.doCleanups()
            self.setUp()
            self.write(
                "actions/restore-cache/action.yml",
                "runs:\n"
                "  using: composite\n"
                "  steps:\n"
                "    - uses: actions/cache/restore@v4\n",
            )
            violations = workflow_actions.audit(self.workflows)
            self.assertEqual(len(violations), 1)
            self.assertEqual(violations[0].path.name, "action.yml")
            self.assertEqual(violations[0].reference, "actions/cache/restore@v4")
        with self.subTest(phase='quotes and comments do not hide a mutable reference'):
            self.doCleanups()
            self.setUp()
            self.write(
                "quoted.yml",
                "steps:\n"
                "  - uses: 'owner/action@v1' # mutable\n"
                "  # uses: owner/commented@main\n",
            )
            violations = workflow_actions.audit(self.workflows)
            self.assertEqual(len(violations), 1)
            self.assertEqual(violations[0].reference, "owner/action@v1")


if __name__ == "__main__":
    unittest.main()
