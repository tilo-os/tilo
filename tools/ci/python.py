#!/usr/bin/env python3
"""Run a pinned CI helper with stdlib precedence and fresh bytecode lookup.

Invoke with python3 -I: candidate cwd, PYTHONPATH and user site hooks must never
enter interpreter startup. Local helper imports remain available after stdlib.
"""
import runpy
from importlib.machinery import PathFinder
import sys
import tempfile
from pathlib import Path


class StandardLibraryOnly:
    """Absent platform-specific stdlib modules must not fall back to tools."""
    def __init__(self, trusted_paths):
        self.trusted_paths = list(trusted_paths)

    def find_spec(self, fullname, path=None, target=None):
        if fullname.partition('.')[0] not in sys.stdlib_module_names:
            return None
        spec = PathFinder.find_spec(fullname, self.trusted_paths if path is None else path, target)
        if spec is None:
            raise ModuleNotFoundError('standard-library module unavailable: ' + fullname, name=fullname)
        return spec


def main():
    if not sys.flags.isolated:
        raise SystemExit('ci/python.py requires python3 -I')
    if len(sys.argv) < 2:
        raise SystemExit('usage: python3 -I tools/ci/python.py tools/<helper>.py [args]')
    tools = Path(__file__).resolve().parents[1]
    script = Path(sys.argv[1]).resolve()
    if script.parent not in (tools, tools / 'ci') or script.suffix != '.py' or not script.is_file():
        raise SystemExit('CI helper must be a Python source file in tools or tools/ci')
    # -B alone does not prevent reading candidate-supplied __pycache__ files.
    # Use an empty private cache namespace and prohibit writes for this process.
    with tempfile.TemporaryDirectory(prefix='ci-python-') as cache:
        sys.pycache_prefix = cache
        sys.dont_write_bytecode = True
        # Builtin/Frozen importers keep precedence. In particular, subprocess
        # probes msvcrt on Linux: absence must stay ImportError, not tools/msvcrt.
        sys.meta_path.insert(sys.meta_path.index(PathFinder), StandardLibraryOnly(sys.path))
        sys.path.append(str(tools))
        sys.argv = [str(script), *sys.argv[2:]]
        runpy.run_path(str(script), run_name='__main__')


if __name__ == '__main__':
    main()
