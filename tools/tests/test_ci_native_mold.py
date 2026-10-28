import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from hang import HANG_BOUND


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
WRAPPER = ROOT / "tools" / "ci" / "native-mold-linker.sh"


class NativeMoldLinkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.binary_directory = Path(self.temporary.name)
        self.environment = dict(
            os.environ,
            PATH=f"{self.binary_directory}:/usr/bin:/bin",
            TILO_CI_MOLD_TARGET="aarch64-unknown-linux-gnu",
        )
        self.environment.pop("TILO_CI_MOLD_THREADS", None)
        self.tool("uname", '[ "$1" = -s ] && echo Linux || echo aarch64')
        self.tool("mold", "exit 0")
        self.tool(
            "gcc",
            'if [ "$1" = -dumpmachine ]; then '
            'echo aarch64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi',
        )

    def tool(self, name: str, body: str) -> None:
        path = self.binary_directory / name
        path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
        path.chmod(0o755)

    def run_wrapper(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(WRAPPER), *arguments],
            env=self.environment,
            text=True,
            capture_output=True,
            timeout=HANG_BOUND,
        )

    def test_native_mold_wrapper_links_only_explicit_native_targets_with_hardening(self) -> None:
        with self.subTest(phase='preserves hardening and spaced arguments'):
            self.doCleanups()
            self.setUp()
            arguments = [
                "object with spaces.o",
                "-Wl,-z,relro,-z,now",
                "-Wl,-z,noexecstack",
            ]
            result = self.run_wrapper(*arguments)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout.splitlines(),
                arguments + ["-fuse-ld=mold", "-Wl,--threads=2"],
            )
        with self.subTest(phase='preserves x86 shadow stack flag'):
            self.doCleanups()
            self.setUp()
            self.tool("uname", '[ "$1" = -s ] && echo Linux || echo x86_64')
            self.tool(
                "gcc",
                'if [ "$1" = -dumpmachine ]; then '
                'echo x86_64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi',
            )
            self.environment["TILO_CI_MOLD_TARGET"] = "x86_64-unknown-linux-gnu"
            result = self.run_wrapper("-Wl,-z,shstk")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("-Wl,-z,shstk", result.stdout.splitlines())
        with self.subTest(phase='requires an explicit native gnu target'):
            self.doCleanups()
            self.setUp()
            self.environment.pop("TILO_CI_MOLD_TARGET")
            self.assertEqual(self.run_wrapper().returncode, 2)
            for target in (
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-musl",
            ):
                self.environment["TILO_CI_MOLD_TARGET"] = target
                self.assertEqual(self.run_wrapper().returncode, 2)
        with self.subTest(phase='rejects a cross compiler'):
            self.doCleanups()
            self.setUp()
            self.tool("gcc", "echo x86_64-redhat-linux-gnu")
            self.assertEqual(self.run_wrapper().returncode, 2)
        with self.subTest(phase='accepts fedora driver tuple'):
            self.doCleanups()
            self.setUp()
            self.tool(
                "gcc",
                'if [ "$1" = -dumpmachine ]; then '
                'echo aarch64-redhat-linux; else printf "%s\\n" "$@"; fi',
            )
            self.assertEqual(self.run_wrapper().returncode, 0)
        with self.subTest(phase='bounds linker threads'):
            self.doCleanups()
            self.setUp()
            for value in ("0", "33", "-1", "two", "2;echo bad"):
                self.environment["TILO_CI_MOLD_THREADS"] = value
                self.assertEqual(self.run_wrapper().returncode, 2)
            self.environment["TILO_CI_MOLD_THREADS"] = "4"
            result = self.run_wrapper()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("-Wl,--threads=4", result.stdout.splitlines())


if __name__ == "__main__":
    unittest.main()
