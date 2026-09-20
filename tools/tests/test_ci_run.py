import os
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


class CiRunSummaryTests(unittest.TestCase):
    def test_clippy_diff_failure_keeps_lint_location_and_name(self):
        diagnostic = """introduced 1 Clippy diagnostic site(s):
warning: item in documentation is missing backticks
 --> crates/packaging/pkg/src/version.rs:2:38
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
                [
                    "bash",
                    shell_path(CI_RUN),
                    "Clippy diagnostic diff",
                    "--",
                    "bash",
                    shell_path(command),
                ],
                cwd=ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("First diagnostics", result.stdout)
            self.assertNotIn("No recognised diagnostic matched", result.stdout)
            self.assertIn("crates/packaging/pkg/src/version.rs:2:38", result.stdout)
            self.assertIn("clippy::doc-markdown", result.stdout)
            self.assertIn("::error::Clippy diagnostic diff: warning:", result.stdout)
            rendered_summary = summary.read_text(encoding="utf-8")
            self.assertIn("crates/packaging/pkg/src/version.rs:2:38", rendered_summary)
            self.assertIn("clippy::doc-markdown", rendered_summary)


if __name__ == "__main__":
    unittest.main()
