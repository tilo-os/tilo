import importlib.util
import json
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path


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
                    timeout=10, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout)

    def test_completion_reaps_heartbeat_sleeper_and_preserves_status(self):
        for status in (0, 7, -15):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                sleeper = root / "sleeper.pid"
                binary = root / "bin"
                binary.mkdir()
                shim = binary / "sleep"
                # Record only the heartbeat sleeper, then retain its exact PID.
                shim.write_text(
                    "#!/usr/bin/env bash\n"
                    f'if [ "$1" = 60 ]; then echo $$ > {shlex.quote(shell_path(sleeper))}; fi\n'
                    'exec /bin/sleep "$@"\n', encoding="utf-8", newline="\n"
                )
                shim.chmod(0o755)
                command = root / "command.sh"
                finish = 'kill -TERM "$PPID"\nexit 0\n' if status == -15 else f"exit {status}\n"
                command.write_text(
                    "#!/usr/bin/env bash\n"
                    f"for attempt in {{1..200}}; do [ -s {shlex.quote(shell_path(sleeper))} ] && break; sleep 0.01; done\n"
                    f"test -s {shlex.quote(shell_path(sleeper))} || exit 99\n"
                    + finish, encoding="utf-8", newline="\n"
                )
                try:
                    result = subprocess.run(
                        bash_command("-c", 'export PATH="$1:$PATH"; exec bash "$2" heartbeat -- bash "$3"',
                                     "fixture", shell_path(binary), shell_path(CI_RUN), shell_path(command)),
                        cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        timeout=10, check=False,
                    )
                    # wsl.exe reports the terminating Linux signal as a positive code;
                    # native subprocess reports it as a negative return code.
                    expected = 15 if os.name == "nt" and status == -15 else status
                    self.assertEqual(result.returncode, expected, result.stdout)
                    pid = sleeper.read_text().strip()
                    reaped = subprocess.run(
                        bash_command("-c", 'test ! -e "/proc/$1"', "fixture", pid),
                        check=False,
                    )
                    self.assertEqual(reaped.returncode, 0, "heartbeat must reap its owned sleep")
                finally:
                    # Keep a regressed wrapper from leaking its recorded child.
                    # This runs after the reaping assertion, never in its place.
                    if sleeper.exists():
                        subprocess.run(
                            bash_command("-c", 'kill -TERM "$1" 2>/dev/null || true',
                                         "fixture", sleeper.read_text().strip()), check=False,
                        )

    def test_clippy_diff_failure_keeps_lint_location_and_name(self):
        diagnostic = """introduced 1 Clippy diagnostic site(s):
warning: item in documentation is missing backticks
 --> crates/packaging/tilo-app-package/src/version.rs:2:38
  |
  = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.97.0/index.html#doc_markdown
  = note: `-D clippy::doc-markdown` implied by `-D clippy::pedantic`
"""
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            command = directory / "fail.sh"
            summary = directory / "summary.md"
            command.write_bytes(
                ("#!/usr/bin/env bash\ncat <<'EOF'\n" + diagnostic + "EOF\nexit 1\n").encode()
            )
            command.chmod(0o755)
            env = os.environ | {"GITHUB_STEP_SUMMARY": shell_path(summary)}
            if os.name == "nt":
                inherited = env.get("WSLENV", "")
                env["WSLENV"] = ":".join(filter(None, [inherited, "GITHUB_STEP_SUMMARY"]))
            result = subprocess.run(
                bash_command(
                    shell_path(CI_RUN),
                    "Clippy diagnostic diff",
                    "--",
                    "bash",
                    shell_path(command),
                ),
                cwd=ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
                timeout=10,
            )

            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("First diagnostics", result.stdout)
            self.assertNotIn("No recognised diagnostic matched", result.stdout)
            self.assertIn("crates/packaging/tilo-app-package/src/version.rs:2:38", result.stdout)
            self.assertIn("clippy::doc-markdown", result.stdout)
            self.assertIn("::error::Clippy diagnostic diff: warning:", result.stdout)
            rendered_summary = summary.read_text(encoding="utf-8")
            self.assertIn("crates/packaging/tilo-app-package/src/version.rs:2:38", rendered_summary)
            self.assertIn("clippy::doc-markdown", rendered_summary)


class CiRunMemoryTests(unittest.TestCase):
    def run_fixture(self, directory, command, status=23):
        result = subprocess.run(
            bash_command("-c", 'export TILO_CGROUP_MEMORY_PATH="$1"; exec bash "$2" memory -- bash -c "$3"',
                         "fixture", shell_path(directory), shell_path(CI_RUN), command),
            cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=10, check=False,
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

    def test_failed_command_reports_observed_oom_delta_and_preserves_status(self):
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

    def test_missing_cgroup_is_nonfatal_and_does_not_claim_oom(self):
        with tempfile.TemporaryDirectory() as directory:
            output = self.run_fixture(Path(directory) / "missing", "exit 23")
            self.assertIsNone(self.receipt(output, "BEFORE")["memory.events"])
            self.assertIsNone(self.receipt(output, "AFTER")["memory.events"])
            self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", output)

    def test_unchanged_missing_or_reset_counters_do_not_claim_oom(self):
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

    def test_success_does_not_emit_failure_memory_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.put_memory(root)
            output = self.run_fixture(root, "exit 0", status=0)
            self.assertNotIn("CI_CGROUP_MEMORY_", output)

    def test_unreadable_and_malformed_fields_remain_unknown(self):
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

    def test_diagnostic_helper_failure_preserves_command_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            python = root / "python3"
            python.write_text("#!/bin/sh\nexit 91\n")
            python.chmod(0o755)
            result = subprocess.run(
                bash_command("-c", 'export PATH="$1:$PATH"; exec bash "$2" memory -- bash -c "exit 23"',
                             "fixture", shell_path(root), shell_path(CI_RUN)),
                cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                timeout=10, check=False,
            )
            self.assertEqual(result.returncode, 23, result.stdout)
            self.assertIn("CI_CGROUP_MEMORY=unavailable", result.stdout)
            self.assertNotIn("CI_CGROUP_MEMORY_EVENTS_DELTA=", result.stdout)


class CgroupResolutionTests(unittest.TestCase):
    def test_cgroup2_mount_root_and_escaped_mountpoint_are_respected(self):
        for membership, mount_root, mountpoint, expected in (
            ("/", "/", "/sys/fs/cgroup", "/sys/fs/cgroup"),
            ("/job/build", "/", "/sys/fs/cgroup", "/sys/fs/cgroup/job/build"),
            ("/job/build", "/job", r"/cgroup\040mount", "/cgroup mount/build"),
        ):
            with self.subTest(membership=membership, mount_root=mount_root):
                mountinfo = f"26 22 0:23 {mount_root} {mountpoint} rw - cgroup2 cgroup rw"
                self.assertEqual(cgroup_memory.resolve_cgroup(f"0::{membership}\n", mountinfo), Path(expected))

    def test_missing_or_unmapped_cgroup_is_not_confused_with_mount_root(self):
        mountinfo = "26 22 0:23 /other /sys/fs/cgroup rw - cgroup2 cgroup rw"
        for membership in ("0::/job\n", "0::/../job\n", "1:memory:/job\n", ""):
            with self.subTest(membership=membership):
                self.assertIsNone(cgroup_memory.resolve_cgroup(membership, mountinfo))


if __name__ == "__main__":
    unittest.main()
