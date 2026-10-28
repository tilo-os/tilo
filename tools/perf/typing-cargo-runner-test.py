#!/usr/bin/env python3
"""Runner wiring tests with fake tools, not benchmark or counter evidence."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(
            prefix="typing-runner-test-", dir=os.environ.get("TMPDIR")
        )
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = dict(os.environ)
        self.env.pop("CARGO_TARGET_DIR", None)
        self.env["PATH"] = str(self.root / "bin") + os.pathsep + self.env["PATH"]
        self.env["TEST_INVOCATIONS"] = str(self.root / "calls.jsonl")
        for name in ["typing-cargo-runner.sh", "typing-iai.sh"]:
            destination = self.root / "tools/perf" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(REPO / "tools/perf" / name, destination)
        for name in [
            "Cargo.toml", "Cargo.lock", "apps/code-editor/Cargo.toml",
            "apps/code-editor/benches/typing.rs", "apps/code-editor/src/app.rs",
            "apps/code-editor/src/editor_view.rs", "apps/code-editor/src/editor_scroll.rs",
            "crates/sdk/ui-core/src/text_render/cache.rs",
            "target/iai-valgrind-fixed/build-inputs.txt",
            "target/iai-valgrind-fixed/install/libexec/valgrind/callgrind-amd64-linux",
        ]:
            self.put(name, "fixture\n")
        self.put("bin/cargo", """#!/bin/sh
[ "$*" = 'install --list --root '"$PWD/target/iai-runner" ] || exit 99
printf 'iai-callgrind-runner v0.16.1:\\n'
""", executable=True)
        self.put("bin/rustc", "#!/bin/sh\necho fixture-rustc\n", executable=True)
        self.put("target/iai-runner/bin/iai-callgrind-runner", "fixture", executable=True)
        self.put("target/iai-valgrind-fixed/install/bin/valgrind",
                 "#!/bin/sh\necho valgrind-3.27.1\n", executable=True)
        self.put("tools/perf/build-typing-tools.sh", "#!/bin/sh\nexit 0\n")
        self.put("tools/cargo-runner.sh", '#!/bin/sh\nexec "$@"\n', executable=True)
        self.put("tools/font-catalog.py", """import os, pathlib, sys
if sys.argv[1] == 'root':
    print(pathlib.Path('fonts').resolve())
elif sys.argv[1] == 'stage':
    pathlib.Path(sys.argv[3]).mkdir(parents=True)
elif sys.argv[1] == 'verify':
    sys.exit(int(os.environ.get('FONT_FAILURE', '0')))
""")
        self.put("tools/perf/typing-iai-check-test.py", "pass\n")
        self.put("tools/perf/typing-iai-check.py", "from pathlib import Path\nPath('audited').touch()\n")
        self.binary = self.root / "target/release/deps/typing-fixture"
        self.put(str(self.binary.relative_to(self.root)), """#!/usr/bin/env python3
import json, os, pathlib, sys
with pathlib.Path(os.environ['TEST_INVOCATIONS']).open('a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\\n')
if '--iai-run' in sys.argv:
    sys.exit(int(os.environ.get('FIXTURE_FAILURE', '0')))
print('{}')
""", executable=True)
        self.put("target/iai-valgrind-fixed/install/libexec/valgrind/callgrind-amd64-linux",
                 "fixture", executable=True)

    def put(self, name, contents, executable=False):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        if executable:
            path.chmod(0o755)

    def run_fixture(self, directory=None):
        return subprocess.run(
            ["bash", str(self.root / "tools/perf/typing-cargo-runner.sh"),
             str(self.binary), "--bench"],
            cwd=directory or self.root, env=self.env, text=True,
            capture_output=True, check=False,
        )

    def calls(self):
        return [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def test_packages_cargo_executable_without_rebuilding_and_checks_all_cases(self):
        result = self.run_fixture()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 5)
        self.assertEqual([call[2:4] for call in calls[:4]],
                         [["0", "0"], ["0", "1"], ["1", "0"], ["1", "1"]])
        self.assertEqual(calls[4].count("--bench"), 1)
        self.assertIn("--allow-aslr=true", calls[4])
        self.assertIn("--save-summary=json", calls[4])
        self.assertTrue((self.root / "audited").exists())
        packaged = self.root / "target/iai-built/deps/typing"
        self.assertEqual(hashlib.sha256(packaged.read_bytes()).digest(),
                         hashlib.sha256(self.binary.read_bytes()).digest())
        self.assertTrue((packaged.parent / "fonts").is_dir())

    def test_cargo_package_working_directory_is_not_the_workspace_root(self):
        result = self.run_fixture(self.root / "apps/code-editor")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(self.calls()), 5)
        self.assertTrue((self.root / "audited").exists())

    def test_failed_correctness_check_stops_before_instrumentation(self):
        self.env["FIXTURE_FAILURE"] = "17"
        result = self.run_fixture()
        self.assertEqual(result.returncode, 17, result.stdout + result.stderr)
        self.assertEqual(len(self.calls()), 1)
        self.assertFalse((self.root / "audited").exists())

    def test_failed_font_verification_stops_before_any_measurement(self):
        self.env["FONT_FAILURE"] = "23"
        result = self.run_fixture()
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse((self.root / "calls.jsonl").exists())
        self.assertFalse((self.root / "audited").exists())

    def test_tool_cache_requires_exact_inputs_and_installed_binary_checksums(self):
        shutil.copyfile(REPO / "tools/perf/build-typing-tools.sh",
                        self.root / "tools/perf/build-typing-tools.sh")
        self.put("tools/perf/typing-valgrind-accounting.patch", "fixture-patch\n")
        self.put("bin/cc", "#!/bin/sh\necho fixture-cc\n", executable=True)
        # A cache miss may try downloading, but this wiring test never compiles
        # anything or contacts the network.
        self.put("bin/curl", "#!/bin/sh\nexit 31\n", executable=True)
        content = b""
        for name in ["tools/perf/build-typing-tools.sh",
                     "tools/perf/typing-valgrind-accounting.patch"]:
            digest = hashlib.sha256((self.root / name).read_bytes()).hexdigest()
            content += f"{digest}  {name}\n".encode()
        content += b"fixture-cc\nfixture-rustc\n"
        key = hashlib.sha256(content).hexdigest()
        cache = self.root / "target/iai-valgrind-fixed"
        (cache / "cache-key").write_text(key + "\n")
        for manifest, path in [
            ("install.sha256", cache / "install/libexec/valgrind/callgrind-amd64-linux"),
            ("runner.sha256", self.root / "target/iai-runner/bin/iai-callgrind-runner"),
        ]:
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            (cache / manifest).write_text(f"{digest}  {path}\n")
        command = ["bash", "tools/perf/build-typing-tools.sh"]
        result = subprocess.run(command, cwd=self.root, env=self.env,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Verified cached", result.stdout)
        (cache / "install/libexec/valgrind/callgrind-amd64-linux").write_text("corrupted")
        result = subprocess.run(command, cwd=self.root, env=self.env,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 31, result.stdout + result.stderr)
        self.assertNotIn("Verified cached", result.stdout)

    def test_bootstrap_failure_is_not_reported_as_a_benchmark_success(self):
        self.put("tools/perf/build-typing-tools.sh", "#!/bin/sh\nexit 27\n")
        result = self.run_fixture()
        self.assertEqual(result.returncode, 27)
        self.assertFalse((self.root / "calls.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
