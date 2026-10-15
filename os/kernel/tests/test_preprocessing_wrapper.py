import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).parent
WRAPPER = Path(os.environ.get("TEST_KERNEL_WRAPPER", HERE.parent / "cc-wrapper.sh"))


class PreprocessingWrapperTests(unittest.TestCase):
    def invoke(self, arguments):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compiler = root / "compiler"
            compiler.write_text("#!/usr/bin/env python3\nimport os,json,sys\n"
                                "open(os.environ['COMPILER_ARGS'],'w').write(json.dumps(sys.argv[1:]))\n"
                                "print('compiler stdout')\nprint('compiler stderr',file=sys.stderr)\n"
                                "sys.exit(17)\n")
            cache = root / "cache"
            cache.write_text("#!/usr/bin/env python3\nimport os,sys\n"
                             "open(os.environ['CACHE_CALLS'],'a').write('called\\n')\n"
                             "os.execv(sys.argv[1],sys.argv[1:])\n")
            compiler.chmod(0o755)
            cache.chmod(0o755)
            env = {**os.environ, "TILO_KERNEL_CC_LAUNCHER": str(cache),
                   "COMPILER_ARGS": str(root / "args.json"), "CACHE_CALLS": str(root / "calls")}
            result = subprocess.run(["sh", str(WRAPPER), str(compiler), *arguments],
                                    env=env, capture_output=True)
            args = json.loads((root / "args.json").read_text())
            calls = (root / "calls").read_text() if (root / "calls").exists() else ""
            self.assertEqual(args, arguments)
            self.assertEqual(result.returncode, 17)
            self.assertEqual(result.stdout, b"compiler stdout\n")
            self.assertEqual(result.stderr, b"compiler stderr\n")
            return calls

    def test_preprocessing_bypasses_uncacheable_launcher_and_preserves_result(self):
        self.assertEqual(self.invoke(["-E", "input.c", "-o", "output.i"]), "")

    def test_compilation_still_requires_and_uses_cache_launcher(self):
        self.assertEqual(self.invoke(["-c", "input.c", "-o", "output.o"]), "called\n")

    def test_embedded_E_is_not_a_preprocessing_flag(self):
        self.assertEqual(self.invoke(["-DMODE=-E", "-c", "input.c"]), "called\n")

    def test_E_option_value_retains_the_cache_launcher(self):
        self.assertEqual(self.invoke(["-include", "-E", "-c", "input.c"]), "called\n")

    def test_option_terminator_retains_the_cache_launcher(self):
        self.assertEqual(self.invoke(["--", "-E"]), "called\n")

    def test_object_compilation_without_launcher_still_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "compiler-was-run"
            compiler = Path(directory) / "compiler"
            compiler.write_text("#!/bin/sh\ntouch '" + str(marker) + "'\n")
            compiler.chmod(0o755)
            env = dict(os.environ)
            env.pop("TILO_KERNEL_CC_LAUNCHER", None)
            result = subprocess.run(["sh", str(WRAPPER), str(compiler), "-c", "input.c"],
                                    env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(marker.exists())

    def test_existing_incbin_signing_and_sbat_bypasses_preserved(self):
        for source in ["certs/system_certificates.S", "arch/x86/boot/compressed/piggy.S",
                       "arch/x86/boot/compressed/sbat.S"]:
            with self.subTest(source=source):
                self.assertEqual(self.invoke(["-c", source, "-o", "output.o"]), "")


if __name__ == "__main__":
    unittest.main()
