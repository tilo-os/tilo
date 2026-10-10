"""Unit tests for the run's performance records and the installed-desktop gates' parsing.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import records  # noqa: E402
from acceptance.checks import resources, startup  # noqa: E402
import run  # noqa: E402

SOFTWARE = "software (no GPU render node; wgpu on a CPU rasterizer): nodes: card0 | drivers: vkms"

BOOT_PASS = """\
phases: kernel 1.204s, initrd 2.871s, userspace to greeter 6.412s
kernel+userspace reached usable greeter in 10.487s (budget: 20s)
GATE-EXIT=0
"""

BOOT_FAIL = """\
kernel+userspace reached usable greeter in 24.020s (budget: 20s)
phases: kernel 1.100s, initrd 9.000s, userspace to greeter 13.920s
GATE-EXIT=1
"""


def meta(**extra):
    base = {"commit": "a" * 40, "vm_cpus": 6, "vm_memory_mb": 4608,
            "outputs": ["1920x1080@1", "1280x800@1.25"], "renderer": SOFTWARE,
            "display": {"hardware": False},
            "provenance": {"record": {"commit": "b" * 40}, "booted_digest": "sha256:" + "c" * 64}}
    return {**base, **extra}


class Renderer(unittest.TestCase):
    def test_names_the_vm_renderers(self):
        self.assertEqual(records.renderer(SOFTWARE), "lavapipe")
        self.assertEqual(records.renderer("GPU (virgl): nodes: renderD128 | drivers: virtio-pci"), "virgl")
        self.assertEqual(records.renderer("GPU (i915): nodes: renderD128", {"hardware": True}), "gpu")

    def test_an_unnamed_renderer_is_refused_rather_than_guessed(self):
        with self.assertRaises(ValueError):
            records.renderer(None)
        with self.assertRaises(ValueError):
            records.renderer("GPU (unknown driver): nodes: renderD128", {"hardware": False})


class Build(unittest.TestCase):
    def test_samples_of_one_kpi_pool_into_one_record_with_the_run_environment(self):
        samples = [records.sample("app.open.warm_ms.notes", "ms", [420, 380], "A15", {"app": "so.tilo.notes"}),
                   records.sample("app.open.warm_ms.notes", "ms", [400], "A15", {}),
                   records.sample("idle.ram_mib", "MiB", [1100], "R01", {})]
        env = records.environment("a" * 40, "lavapipe", "runner-disk-100g", "6 vCPU")
        built = {r.kpi: r for r in records.build(samples, env, {"base_commit": "b" * 40})}
        notes = built["app.open.warm_ms.notes"]
        self.assertEqual((notes.n, notes.p50, notes.max), (3, 400.0, 420.0))
        self.assertEqual(notes.meta["check"], "A15")
        self.assertEqual(notes.meta["base_commit"], "b" * 40)
        self.assertEqual(notes.env_key, ("release", "lavapipe", "kvm", "runner-disk-100g"))
        self.assertIn("guest 6 vCPU", notes.hardware)
        self.assertEqual(built["idle.ram_mib"].unit, "MiB")

    def test_nothing_measured_is_no_sample_and_a_bad_unit_is_refused(self):
        self.assertIsNone(records.sample("idle.frames", "count", [None], "R01", {}))
        with self.assertRaises(ValueError):
            records.sample("idle.shell_cpu_pct", "%", [0.3], "R01", {})

    def test_one_kpi_in_two_units_is_refused(self):
        samples = [records.sample("boot.to_greeter_s", "s", [10], "R06", {}),
                   records.sample("boot.to_greeter_s", "ms", [10000], "R06", {})]
        env = records.environment("a" * 40, "lavapipe", "local", "guest")
        with self.assertRaises(ValueError):
            records.build(samples, env)


class WriteRecords(unittest.TestCase):
    def test_a_swapped_run_names_the_swapped_commit_and_keeps_the_base(self):
        swap = {"commit": "d" * 40, "components": [{"component": "shell"},
                                                   {"component": "app:notes", "error": "build failed"}]}
        m = meta()
        m["provenance"]["swap"] = swap
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.jsonl"
            count = run.write_records(path, [records.sample("idle.frames", "count", [0], "R01", {})],
                                      m, "runner-disk-100g")
            line = json.loads(path.read_text())
        self.assertEqual(count, 1)
        self.assertEqual(line["commit"], "d" * 40)
        self.assertEqual(line["meta"]["base_commit"], "b" * 40)
        self.assertEqual(line["meta"]["swapped"], "shell")
        self.assertEqual(line["renderer"], "lavapipe")
        self.assertEqual(line["virtualization"], "kvm")

    def test_an_unswapped_run_names_the_disk_and_an_aborted_run_writes_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.jsonl"
            run.write_records(path, [records.sample("idle.frames", "count", [1], "R01", {})], meta(), "local")
            self.assertEqual(json.loads(path.read_text())["commit"], "b" * 40)
            self.assertEqual(run.write_records(path, [], meta(), "local"), 0)
            self.assertEqual(path.read_text(), "")


# resources.SAMPLE's output for a 10 s window at HZ 100: the compositor (100) with a
# network worker, two llvmpipe threads, one thread that exits and one that starts in the
# window; its busier shader-seed child (200, also `shell`); Terminal's tilo-ush (300).
IDLE_SAMPLE = """\
@@0
U 40.00
P 100 shell 200 100 1 1000
H 100 100 shell 50 100
H 100 101 tilo-net-0 10 50
H 100 102 llvmpipe-0 5 10
H 100 103 llvmpipe-1 5 10
H 100 104 tilo-files-index 120 30
P 200 shell 0 0 100 3000
H 200 200 shell 0 0
P 300 tilo-ush 0 0 1 2000
H 300 300 tilo-ush 0 0
H 300 301 broker 0 0
T 10000 9000
@@1
U 50.05
P 100 shell 245 105 1 1000
H 100 100 shell 52 105
H 100 101 tilo-net-0 30 150
H 100 102 llvmpipe-0 15 30
H 100 103 llvmpipe-1 10 20
H 100 105 tilo-present 3 4
P 200 shell 300 2 100 3000
H 200 200 shell 300 2
P 300 tilo-ush 100 630 1 2000
H 300 300 tilo-ush 20 30
H 300 301 broker 80 600
T 16000 14000
HZ 100
"""


class IdleThreads(unittest.TestCase):
    def summary(self, text=IDLE_SAMPLE):
        return resources.summarize(resources._parse_sample(text), 10)

    def test_the_compositor_is_the_shell_with_no_shell_above_it_not_the_busiest(self):
        m = self.summary()
        self.assertEqual(m["compositor_pid"], 100)
        self.assertEqual((m["shell_cpu_pct"], m["shell_wakeups_per_s"]), (4.5, 0.5))
        self.assertEqual(m["top"][0]["pid"], 200)
        two = IDLE_SAMPLE.replace("P 200 shell 0 0 100 3000", "P 200 shell 0 0 1 3000")
        with self.assertRaises(resources.core.Unobservable):
            self.summary(two)

    def test_names_the_threads_that_spent_the_compositors_cpu(self):
        m = self.summary()
        self.assertEqual(m["session_age_s"], 30.0)
        self.assertEqual([(t["tid"], t["cpu_pct"]) for t in m["shell_threads"]],
                         [(101, 2.0), (102, 1.0), (103, 0.5), (105, 0.3), (100, 0.2)])
        self.assertTrue(next(t for t in m["shell_threads"] if t["tid"] == 105)["new"])
        groups = [(g["name"], g["threads"], g["cpu_pct"]) for g in m["shell_thread_groups"]]
        self.assertEqual(groups[:2], [("tilo-net", 1, 2.0), ("llvmpipe", 2, 1.5)])
        # 0.5 + 10 + 2 + 1 + 0.4 context switches per second, the main thread's 0.5 among them.
        self.assertEqual(m["shell_threads_wakeups_per_s"], 13.9)
        # tilo-files-index ended inside the window: its 0.5% is the process's, not a thread's.
        self.assertEqual(m["shell_exited_threads_cpu_pct"], 0.5)

    def test_a_busy_session_process_carries_its_own_threads(self):
        ush = next(p for p in self.summary()["top"] if p["comm"] == "tilo-ush")
        self.assertEqual((ush["cpu_pct"], ush["threads"][0]["name"], ush["threads"][0]["cpu_pct"]),
                         (10.0, "broker", 8.0))

    def test_a_busy_process_keeps_each_threads_index_and_whether_it_is_new(self):
        ush = next(p for p in self.summary()["top"] if p["comm"] == "tilo-ush")
        self.assertEqual([(t["tid"], t["comm"], t["new"]) for t in ush["thread_rows"]],
                         [(301, "broker", False), (300, "tilo-ush", False)])

    def test_terminals_runtime_sums_every_tilo_ush_process_and_thread(self):
        text = IDLE_SAMPLE.replace("@@1\n", "P 400 tilo-ush 0 0 1 2500\nH 400 400 tilo-ush 0 0\n@@1\n") \
            .replace("HZ 100", "P 400 tilo-ush 5 20 1 2500\nH 400 400 tilo-ush 5 20\n"
                               "H 400 401 iroh-blob-store-7 0 10\nHZ 100")
        ush = self.summary(text)["ush"]
        self.assertEqual((ush["processes"], ush["cpu_pct"], ush["wakeups_per_s"]), (2, 10.5, 66.0))
        self.assertEqual([g["name"] for g in ush["thread_groups"]], ["broker", "tilo-ush", "iroh-blob-store"])
        self.assertTrue(next(t for t in ush["threads"]["400"] if t["tid"] == 401)["new"])
        self.assertIsNone(self.summary(IDLE_SAMPLE.replace("tilo-ush", "foot"))["ush"])

    def test_tilo_ush_is_judged_only_once_terminals_shell_ran(self):
        budgets = {"idle_apps_ush_cpu_pct": 1.0, "idle_apps_ush_wakeups_per_s": 5.0}
        ush = self.summary()["ush"]
        cpu, woke = resources.ush_problems(ush, 3.5, budgets)
        self.assertEqual(cpu, "tilo-ush CPU 10.0% > 1.0% (broker 8.0%, tilo-ush 2.0%; exited threads 0.0%)")
        self.assertEqual(woke, "tilo-ush woke 63.0/s > 5.0/s (broker 60.0/s, tilo-ush 3.0/s)")
        [unmeasured] = resources.ush_problems(ush, None, budgets)
        self.assertIn("was not measured idle", unmeasured)
        quiet = {**ush, "cpu_pct": 0.2, "wakeups_per_s": 1.0}
        self.assertEqual(resources.ush_problems(quiet, 3.5, budgets), [])

    def test_an_over_budget_compositor_names_its_busiest_threads_and_age(self):
        m = {**self.summary(), "frames_painted": 0, "shell_loop_ticks_per_s": 0.0}
        budgets = {"idle_shell_cpu_pct": 1.0, "idle_frames": 2, "idle_loop_ticks_per_s": 2.0,
                   "idle_user_cpu_pct": 100.0}
        [problem] = resources._budget_problems(m, budgets, "idle")
        self.assertEqual(problem, "compositor CPU 4.5% > 1.0% (tilo-net 2.0%, llvmpipe×2 1.5%, "
                                  "tilo-present 0.3%, shell 0.2%, exited threads 0.5%; session age 30.0 s)")
        # The same window, a frame painted in it, on a CPU rasteriser leaves llvmpipe out and says so.
        [problem] = resources._budget_problems({**m, "frames_painted": 1}, budgets, "idle", software=True)
        self.assertEqual(problem, "compositor CPU less its software rasteriser's 1.5% is 3.0% > 1.0% "
                                  "(tilo-net 2.0%, tilo-present 0.3%, shell 0.2%, exited threads 0.5%; "
                                  "session age 30.0 s)")


# Run 91460's R01 in small (TIL-739): the clock frame rasterised by lavapipe. The compute pool
# that ran it (`tilo-gpu-driver`, 1.4%) and the rasteriser threads (`llvmpipe-0`) next to the
# present thread (0.1%) and the network runtime (0.2%): the compositor's own threads 0.3%.
RASTER_SAMPLE = """\
@@0
U 40.00
P 100 shell 100 100 1 1000
H 100 100 shell 50 100
H 100 101 tilo-present 10 10
H 100 102 llvmpipe-0 5 0
H 100 103 tilo-gpu-driver 5 0
H 100 104 tilo-gpu-driver 5 0
H 100 105 tilo-net-rt-0 10 10
T 10000 9000
@@1
U 50.00
P 100 shell 117 110 1 1000
H 100 100 shell 50 101
H 100 101 tilo-present 11 13
H 100 102 llvmpipe-0 5 0
H 100 103 tilo-gpu-driver 12 7
H 100 104 tilo-gpu-driver 12 8
H 100 105 tilo-net-rt-0 12 20
T 16000 15000
HZ 100
"""


class SoftwareRaster(unittest.TestCase):
    BUDGETS = {"idle_shell_cpu_pct": 1.0, "idle_frames": 2, "idle_loop_ticks_per_s": 2.0,
               "idle_user_cpu_pct": 100.0, "idle_software_raster_cpu_pct": 0.1}

    def window(self, frames):
        m = resources.summarize(resources._parse_sample(RASTER_SAMPLE), 10)
        return {**m, "frames_painted": frames, "shell_loop_ticks_per_s": 0.1}

    def test_the_rasterisers_threads_are_summed_apart(self):
        m = self.window(1)
        self.assertEqual((m["shell_cpu_pct"], m["shell_gpu_driver_cpu_pct"]), (1.7, 1.4))
        self.assertEqual([(g["name"], g["threads"]) for g in m["shell_thread_groups"]][:2],
                         [("tilo-gpu-driver", 2), ("tilo-net-rt", 1)])

    def test_a_software_renderer_is_judged_on_the_compositors_own_threads(self):
        self.assertEqual(resources._budget_problems(self.window(1), self.BUDGETS, "idle", software=True), [])

    def test_a_gpu_renderer_counts_every_thread(self):
        [problem] = resources._budget_problems(self.window(1), self.BUDGETS, "idle")
        self.assertTrue(problem.startswith("compositor CPU 1.7% > 1.0% (tilo-gpu-driver×2 1.4%"), problem)

    def test_a_rasteriser_busy_with_no_frame_painted_fails(self):
        [problem] = resources._budget_problems(self.window(0), self.BUDGETS, "idle", software=True)
        self.assertEqual(problem, "software rasteriser CPU 1.4% with no frame painted > 0.1%")


class Gates(unittest.TestCase):
    def test_boot_gate_total_and_phases(self):
        gate = resources.parse_boot_gate(BOOT_PASS)
        self.assertEqual(gate, {"exit": 0, "to_greeter_s": 10.487, "kernel_s": 1.204,
                                "initrd_s": 2.871, "userspace_s": 6.412})
        failed = resources.parse_boot_gate(BOOT_FAIL)
        self.assertEqual((failed["exit"], failed["to_greeter_s"], failed["initrd_s"]), (1, 24.02, 9.0))

    def test_a_boot_gate_that_named_no_time_has_none(self):
        gate = resources.parse_boot_gate("greetd.service is not active\nGATE-EXIT=1\n")
        self.assertEqual(gate, {"exit": 1, "to_greeter_s": None})

    def test_open_times_run_from_the_request_and_from_process_start(self):
        marks = {"path": "x", "origin_epoch_ms": 1_000_150, "events": {"first-frame-presented": 0.412}}
        self.assertEqual(startup.open_times(1_000_000, marks), (562.0, 412.0))
        self.assertIsNone(startup.open_times(1_000_000, {"path": "x", "origin_epoch_ms": 1, "events": {}}))
        self.assertIsNone(startup.open_times(1_000_000, None))
        self.assertEqual(startup.median([5.0, 1.0, 9.0, 7.0]), 5.0)

    def test_the_compositor_memory_breakdown_names_its_largest_anonymous_mappings(self):
        text = "\n".join([
            "S VmRSS: 1253068", "S RssAnon: 598118", "S RssFile: 650000", "S Threads: 87",
            "R Rss: 1253068", "R Pss_Anon: 598000", "R Anonymous: 598118", "R Swap: 0",
            "A 262144 7f0000000000-7f0010000000 [anon]",
            "A 65536 55d000000000-55d004000000 [heap]",
            "A 4096 7f1000000000-7f1000400000 /memfd:wayland-shm (deleted)",
        ])
        m = resources.core.parse_shell_memory(text)
        self.assertEqual(m["status_kib"], {"VmRSS": 1253068, "RssAnon": 598118, "RssFile": 650000, "Threads": 87})
        self.assertEqual(m["smaps_rollup_kib"]["Anonymous"], 598118)
        self.assertEqual([t["name"] for t in m["top_anon_mappings"]],
                         ["[anon]", "[heap]", "/memfd:wayland-shm (deleted)"])
        self.assertEqual(m["top_anon_mappings"][0]["anon_kib"], 262144)

    def test_the_compositor_breakdown_names_its_shmem_by_fd(self):
        text = "\n".join([
            "S VmRSS: 600000", "S RssAnon: 256000", "S RssShmem: 167936",
            "F\t31\t492830720 600000 512\t\t/memfd:allocation fd (deleted)",
            "F\t40\t4194304 8192 512\t\t/memfd:wayland-shm (deleted)",
            "F\t41\t4194304 4096 512\t\t/memfd:wayland-shm (deleted)",
            "F\t12\t0 0 4096\tdrm-driver=virtio_gpu drm-total-memory=33_MiB \t/dev/dri/card0",
            "F\t50\t- - -\tsize=8294400 exp_name=udmabuf \tanon_inode:dmabuf",
            "PAGE-MONITOR no",
        ])
        m = resources.core.parse_shell_memory(text)
        self.assertEqual(list(m["shm_by_name"]), ["allocation fd", "wayland-shm"])
        self.assertEqual(m["shm_by_name"]["allocation fd"],
                         {"count": 1, "size_mib": 470.0, "allocated_mib": 293.0})
        self.assertEqual(m["shm_by_name"]["wayland-shm"]["count"], 2)
        drm = next(f for f in m["shm_fds"] if f["kind"] == "drm")
        self.assertEqual(drm["fdinfo"], {"drm-driver": "virtio_gpu", "drm-total-memory": "33_MiB"})
        dmabuf = next(f for f in m["shm_fds"] if f["kind"] == "dmabuf")
        self.assertEqual((dmabuf["allocated_bytes"], dmabuf["fdinfo"]["size"]), (None, "8294400"))

    def test_an_unreadable_compositor_breakdown_is_unobservable(self):
        class Vm:
            def root_exec(self, script):
                assert "pid=42" in script
                return "S VmRSS: 10\n"

        class Ctx:
            vm = Vm()

        with self.assertRaises(resources.core.Unobservable):
            resources.core.shell_memory(Ctx(), 42)

    def test_the_new_checks_are_in_the_run_order(self):
        for cid in ("R06", "A15", "R07", "A16"):
            self.assertIn(cid, run.ORDER)
        self.assertLess(run.ORDER.index("R02"), run.ORDER.index("A15"))
        self.assertLess(run.ORDER.index("A16"), run.ORDER.index("R05"))


if __name__ == "__main__":
    unittest.main()
