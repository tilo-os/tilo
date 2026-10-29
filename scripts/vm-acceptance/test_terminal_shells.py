"""Finding Terminal's shells and where its `~` writes, run against real processes."""
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from acceptance import core


class LocalVm:
    def __init__(self, env=None):
        self.env = env

    def exec(self, script):
        return subprocess.run(["sh", "-c", script], capture_output=True, text=True,
                              env=self.env, check=True).stdout


class Ctx:
    def __init__(self, env=None):
        self.vm = LocalVm(env)


class TerminalShells(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        # A process whose name is tilo-ush, as the runtime's are.
        self.ush = Path(self.dir.name) / "tilo-ush"
        self.ush.symlink_to("/bin/bash")

    def spawn(self, script, *argv):
        proc = subprocess.Popen([str(self.ush), "-c", script, *argv])
        self.addCleanup(proc.wait)
        self.addCleanup(proc.kill)
        return proc

    def wait_children(self, pid):
        for _ in range(100):
            children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
            if children:
                return [int(c) for c in children]
            time.sleep(0.05)
        self.fail("the session's process started no child")

    def test_ush_shell_is_the_session_process(self):
        ush = self.spawn("sleep 60; :", "tilo-ush", "run", "--terminal-fd", "3",
                         "--shell", "ush", "--host-root", "/")
        self.wait_children(ush.pid)
        self.assertIn(ush.pid, core.terminal_shells(Ctx()))

    def test_linux_shell_is_the_bash_under_the_session(self):
        linux = self.spawn("bash -c 'sleep 60; :'; :", "tilo-ush", "run", "--terminal-fd", "3",
                           "--shell", "linux", "--host-root", "/")
        (bash,) = self.wait_children(linux.pid)
        shells = core.terminal_shells(Ctx())
        self.assertIn(bash, shells)
        self.assertNotIn(linux.pid, shells)

    def test_probe_finds_a_write_to_the_workspace_folder(self):
        workspace = Path(self.dir.name) / "workspace"
        workspace.mkdir()
        (workspace / "acc-term-1").write_text("ok\n")
        env = dict(os.environ, TILO_WORKSPACE_ROOT=str(workspace), HOME=self.dir.name)
        self.assertEqual(core.terminal_probe(Ctx(env), "acc-term-1"), [str(workspace / "acc-term-1")])


if __name__ == "__main__":
    unittest.main()
