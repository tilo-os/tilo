import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CI_RUN = ROOT / "scripts" / "ci-run.sh"


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


if __name__ == "__main__":
    unittest.main()
