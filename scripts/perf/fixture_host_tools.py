"""A stand-in `ps`, so collector tests do not depend on the host's procps."""
from pathlib import Path
import os

PROCESS_LISTING = '    PID %CPU COMMAND\n      1  0.0 fixture-init\n'


def fixture_path(root):
    """Return PATH with a directory holding a fixture `ps` first."""
    tools = Path(root) / 'fixture-bin'
    tools.mkdir()
    ps = tools / 'ps'
    ps.write_text("#!/bin/sh\ncat <<'EOF'\n" + PROCESS_LISTING + 'EOF\n')
    ps.chmod(0o755)
    return str(tools) + os.pathsep + os.environ.get('PATH', os.defpath)
