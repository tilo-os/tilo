#!/usr/bin/env python3
"""No Chromium subprocess aborts while the host sits idle.

Chromium's zygote gives each process it forks a fresh stack canary. The
children exit by returning through the host's main(); while that frame carried
a canary (the host builds with -fstack-protector-strong) every exiting child
died with "*** stack smashing detected ***", from about a minute after start.
Runs a real host idle for `--seconds` (past that point) and fails on any such
abort in its stderr.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('host', type=Path)
parser.add_argument('--seconds', type=int, default=90)
args = parser.parse_args()

with tempfile.TemporaryDirectory(prefix='tilo-cef-stable-') as profile, tempfile.TemporaryFile() as log:
    host = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile, '--ozone-platform=headless', '--disable-gpu'],
                            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=log)
    try:
        time.sleep(args.seconds)
        assert host.poll() is None, 'the host exited'
    finally:
        host.kill()
        host.wait()
        log.seek(0)
        diagnostics = log.read().decode(errors='replace')
    aborts = diagnostics.count('stack smashing')
    assert aborts == 0, '%d subprocess aborts:\n%s' % (aborts, diagnostics[-2000:])
    print('PASS: no subprocess aborted in %ds' % args.seconds)
