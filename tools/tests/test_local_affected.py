import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "ci/local_affected.py"
spec = importlib.util.spec_from_file_location("local_affected", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FailedLineTests(unittest.TestCase):
    def test_nextest_status_lines_name_binary_and_test(self):
        cases = {
            "        FAIL [   0.010s] (  3/120) shell::it overview::adds_workspace":
                ("shell::it", "overview::adds_workspace"),
            "     SIGSEGV [   1.000s] tilo-ui tests::paints":
                ("tilo-ui", "tests::paints"),
            "     TIMEOUT [ 480.001s] (1/2) collaboration::bin/peer sync::stalls":
                ("collaboration::bin/peer", "sync::stalls"),
            "   LEAK-FAIL [   0.200s] (9/9) agent::it presence_semantics::leaves":
                ("agent::it", "presence_semantics::leaves"),
        }
        for line, expected in cases.items():
            with self.subTest(line=line):
                match = module.FAILED_LINE.match(line)
                self.assertIsNotNone(match)
                self.assertEqual((match.group(2), match.group(3)), expected)

    def test_passing_and_other_lines_are_not_failures(self):
        for line in ("        PASS [   0.010s] (  3/120) shell::it overview::adds_workspace",
                     "error: test run failed",
                     "    Starting 12 tests across 3 binaries",
                     '{"type":"test","event":"failed","name":"shell::it$overview::x"}'):
            with self.subTest(line=line):
                self.assertIsNone(module.FAILED_LINE.match(line))


class SelectionTests(unittest.TestCase):
    def test_package_flags_round_trip_and_union_keeps_order(self):
        rows = [{"packages": "-p shell -p tilo-ui"}, {"packages": "-p tilo-ui -p ebook"}, {"packages": ""}]
        self.assertEqual(module.ordered_union(rows, "packages"), ["shell", "tilo-ui", "ebook"])
        self.assertEqual(module.flags(["a", "b"]), ["-p", "a", "-p", "b"])
        self.assertEqual(module.names("-p a --package b"), ["a", "b"])

    def test_split_gives_the_second_builder_its_share_and_keeps_shell_on_x86(self):
        seconds = {"shell": 600, "tilo-ui": 300, "agent": 250, "chat": 50, "tiny": 0}
        costs = {"shell": 400, "tilo-ui": 200, "agent": 100, "chat": 50, "tiny": 10}
        first, second = module.split_packages(list(seconds), seconds, costs, share=0.25)
        self.assertIn("shell", first)
        self.assertEqual(sorted(first + second), sorted(seconds))
        self.assertTrue(second)
        weight = lambda names: sum(costs[n] / 760 + seconds[n] / 1200 for n in names)  # noqa: E731
        self.assertLessEqual(weight(second), 0.25 * weight(seconds) + 1e-9)

    def test_failed_tests_become_an_exact_filterset(self):
        expression = module.filterset_of({("shell::it", "a::b"), ("tilo-ui", "c")})
        self.assertEqual(expression,
                         "(binary_id(=shell::it) and test(=a::b)) or (binary_id(=tilo-ui) and test(=c))")
        self.assertEqual(module.package_of("collaboration::bin/peer"), "collaboration")


class RecordTests(unittest.TestCase):
    def test_record_is_per_worktree_and_base(self):
        with tempfile.TemporaryDirectory() as scratch:
            with mock.patch.object(module, "git", return_value=scratch):
                path = module.record_path("origin/main")
                self.assertEqual(path, Path(scratch) / "local-affected" / "failed-origin_main.json")
                self.assertEqual(module.read_record(path), set())
                module.write_record(path, {("shell::it", "a::b")})
                self.assertEqual(module.read_record(path), {("shell::it", "a::b")})
                self.assertNotEqual(module.record_path("integration"), path)


class JobTests(unittest.TestCase):
    def test_a_job_records_failures_and_whether_the_run_finished(self):
        with tempfile.TemporaryDirectory() as scratch:
            job = module.Job("test", Path(scratch))
            local = module.Builder("local", None)
            script = ("echo '        FAIL [   0.010s] (1/2) shell::it a::b'; "
                      "echo '        PASS [   0.010s] (2/2) shell::it a::c'; exit 100")
            with mock.patch.object(module, "say"):
                code = job.run("nextest", local, ["bash", "-c", script])
            self.assertEqual(code, 100)
            self.assertEqual(job.failed_tests, {("shell::it", "a::b")})
            self.assertTrue(job.ran_to_end)
            self.assertFalse(job.ok())
            with mock.patch.object(module, "say"):
                job.run("build", local, ["bash", "-c", "echo 'error: could not compile'; exit 101"])
            self.assertFalse(job.ran_to_end)
            self.assertIn("could not compile", job.tail())

    def test_a_builder_runs_its_command_through_rcargo_exec(self):
        builder = module.Builder("x86", Path("/config/x86.toml"))
        command, env = builder.command(["bash", "tools/ci/lint-affected.sh"])
        self.assertEqual(command, ["rcargo", "exec", "--", "bash", "tools/ci/lint-affected.sh"])
        self.assertEqual(env["RCARGO_CONFIG"], "/config/x86.toml")
        self.assertEqual(module.Builder("local", None).command(["true"])[0], ["true"])


class HelpTests(unittest.TestCase):
    def test_help_runs_nothing(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--help"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("test-changed.sh", result.stdout)


if __name__ == "__main__":
    unittest.main()
