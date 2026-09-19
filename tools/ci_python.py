#!/usr/bin/env python3
"""Run a pinned CI helper with stdlib precedence and fresh bytecode lookup.

Invoke with python3 -I: candidate cwd, PYTHONPATH and user site hooks must never
enter interpreter startup. Local helper imports remain available after stdlib.
"""
import runpy
import sys
import tempfile
from pathlib import Path


def main():
    if not sys.flags.isolated:
        raise SystemExit('ci_python.py requires python3 -I')
    if len(sys.argv) < 2:
        raise SystemExit('usage: python3 -I tools/ci_python.py tools/<helper>.py [args]')
    tools = Path(__file__).resolve().parent
    script = Path(sys.argv[1]).resolve()
    if script.parent != tools or script.suffix != '.py' or not script.is_file():
        raise SystemExit('CI helper must be a Python source file directly in tools')
    # -B alone does not prevent reading candidate-supplied __pycache__ files.
    # Use an empty private cache namespace and prohibit writes for this process.
    with tempfile.TemporaryDirectory(prefix='ci-python-') as cache:
        sys.pycache_prefix = cache
        sys.dont_write_bytecode = True
        sys.path.append(str(tools))
        sys.argv = [str(script), *sys.argv[2:]]
        runpy.run_path(str(script), run_name='__main__')


if __name__ == '__main__':
    main()
