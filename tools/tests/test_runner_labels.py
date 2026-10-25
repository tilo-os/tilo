import importlib.util
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('runner_labels', ROOT / 'tools/ci/runner_labels.py')
runner_labels = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner_labels)

RUNS_ON = re.compile(r'^\s*runs-on:\s*(.+)$', re.MULTILINE)
MATRIX_RUNNER = re.compile(r'^\s*-?\s*(?:runner|build_runner):\s*([A-Za-z0-9_-]+)\s*$', re.MULTILINE)
JSON_RUNNER = re.compile(r'"(?:runner|build_runner)"\s*:\s*"([^"]+)"')
QUOTED = re.compile(r"'([^']+)'")


def markdown_tables(text):
    """Each Markdown table as its rows of trimmed cells, the separator row left out."""
    tables, rows = [], None
    for line in text.splitlines() + ['']:
        line = line.strip()
        if not line.startswith('|'):
            if rows:
                tables.append(rows)
            rows = None
            continue
        cells = [cell.strip() for cell in line.strip('|').split('|')]
        rows = rows or []
        if not all(set(cell) <= set('-: ') for cell in cells):
            rows.append(cells)
    return tables


def workflow_labels():
    for path in sorted((ROOT / '.forgejo/workflows').glob('*.yml')):
        text = path.read_text()
        for value in RUNS_ON.findall(text):
            value = value.split(' #', 1)[0].strip()
            if value.startswith('${{'):
                # Expressions compare other strings too; their labels are quoted.
                yield from ((path.name, label) for label in QUOTED.findall(value)
                            if label.startswith(('runner', 'imager-')))
            else:
                yield path.name, value.strip('\'"')
        for label in MATRIX_RUNNER.findall(text) + JSON_RUNNER.findall(text):
            yield path.name, label


class RunnerLabels(unittest.TestCase):
    def test_every_workflow_label_is_documented(self):
        seen = set()
        for workflow, label in workflow_labels():
            with self.subTest(workflow=workflow, label=label):
                self.assertIn(label, runner_labels.LABELS, 'add it to tools/ci/runner_labels.py')
            seen.add(label)
        self.assertIn('runner', seen)

    def test_readme_table_matches_the_module(self):
        # Cell by cell: a Markdown formatter may pad the columns.
        (generated,) = markdown_tables(runner_labels.table())
        readme = markdown_tables((ROOT / 'tools/ci/README.md').read_text())
        self.assertIn(generated, readme, 'regenerate with python3 tools/ci/runner_labels.py')

    def test_matrix_constants_name_documented_labels(self):
        for mapping in (runner_labels.NATIVE, runner_labels.LARGE):
            self.assertEqual(set(mapping), {'x86_64', 'aarch64'})
            for arch, label in mapping.items():
                self.assertEqual(runner_labels.LABELS[label].arch, arch)

    def test_ci_helpers_take_labels_from_the_module(self):
        for relative in ('tools/apps/app_release.py',):
            source = (ROOT / relative).read_text()
            with self.subTest(helper=relative):
                self.assertIn('runner_labels', source)
                for label in runner_labels.LABELS:
                    self.assertIsNone(re.search(r'["\']' + re.escape(label) + r'["\'](?!\s*:)', source),
                                      label)


if __name__ == '__main__':
    unittest.main()
