import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
from runtime_benchmark import distribution, measured_rows, present_rows, worker_summary, resource_summary, correlate_frames


class StatisticsTests(unittest.TestCase):
    def test_frame_correlation_requires_explicit_unique_identity(self):
        with self.subTest(phase='explicit identity keeps missing outcomes'):
            import json
            lines = ["tilo-render-frame " + json.dumps(row) for row in [
                {"frame_ix": 100, "cpu_frame_id": 3, "outcome": "Presented"},
                {"frame_ix": 101, "cpu_frame_id": 4, "outcome": "Skipped"},
                {"frame_ix": 103, "cpu_frame_id": 99, "outcome": "Presented"},
                {"frame_ix": 102, "cpu_frame_id": None, "outcome": "Presented"}]]
            report = correlate_frames([{"frame":3},{"frame":4},{"frame":5}], lines)
            self.assertEqual(report["matched_outcomes"], {"Presented":1,"Skipped":1})
            self.assertEqual(report["cpu_frames_without_worker_outcome"], [5])
            self.assertEqual(report["unprofiled_worker_frames"], 1)
            self.assertEqual(report["worker_frames_without_cpu_sample"], [99])
        with self.subTest(phase='cpu frame identity must be unique nonnegative integer'):
            for frames in [[{"frame":-1}], [{"frame":True}], [{"frame":None}],
                           [{"frame":3}, {"frame":3}]]:
                with self.assertRaises(ValueError):
                    correlate_frames(frames, [])
        with self.subTest(phase='missing or duplicate identity rejected'):
            with self.assertRaises(ValueError):
                correlate_frames([{"frame":3}], ['tilo-render-frame {"frame_ix":3,"outcome":"Presented"}'])
            row = 'tilo-render-frame {"cpu_frame_id":3,"outcome":"Presented"}'
            with self.assertRaises(ValueError):
                correlate_frames([{"frame":3}], [row,row])

    def test_resource_summary_separates_cache_results_and_requires_valid_samples(self):
        with self.subTest(phase='cache hits do not dilute miss cost'):
            import json
            lines = ["tilo-resource-stage " + json.dumps({"stage": "local_cache",
                "cache_result": result, "duration_us": duration, "generation": 7,
                "permission_generation": 2}) for result, duration in
                [("hit", 1), ("hit", 2), ("miss", 30)]]
            result = resource_summary(lines)
            self.assertEqual(result["local_cache"]["hit"]["n"], 2)
            self.assertEqual(result["local_cache"]["miss"]["p99_ms"], 0.03)
        with self.subTest(phase='diagnostics must have valid samples'):
            for lines in [[], ['tilo-resource-stage {"stage":"local_cache","cache_result":"miss","duration_us":-1}']]:
                with self.assertRaises(ValueError):
                    resource_summary(lines)

    def test_sample_selection_and_nearest_rank_distribution(self):
        with self.subTest(phase='nearest rank tail and units'):
            stats = distribution(list(range(1, 101)), scale=0.001)
            self.assertEqual(stats["n"], 100)
            self.assertEqual(stats["p50_ms"], 0.05)
            self.assertEqual(stats["p95_ms"], 0.095)
            self.assertEqual(stats["p99_ms"], 0.099)
        with self.subTest(phase='missing samples fail'):
            with self.assertRaises(ValueError):
                distribution([])
        with self.subTest(phase='warmup and tail excluded'):
            self.assertEqual(measured_rows(["startup", "warmup", "sample", "tail"], 2, 3), ["sample"])
            with self.assertRaises(ValueError):
                measured_rows([1], 0, 2)
        with self.subTest(phase='present parse ignores other logs'):
            rows = present_rows(["other log", "present-frame 9: acquire=10us encode=20us submit=30us present=40us scene=1B (800x600)"])
            self.assertEqual(rows, [{"acquire_us":10,"encode_us":20,"submit_us":30,"present_us":40}])

    def test_worker_summary_counts_only_presented_work_and_requires_evidence(self):
        with self.subTest(phase='skips are not successful frames'):
            import json
            lines = ["tilo-render-frame " + json.dumps({"frame_ix": i, "outcome": outcome,
                     "start_us": i * 100, "end_us": i * 100 + elapsed, "total_us": elapsed,
                     "width": 1600, "height": 1200})
                     for i, (outcome, elapsed) in enumerate([("Presented", 40), ("Skipped", 1), ("Presented", 60)])]
            result = worker_summary(lines)
            self.assertEqual(result["outcomes"], {"Presented": 2, "Skipped": 1})
            self.assertEqual(result["presented_worker_wall"]["n"], 2)
            self.assertEqual(result["presented_worker_wall"]["p50_ms"], 0.04)
            self.assertEqual(result["skipped_fraction"], 1 / 3)
        with self.subTest(phase='occlusion is counted as skipped not presented work'):
            import json
            lines=['tilo-render-frame '+json.dumps(dict(frame_ix=i,outcome=outcome,total_us=cost))
                   for i,(outcome,cost) in enumerate([('Presented',40),('Occluded',1),('Skipped',2)])]
            result=worker_summary(lines)
            self.assertEqual(result['outcomes']['Occluded'],1)
            self.assertEqual(result['occluded_fraction'],1/3)
            self.assertEqual(result['skipped_fraction'],2/3)
            self.assertEqual(result['presented_worker_wall']['n'],1)
            self.assertEqual(result['presented_worker_wall']['p99_ms'],.04)
            with self.assertRaises(ValueError):
                worker_summary([lines[1]])
        with self.subTest(phase='worker evidence cannot be missing'):
            with self.assertRaises(ValueError):
                worker_summary(["unrelated log"])
        with self.subTest(phase='surface loss cannot pass as a fast frame'):
            with self.assertRaises(ValueError):
                worker_summary(['tilo-render-frame {"outcome":"SurfaceGone","total_us":null}'])


class MeasurementFailureTests(unittest.TestCase):
    def test_reload_outcomes_use_the_benchmark_main_path(self):
        spec = importlib.util.spec_from_file_location(
            "dev_reload_bench", Path(__file__).with_name("dev-reload-bench.py"))
        bench = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bench)
        for no_timings in (False, True):
            for outcome in ("reload rejected: incompatible state", "build failed",
                            "reload without a presented frame", "success"):
                if no_timings and outcome.startswith("reload without"):
                    continue  # This message is emitted only with --timings.
                with self.subTest(no_timings=no_timings, outcome=outcome), tempfile.TemporaryDirectory() as tmp:
                    app = Path(tmp)
                    (app / "src").mkdir()
                    source = app / "src/lib.rs"
                    original = 'String::from("world")'
                    source.write_text(original)
                    success = outcome == "success"
                    line = ("tilo: reloaded 1 open window" if no_timings else
                            "[tilo-app] reload 12 ms") if success else "[tilo-app] " + outcome
                    streams = []

                    class Stream:
                        def __init__(self, *args):
                            self.lines = []
                            streams.append(self)

                        def wait_for(self, pattern, start, timeout):
                            text = "[tilo-app] started (pid 123)" if not start else line
                            self.lines.append((1.0, text))
                            return (len(self.lines) - 1, 1.0, text) if pattern.search(text) else None

                    proc = mock.Mock()
                    argv = ["bench", str(app), "--runs", "1", "--settle", "0"]
                    if no_timings:
                        argv.append("--no-timings")

                    def stop(process, *args, **kwargs):
                        if process is proc and success and not no_timings:
                            report = {"phases_ms": {}, "first_frame_unix_ms": 1000,
                                      "changed_unix_ms": 1000, "path": "hot"}
                            streams[1].lines.append((1, json.dumps(
                                {"result": {"reloads": [report, report]}})))

                    with mock.patch.object(bench.sys, "argv", argv), \
                         mock.patch.object(bench, "start_compositor", return_value=(mock.Mock(), "wayland-1")), \
                         mock.patch.object(bench.subprocess, "Popen", return_value=proc), \
                         mock.patch.object(bench, "Lines", Stream), \
                         mock.patch.object(bench, "kill_group", side_effect=stop) as cleanup, \
                         mock.patch.object(bench.time, "sleep"), \
                         mock.patch.object(bench.time, "time", return_value=1.0), \
                         mock.patch.object(bench.tempfile, "mkdtemp", return_value=tmp), \
                         contextlib.redirect_stdout(io.StringIO()), \
                         contextlib.redirect_stderr(io.StringIO()):
                        if success:
                            self.assertEqual(bench.main(), 0)
                        else:
                            with self.assertRaises(SystemExit) as failure:
                                bench.main()
                            self.assertIn(outcome, str(failure.exception))
                        self.assertEqual(cleanup.call_count, 2)
                    self.assertEqual(source.read_text(), original)

    def test_boot_measurement_requires_success_and_observed_timing(self):
        script = Path(__file__).with_name("qemu-boot-measure.sh").resolve()
        target = "systemd[1]: Reached target multi-user.target - Multi-User System."
        cases = [("success", "[    1.250] " + target, True),
                 ("graphical", "[    2.500] previous line\n\\x1b[32m[ OK ]\\x1b[0m " +
                  target.replace("multi-user", "graphical"), True),
                 ("panic", "Kernel panic", False),
                 ("panic-after-target", "[ 1.250] " + target + "\nKernel panic", False),
                 ("exited", "", False), ("timeout", "", False),
                 ("missing-timing", target, False),
                 ("late-timing", target + "\n[ 9.000] too late", False),
                 ("parser-failure", "[ 1.250] " + target, False)]
        for name, console, success in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                image = root / "base.qcow2"
                image.touch()
                fixture = root / "console"
                fixture.write_text(console.replace("\\x1b", "\x1b"))
                tools = root / "bin"
                tools.mkdir()
                qemu = tools / "qemu-system-x86_64"
                qemu.write_text('''#!/usr/bin/env python3
import os, pathlib, socket, sys
args = sys.argv
pathlib.Path(args[args.index('-pidfile') + 1]).write_text('99999999')
serial = args[args.index('-serial') + 1].removeprefix('file:')
pathlib.Path(serial).write_text(pathlib.Path(os.environ['FIXTURE']).read_text())
monitor = args[args.index('-monitor') + 1].split(',')[0].removeprefix('unix:')
s = socket.socket(socket.AF_UNIX)
s.bind(monitor)
''')
                stubs = {"qemu-img": 'touch "${@: -1}"',
                         "nc": 'echo shutdown >> "$EVENTS"; exit 1',
                         "seq": "exit 0"}
                if name == "parser-failure":
                    stubs["awk"] = "exit 42"
                for tool, body in stubs.items():
                    (tools / tool).write_text("#!/usr/bin/env bash\n" + body + "\n")
                for tool in tools.iterdir():
                    tool.chmod(0o755)
                env = {**os.environ, "PATH": f"{tools}:{os.environ['PATH']}",
                       "TILO_DISTROBOX": "", "TILO_PERF_OUT": str(root),
                       "FIXTURE": str(fixture), "EVENTS": str(root / "events"),
                       "CASE": name}
                # Avoid real waiting or signalling; all measurement and shutdown
                # control flow still runs in the production script.
                wrapper = '''
kill() {
    echo "kill $*" >> "$EVENTS"
    if [ "$1" = -0 ] && [ "$CASE" = exited ]; then return 1; fi
    return 0
}
sleep() { if [ "$CASE" = timeout ]; then SECONDS=$((SECONDS + 200)); fi; }
export -f kill sleep
exec bash "$1" "$2" run
'''
                (root / "run.json").write_text("stale result")
                result = subprocess.run(["bash", "-c", wrapper, "test", str(script), str(image)],
                                        env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0 if success else 1, result.stderr)
                self.assertIn("shutdown", (root / "events").read_text())
                self.assertIn("kill -9", (root / "events").read_text())
                self.assertFalse((root / "run.snapshot.qcow2").exists())
                if success:
                    report = json.loads(result.stdout)
                    self.assertEqual(report["state"], "reached-target")
                    self.assertEqual(report["boot_ms"], 2500 if name == "graphical" else 1250)
                else:
                    self.assertEqual(result.stdout, "")
                    self.assertFalse((root / "run.json").exists())
                    self.assertIn("boot measurement failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
