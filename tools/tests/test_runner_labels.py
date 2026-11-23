import importlib.util
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('runner_labels', ROOT / 'tools/ci/runner_labels.py')
runner_labels = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner_labels)



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


class RunnerLabels(unittest.TestCase):
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
        for relative in ('tools/apps/app_release.py', 'tools/ci/affected_packages.py'):
            source = (ROOT / relative).read_text()
            with self.subTest(helper=relative):
                self.assertIn('runner_labels', source)
                for label in runner_labels.LABELS:
                    self.assertIsNone(re.search(r'["\']' + re.escape(label) + r'["\'](?!\s*:)', source),
                                      label)


if __name__ == '__main__':
    unittest.main()
