import importlib.util
import json
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

from hang import HANG_BOUND


ROOT = Path(__file__).resolve().parents[2]
CI_RUN = ROOT / "scripts" / "ci-run.sh"
spec = importlib.util.spec_from_file_location("cgroup_memory", ROOT / "tools/ci/cgroup_memory.py")
cgroup_memory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cgroup_memory)


def shell_path(path):
    path = Path(path).resolve()
    if os.name != "nt":
        return str(path)
    return f"/mnt/{path.drive[0].lower()}{path.as_posix()[2:]}"


def bash_command(*args):
    # Legacy bash.exe rewrites -c positional arguments. WSL's direct exec
    # preserves the argv used by the same Linux regression on Windows hosts.
    prefix = ["wsl.exe", "--exec", "bash"] if os.name == "nt" else ["bash"]
    return prefix + list(args)


class CiRunSummaryTests(unittest.TestCase):
    def test_immediate_commands_do_not_leave_an_open_heartbeat_pipe(self):
        # Exercise shutdown before the heartbeat necessarily assigns its child
        # PID. Capturing output observes leaked pipe owners, unlike wait alone.
        for attempt in range(30):
            with self.subTest(attempt=attempt):
                result = subprocess.run(
                    bash_command(shell_path(CI_RUN), "immediate", "--", "true"),
                    cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    timeout=HANG_BOUND, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout)

    def run_heartbeat_fixture(self, root, body):
        command = root / "command.sh"
        command.write_text("#!/usr/bin/env bash\n" + body, encoding="utf-8", newline="\n")
        # Capturing output waits for every owner of the pipe, the heartbeat included.
        return subprocess.run(
            bash_command(shell_path(CI_RUN), "heartbeat", "--", "bash", shell_path(command)),
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=HANG_BOUND, check=False,
        )

    def test_completion_stops_the_heartbeat_and_preserves_status(self):
        for status in (0, 7, -15):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                # Give the heartbeat time to reach its wait before finishing.
                finish = 'kill -TERM "$PPID"\nexit 0\n' if status == -15 else f"exit {status}\n"
                result = self.run_heartbeat_fixture(Path(directory), "sleep 0.2\n" + finish)
                # wsl.exe reports the terminating Linux signal as a positive code;
                # native subprocess reports it as a negative return code.
                expected = 15 if os.name == "nt" and status == -15 else status
                self.assertEqual(result.returncode, expected, result.stdout)

    def test_a_process_the_command_leaves_behind_does_not_keep_the_heartbeat(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            leftover = root / "leftover.pid"
            try:
                # The leftover outlives the hang bound, so a heartbeat that
                # waited for it would time the run out.
                result = self.run_heartbeat_fixture(
                    root,
                    f"sleep {int(HANG_BOUND) * 2} >/dev/null 2>&1 &\n"
                    f"echo $! > {shlex.quote(shell_path(leftover))}\nexit 0\n",
                )
                self.assertEqual(result.returncode, 0, result.stdout)
            finally:
                if leftover.exists():
                    subprocess.run(
                        bash_command("-c", 'kill -TERM "$1" 2>/dev/null || true',
                                     "fixture", leftover.read_text().strip()), check=False,
                    )


class CiRunMemoryTests(unittest.TestCase):
    def run_fixture(self, directory, command, status=23):
        result = subprocess.run(
            bash_command("-c", 'export TILO_CGROUP_MEMORY_PATH="$1"; exec bash "$2" memory -- bash -c "$3"',
                         "fixture", shell_path(directory), shell_path(CI_RUN), command),
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=HANG_BOUND, check=False,
        )
        self.assertEqual(result.returncode, status, result.stdout)
        return result.stdout

    def receipt(self, output, phase):
        prefix = f"CI_CGROUP_MEMORY_{phase}="
        lines = [line.removeprefix(prefix) for line in output.splitlines() if line.startswith(prefix)]
        self.assertEqual(len(lines), 1, output)
        return json.loads(lines[0])

    def put_memory(self, root):
        for name, value in {"max": "4294967296", "current": "1024", "peak": "2048",
                            "events": "low 0\nhigh 0\nmax 7\noom 3\noom_kill 2\n"}.items():
            (root / f"memory.{name}").write_text(value)

    def test_memory_evidence_claims_oom_only_from_observed_counter_deltas(self):
        with self.subTest(phase='failed command reports observed oom delta and preserves status'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.put_memory(root)
                output = self.run_fixture(root,
                    'printf "4096\\n" > "$TILO_CGROUP_MEMORY_PATH/memory.current"; '
                    'printf "8192\\n" > "$TILO_CGROUP_MEMORY_PATH/memory.peak"; '
                    'printf "low 0\\nhigh 0\\nmax 9\\noom 4\\noom_kill 3\\n" '
                    '> "$TILO_CGROUP_MEMORY_PATH/memory.events"; exit 23')
                before = self.receipt(output, "BEFORE")
                after = self.receipt(output, "AFTER")
                self.assertEqual(before["memory.max"], "4294967296")
                self.assertEqual(before["memory.current"], "1024")
                self.assertEqual(after["memory.current"], "4096")
                self.assertEqual(after["memory.peak"], "8192")
                self.assertEqual(before["memory.events"]["oom_kill"], 2)
                self.assertEqual(after["memory.events"]["oom_kill"], 3)
                self.assertEqual(self.receipt(output, "EVENTS_DELTA"), {"max": 2, "oom": 1, "oom_kill": 1})
        with self.subTest(phase='missing cgroup is nonfatal and does not claim oom'):
            with tempfile.TemporaryDirectory() as directory:
                output = self.run_fixture(Path(directory) / "missing", "exit 23")
                self.assertIsNone(self.receipt(output, "BEFORE")["memory.events"])
                self.assertIsNone(self.receipt(output, "AFTER")["memory.events"])
                self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", output)
        with self.subTest(phase='unchanged missing or reset counters do not claim oom'):
            for state in ("unchanged", "missing-before", "missing-after", "reset", "replaced"):
                with self.subTest(state=state), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory) / "cgroup"
                    root.mkdir()
                    self.put_memory(root)
                    command = "exit 23"
                    if state == "missing-before":
                        (root / "memory.events").unlink()
                        command = 'printf "oom 3\\noom_kill 2\\n" > "$TILO_CGROUP_MEMORY_PATH/memory.events"; exit 23'
                    elif state == "missing-after":
                        command = 'rm "$TILO_CGROUP_MEMORY_PATH/memory.events"; exit 23'
                    elif state == "reset":
                        command = 'printf "oom 1\\noom_kill 0\\n" > "$TILO_CGROUP_MEMORY_PATH/memory.events"; exit 23'
                    elif state == "replaced":
                        command = ('mv "$TILO_CGROUP_MEMORY_PATH" "$TILO_CGROUP_MEMORY_PATH.old"; '
                                   'mkdir "$TILO_CGROUP_MEMORY_PATH"; '
                                   'printf "oom 4\\noom_kill 3\\n" > "$TILO_CGROUP_MEMORY_PATH/memory.events"; exit 23')
                    output = self.run_fixture(root, command)
                    self.receipt(output, "BEFORE")
                    self.receipt(output, "AFTER")
                    self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", output)
        with self.subTest(phase='success does not emit failure memory evidence'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.put_memory(root)
                output = self.run_fixture(root, "exit 0", status=0)
                self.assertNotIn("CI_CGROUP_MEMORY_", output)
        with self.subTest(phase='unreadable and malformed fields remain unknown'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.put_memory(root)
                (root / "memory.max").write_text("max")
                (root / "memory.current").write_text("invalid")
                (root / "memory.peak").unlink()
                (root / "memory.peak").mkdir()
                (root / "memory.events").write_text("oom invalid")
                output = self.run_fixture(root, "exit 23")
                for phase in ("BEFORE", "AFTER"):
                    receipt = self.receipt(output, phase)
                    self.assertEqual(receipt["memory.max"], "max")
                    for field in ("memory.current", "memory.peak", "memory.events"):
                        self.assertIsNone(receipt[field])
                self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", output)
        with self.subTest(phase='diagnostic helper failure preserves command status'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                python = root / "python3"
                python.write_text("#!/bin/sh\nexit 91\n")
                python.chmod(0o755)
                result = subprocess.run(
                    bash_command("-c", 'export PATH="$1:$PATH"; exec bash "$2" memory -- bash -c "exit 23"',
                                 "fixture", shell_path(root), shell_path(CI_RUN)),
                    cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    timeout=HANG_BOUND, check=False,
                )
                self.assertEqual(result.returncode, 23, result.stdout)
                self.assertIn("CI_CGROUP_MEMORY=unavailable", result.stdout)
                self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", result.stdout)


class CgroupResolutionTests(unittest.TestCase):
    def test_cgroup_path_resolution_respects_mount_roots_and_unmapped_groups(self):
        with self.subTest(phase='cgroup2 mount root and escaped mountpoint are respected'):
            for membership, mount_root, mountpoint, expected in (
                ("/", "/", "/sys/fs/cgroup", "/sys/fs/cgroup"),
                ("/job/build", "/", "/sys/fs/cgroup", "/sys/fs/cgroup/job/build"),
                ("/job/build", "/job", r"/cgroup\040mount", "/cgroup mount/build"),
            ):
                with self.subTest(membership=membership, mount_root=mount_root):
                    mountinfo = f"26 22 0:23 {mount_root} {mountpoint} rw - cgroup2 cgroup rw"
                    self.assertEqual(cgroup_memory.resolve_cgroup(f"0::{membership}\n", mountinfo), Path(expected))
        with self.subTest(phase='missing or unmapped cgroup is not confused with mount root'):
            mountinfo = "26 22 0:23 /other /sys/fs/cgroup rw - cgroup2 cgroup rw"
            for membership in ("0::/job\n", "0::/../job\n", "1:memory:/job\n", ""):
                with self.subTest(membership=membership):
                    self.assertIsNone(cgroup_memory.resolve_cgroup(membership, mountinfo))


if __name__ == "__main__":
    unittest.main()
