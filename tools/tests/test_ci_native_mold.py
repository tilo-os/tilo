"""Run on Linux: python3 -m unittest discover -s tools/tests -p test_ci_native_mold.py."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


WRAPPER = Path(__file__).resolve().parents[1] / "ci-native-mold.sh"


class NativeMoldTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.bin = Path(self.temp.name)
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin",
                        TILO_CI_MOLD_TARGET="aarch64-unknown-linux-gnu")
        self.env.pop("TILO_CI_MOLD_THREADS", None)
        self.tool("uname", '[ "$1" = -s ] && echo Linux || echo aarch64')
        self.tool("mold", "exit 0")
        self.tool("gcc", 'if [ "$1" = -dumpmachine ]; then echo aarch64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi')

    def tool(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_wrapper(self, *args):
        return subprocess.run(["bash", str(WRAPPER), *args], env=self.env,
                              text=True, capture_output=True, timeout=5)

    def test_preserves_hardening_and_spaced_object_argument(self):
        args = ["object with spaces.o", "-Wl,-z,relro,-z,now", "-Wl,-z,noexecstack"]
        result = self.run_wrapper(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), args + ["-fuse-ld=mold", "-Wl,--threads=2"])

    def test_native_x86_preserves_shadow_stack_flag(self):
        self.tool("uname", '[ "$1" = -s ] && echo Linux || echo x86_64')
        self.tool("gcc", 'if [ "$1" = -dumpmachine ]; then echo x86_64-redhat-linux-gnu; else printf "%s\\n" "$@"; fi')
        self.env["TILO_CI_MOLD_TARGET"] = "x86_64-unknown-linux-gnu"
        result = self.run_wrapper("-Wl,-z,shstk")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-Wl,-z,shstk", result.stdout.splitlines())

    def test_requires_explicit_target(self):
        self.env.pop("TILO_CI_MOLD_TARGET")
        self.assertEqual(self.run_wrapper().returncode, 2)

    def test_rejects_cross_target_and_musl(self):
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-musl"]:
            self.env["TILO_CI_MOLD_TARGET"] = target
            self.assertEqual(self.run_wrapper().returncode, 2)

    def test_rejects_cross_compiler(self):
        self.tool("gcc", "echo x86_64-redhat-linux-gnu")
        self.assertEqual(self.run_wrapper().returncode, 2)

    def test_accepts_fedora_driver_tuple(self):
        self.tool("gcc", 'if [ "$1" = -dumpmachine ]; then echo aarch64-redhat-linux; else printf "%s\\n" "$@"; fi')
        self.assertEqual(self.run_wrapper().returncode, 0)

    def test_thread_budget(self):
        for value in ["0", "33", "-1", "two", "2;echo bad"]:
            self.env["TILO_CI_MOLD_THREADS"] = value
            self.assertEqual(self.run_wrapper().returncode, 2)
        self.env["TILO_CI_MOLD_THREADS"] = "4"
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-Wl,--threads=4", result.stdout.splitlines())


if __name__ == "__main__":
    unittest.main()
