#!/usr/bin/env python3
"""Explicit-only causal policy profiling; publish numbers, never raw profile/log data."""
import argparse
import contextlib
import cProfile
import io
import json
from pathlib import Path
import pstats
import re
import runpy
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
TARGETS = {
    'app-resource-action': 'tools/policy/check_app_resource_action_contract.py',
    'app-portability': 'tools/policy/check_app_portability.py',
    'policy-tests': None,
}
# Fixed runtime names identify blocked process/pipe work without publishing
# arbitrary function names from generated modules or private fixture paths.
RUNTIME_NAMES = frozenset({
    'wait', '_wait', '_try_wait', '_execute_child', 'communicate', '_communicate',
    'read', 'write', 'open', 'close', 'flush', 'check_output', 'run',
    '<built-in method posix.waitpid>', '<built-in method time.sleep>',
})


def summarize(profiler, root):
    rows = []
    for (filename, line, function), (primitive, calls, own, cumulative, _) in pstats.Stats(profiler).stats.items():
        path = Path(filename)
        if path.is_absolute() and path.is_relative_to(root):
            name = path.relative_to(root).as_posix()
            if not name.startswith(('tools/', 'os/', '.codex/')):
                continue
            category = 'repository'
        elif function in RUNTIME_NAMES:
            name = 'python-runtime'
            category = 'runtime'
            line = 0
        else:
            continue
        rows.append({'category': category, 'file': name, 'line': line, 'function': function,
                     'calls': calls, 'primitive_calls': primitive,
                     'own_seconds': round(own, 6), 'cumulative_seconds': round(cumulative, 6)})
    rows.sort(key=lambda row: (-row['cumulative_seconds'], row['file'], row['line']))
    return rows[:60]


def profile_target(target, root=ROOT):
    previous_profile = sys.getprofile()
    profiler = cProfile.Profile()
    captured = io.StringIO()
    status = 0
    started = time.monotonic()
    old_arguments = sys.argv
    try:
        with contextlib.redirect_stdout(captured), contextlib.redirect_stderr(captured):
            profiler.enable()
            try:
                if target == 'policy-tests':
                    sys.argv = ['unittest', 'discover', '-s', str(root / 'tools/tests'),
                                '-p', 'test_*.py', '--durations', '20']
                    runpy.run_module('unittest', run_name='__main__')
                else:
                    script = root / TARGETS[target]
                    sys.argv = [str(script)]
                    runpy.run_path(str(script), run_name='__main__')
            except SystemExit as error:
                status = error.code if isinstance(error.code, int) else (0 if error.code is None else 1)
            except Exception:
                # The existing required workflow owns detailed failure logs.
                # A diagnostic must fail without publishing private exception data.
                status = 1
            finally:
                profiler.disable()
                if isinstance(previous_profile, cProfile.Profile):
                    previous_profile.enable()
    finally:
        sys.argv = old_arguments
    report = {'target': target, 'status': status,
              'profiled_wall_seconds': round(time.monotonic() - started, 6),
              'functions': summarize(profiler, root)}
    if target == 'policy-tests':
        count = re.search(r'Ran ([0-9]+) tests? in ([0-9.]+)s', captured.getvalue())
        report['tests_run'] = int(count[1]) if count else None
        report['failed_tests'] = [kind + ': ' + method + ' (' + owner + ')'
                                  for kind, method, owner in re.findall(
                                      r'^(FAIL|ERROR): ([A-Za-z0-9_]+) \(([A-Za-z0-9_.]+)\)$',
                                      captured.getvalue(), re.MULTILINE)]
        if not report['tests_run']:
            report['status'] = status = 1
    if not report['functions']:
        report['status'] = status = 1
    return status, report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', choices=TARGETS, required=True)
    arguments = parser.parse_args()
    status, report = profile_target(arguments.target)
    print(json.dumps(report, indent=2))
    return status


if __name__ == '__main__':
    raise SystemExit(main())
