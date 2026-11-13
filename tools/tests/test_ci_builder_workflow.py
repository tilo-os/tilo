"""The CI builder is published whenever an input of its tag changes."""
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.forgejo/workflows/build-ci-builder.yml'


def inputs():
    listed = subprocess.run(['bash', str(ROOT / 'tools/ci/publish/ci-builder.sh'), 'inputs'],
                            capture_output=True, text=True, check=True).stdout.split()
    return {path + '/**' if (ROOT / path).is_dir() else path for path in listed}


def triggers():
    text = WORKFLOW.read_text()
    block = text[text.index('    paths:\n'):text.index('  workflow_dispatch:')]
    return set(re.findall(r'^      - "([^"]+)"$', block, re.M))


class CiBuilderWorkflowTests(unittest.TestCase):
    def test_every_input_of_the_tag_triggers_a_publication(self):
        covered = {path for path in inputs()
                   if path in triggers() or any(trigger.endswith('/**') and path.startswith(trigger[:-2])
                                                for trigger in triggers())}
        self.assertEqual(inputs() - covered, set())

    def test_the_workflow_triggers_on_nothing_else(self):
        self.assertEqual(triggers() - inputs(), set())


if __name__ == '__main__':
    unittest.main()
