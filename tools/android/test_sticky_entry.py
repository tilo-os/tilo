#!/usr/bin/env python3
"""Run the compiled production C supervisor harness; this is not ART evidence."""
import argparse
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('harness', type=Path)
    args = parser.parse_args()
    binary = args.harness.resolve(strict=True)
    cases = [('normal', 0, ['initial']), ('term', 143, ['initial']),
             ('empty', 137, ['initial']), ('malformed', 2, ['initial']),
             ('recover', 0, ['initial', 'recovered']),
             ('loop', 137, ['initial', 'recovered', 'recovered', 'recovered'])]
    with tempfile.TemporaryDirectory(prefix='android-entry-tests-') as root:
        for mode, expected, workers in cases:
            counter = Path(root) / mode
            result = subprocess.run([str(binary), mode, str(counter)], timeout=12,
                                    text=True, capture_output=True)
            if result.returncode != expected or counter.read_text().splitlines() != workers:
                raise RuntimeError('production supervisor case failed: ' + mode + '\n' + result.stderr)
            print(mode + ': passed')


if __name__ == '__main__':
    main()
