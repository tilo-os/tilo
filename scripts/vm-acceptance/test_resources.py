"""Unit tests for the idle memory breakdown (resources.py footprint, core.PROCESSES).

    python3 -m unittest scripts/vm-acceptance/test_resources.py

The fixture is the shape of one `core.PROCESSES` dump from a vm-acceptance guest: the
cgroups and process names of run 90408's dump (seat0 greeter as uid 974, the slot as
1001, the dev image's services, a root ssh session), with the dump's newer fields.
"""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core, records  # noqa: E402
from acceptance.checks import resources  # noqa: E402

MIB = 2**20
SLOT = 1001
APPS = "/user.slice/user-1001.slice/user@1001.service/app.slice"
SHELL_CG = "/user.slice/user-1001.slice/session-14.scope"
GREETER_CG = "/user.slice/user-974.slice/session-c1.scope"
UDEV = "/system.slice/systemd-udevd.service"


def proc(pid, ppid, uid, comm, cgroup, rss, anon=0, app="-"):
    """A `P` line in MiB: RSS is anon, 512 KiB of shmem and the rest file; no swap."""
    return "\t".join(map(str, ("P", pid, ppid, uid, comm, rss * 1024, anon * 1024,
                               max(rss - anon, 0) * 1024, 512, 0, app, cgroup)))


def cgroup(path, anon, file=0, shmem=0, kernel=1, at=None):
    """A `C` line in MiB: memory.current is anon + file + kernel; file holds shmem."""
    values = (anon + file + kernel, anon, file, shmem, file // 2, kernel, 0)
    return "\t".join(["C", path, at or path] + [str(v * MIB) for v in values])


# Managed pages, low and high watermarks, largest lowmem protection; 4 KiB pages.
ZONES = ("Z 0,DMA 3840 10 12 3000", "Z 0,DMA32 700000 3000 4000 1500", "Z 0,Normal 400000 2000 2600 0")


def dump(meminfo=None, extra=()):
    mem = {"MemTotal": 4509084, "MemAvailable": 3207004, "AnonPages": 600 * 1024,
           "Shmem": 420 * 1024, "SUnreclaim": 60 * 1024, "KernelStack": 12 * 1024,
           "PageTables": 20 * 1024, "SecPageTables": 0, "VmallocUsed": 30 * 1024,
           "Percpu": 8 * 1024, "Cached": 900 * 1024, "HugePages_Total": 0,
           "Active(file)": 500 * 1024, "Inactive(file)": 300 * 1024, "KReclaimable": 40 * 1024}
    if meminfo is not None:
        mem = meminfo(mem)
    lines = [
        proc(1, 0, 0, "systemd", "/init.scope", 9, 10),
        proc(825, 1, 0, "firewalld", "/system.slice/firewalld.service", 27, 27),
        proc(821, 1, 994, "chronyd", "/system.slice/chronyd.service", 18, 18),
        proc(700, 1, 0, "systemd-udevd", f"{UDEV}/udev", 5, 5),
        proc(1030, 1, 0, "sshd", "/system.slice/sshd.service", 3, 3),
        proc(986, 1, 0, "tilo-device-adm", "/system.slice/tilo-device-admin.service", 7, 7),
        proc(1400, 1, 0, "tilo-vmd", "/system.slice/tilo-vm-awake.service", 1, 1),
        proc(9630, 1, 0, "tilo-vmd", "/system.slice/tilo-vm-input-acceptance.service", 1, 1),
        proc(9964, 9963, 0, "sh", "/user.slice/user-0.slice/session-69.scope", 1, 1),
        proc(1107, 1, 0, "greetd", "/system.slice/greetd.service", 2, 1),
        proc(1114, 1107, 0, "greetd", GREETER_CG, 2, 2),
        proc(1214, 1114, 974, "cage", GREETER_CG, 6, 4),
        proc(1291, 1214, 974, "tilo-greeter", GREETER_CG, 210, 145),
        proc(1322, 1137, 974, "dbus-broker-lau", "/user.slice/user-974.slice/user@974.service/session.slice/"
             "dbus-broker.service", 1, 1),
        proc(4119, 1, SLOT, "tilo-session", SHELL_CG, 2, 1),
        proc(4200, 4119, SLOT, "shell", SHELL_CG, 400, 340),
        proc(4300, 4200, SLOT, "shell", SHELL_CG, 20, 5),
        proc(5910, 4098, SLOT, "fs", f"{APPS}/tilo-fs-recipient.service", 4, 4),
        proc(5911, 4098, SLOT, "fs", f"{APPS}/tilo-fs-cloud.service", 3, 3),
        proc(5920, 4098, SLOT, "wireplumber", "/user.slice/user-1001.slice/user@1001.service/session.slice/"
             "wireplumber.service", 6, 6),
        proc(6000, 4200, SLOT, "tilo-launcher", f"{APPS}/app-tilo-so.tilo.notes-6000.scope", 3, 1,
             "so.tilo.notes"),
        proc(6001, 6000, SLOT, "notes", f"{APPS}/app-tilo-so.tilo.notes-6000.scope", 120, 110),
        cgroup("/init.scope", 10, file=90, shmem=60),
        cgroup("/system.slice/firewalld.service", 28, file=12),
        cgroup("/system.slice/chronyd.service", 18, file=7),
        "\t".join(["C", f"{UDEV}/udev", UDEV] + ["-"] * 7),
        cgroup(UDEV, 5, file=3),
        cgroup("/system.slice/sshd.service", 3, file=2),
        cgroup("/system.slice/tilo-device-admin.service", 7, file=21),
        cgroup("/system.slice/tilo-vm-awake.service", 2),
        cgroup("/system.slice/tilo-vm-input-acceptance.service", 1),
        cgroup("/user.slice/user-0.slice/session-69.scope", 2),
        cgroup("/system.slice/greetd.service", 2, file=2),
        cgroup(GREETER_CG, 150, file=500, shmem=180, kernel=6),
        cgroup("/user.slice/user-974.slice/user@974.service/session.slice/dbus-broker.service", 1),
        cgroup(SHELL_CG, 342, file=315, shmem=40, kernel=12),
        cgroup(f"{APPS}/tilo-fs-recipient.service", 4),
        cgroup(f"{APPS}/tilo-fs-cloud.service", 3),
        cgroup("/user.slice/user-1001.slice/user@1001.service/session.slice/wireplumber.service", 6),
        cgroup(f"{APPS}/app-tilo-so.tilo.notes-6000.scope", 111, file=55, shmem=20, kernel=4),
        *(f"M {k}: {v}" + (" kB" if not k.startswith("Huge") else "") for k, v in mem.items()),
        *ZONES,
        "PS 4096",
        "G 974",
        "T 61440 /run",
        "T 1024 /tmp",
        *extra,
    ]
    return "\n".join(lines) + "\n"


def footprint(text=None):
    return resources.footprint_of(core.parse_processes(text or dump()), SLOT)


class Recorder:
    """A context that keeps records as run.py's does, through records.sample."""
    def __init__(self):
        self.records = {}

    def record(self, kpi, unit, values, **meta):
        item = records.sample(kpi, unit, values, "R01", meta)
        if item is not None:
            self.records[kpi] = item["samples"]


class Parse(unittest.TestCase):
    def test_every_process_carries_its_rss_split_and_every_cgroup_its_charge_point(self):
        table = core.parse_processes(dump())
        greeter = table["procs"][1291]
        self.assertEqual((greeter["rss_mb"], greeter["anon_rss_mb"], greeter["file_rss_mb"],
                          greeter["shmem_rss_mb"], greeter["swap_mb"]), (210.0, 145.0, 65.0, 0.5, 0.0))
        self.assertEqual(table["zones"]["0,DMA32"], {"managed": 700000, "low": 3000, "high": 4000,
                                                     "protection": 1500})
        self.assertEqual(table["page_size"], 4096)
        self.assertEqual(table["procs"][6001]["app"], "so.tilo.notes")
        self.assertEqual(table["cgroups"][f"{UDEV}/udev"]["charged_at"], UDEV)
        self.assertIsNone(table["cgroups"][f"{UDEV}/udev"]["memory_mb"])
        self.assertEqual(table["cgroups"][GREETER_CG]["shmem_mb"], 180.0)
        self.assertEqual(table["greeter_uid"], 974)
        self.assertEqual(table["meminfo_kib"]["Shmem"], 420 * 1024)
        self.assertEqual(table["meminfo_mb"]["MemTotal"], 4403)
        self.assertEqual(table["tmpfs"][0], {"mount": "/run", "used_mib": 60.0})

    def test_the_script_prints_what_the_parser_reads(self):
        # The production script names every field the parser expects. It reads no smaps:
        # Tilo's kernel has none (CONFIG_PROC_PAGE_MONITOR=n).
        for needle in ("RssAnon", "RssFile", "RssShmem", "VmSwap", "/proc/zoneinfo", "protection:",
                       "getconf PAGESIZE", "id -u greetd", "df -kP -t tmpfs",
                       "anon file shmem file_mapped kernel sock"):
            self.assertIn(needle, core.PROCESSES)
        self.assertNotIn("smaps", core.PROCESSES)


class Footprint(unittest.TestCase):
    def test_buckets_cover_every_cgroup_once(self):
        fp = footprint()
        table = core.parse_processes(dump())
        points = {c["charged_at"] for c in table["cgroups"].values() if c["charged_at"]}
        seen = [c["cgroup"] for b in fp["buckets"].values() for c in b["cgroups"]]
        self.assertEqual(sorted(seen), sorted(points))
        for b in fp["buckets"].values():
            self.assertAlmostEqual(b["resident_mib"], sum(c["resident_mib"] for c in b["cgroups"]), places=1)
        self.assertEqual(fp["uncharged_processes"], [])

    def test_greeter_and_harness_are_named_not_guessed(self):
        fp = footprint()
        where = {c["cgroup"]: name for name, b in fp["buckets"].items() for c in b["cgroups"]}
        self.assertEqual(where[SHELL_CG], "compositor")
        self.assertEqual(where[GREETER_CG], "greeter")
        self.assertEqual(where["/user.slice/user-974.slice/user@974.service/session.slice/dbus-broker.service"],
                         "greeter")
        for harness in ("/system.slice/sshd.service", "/system.slice/tilo-device-admin.service",
                        "/system.slice/tilo-vm-awake.service", "/system.slice/tilo-vm-input-acceptance.service",
                        "/user.slice/user-0.slice/session-69.scope"):
            self.assertEqual(where[harness], "harness", harness)
        # greetd itself runs on an installed machine too; only its greeter session ends.
        self.assertEqual(where["/system.slice/greetd.service"], "system")
        self.assertEqual(where[UDEV], "system")
        self.assertEqual(where[f"{APPS}/app-tilo-so.tilo.notes-6000.scope"], "apps")
        self.assertEqual(where[f"{APPS}/tilo-fs-recipient.service"], "session_services")
        self.assertEqual(fp["compositor_pid"], 4200)

    def test_the_greeter_uid_comes_from_the_dump(self):
        text = dump().replace("G 974", "G 975")
        fp = footprint(text)
        where = {c["cgroup"]: name for name, b in fp["buckets"].items() for c in b["cgroups"]}
        self.assertEqual(where[GREETER_CG], "system")

    def test_installed_excludes_only_unreclaimable_harness_memory(self):
        fp = footprint()
        # greeter 150+180+6 + 1+0+1; harness 3+0+1, 7+0+1, 2+0+1, 1+0+1, 2+0+1.
        self.assertEqual(fp["vm_only_mib"], 338.0 + 20.0)
        ctx = Recorder()
        resources.record_footprint(ctx, fp, 1272)
        self.assertEqual(ctx.records["idle.ram_mib.installed"], [1272 - 358.0])
        self.assertEqual(ctx.records["idle.ram_mib.harness"], [358.0])
        # More reclaimable cache in a harness cgroup changes nothing.
        more_cache = dump().replace(cgroup("/system.slice/tilo-device-admin.service", 7, file=21),
                                    cgroup("/system.slice/tilo-device-admin.service", 7, file=400))
        self.assertEqual(footprint(more_cache)["vm_only_mib"], 358.0)

    def test_the_guest_splits_into_anon_shmem_kernel_and_reserve(self):
        g = footprint()["guest"]
        self.assertEqual((g["anon_mib"], g["shmem_mib"], g["kernel_mib"]), (600.0, 420.0, 130.0))
        self.assertEqual(g["used_mib"], round((4509084 - 3207004) / 1024, 1))
        # Zones: min(3000 + 12, 3840) + min(1500 + 4000, 700000) + min(0 + 2600, 400000)
        # pages; low watermarks 5010 pages, which caps both halves MemAvailable withholds.
        parts = g["reserve_parts_mib"]
        self.assertEqual(parts["zones_mib"], round(11112 * 4096 / MIB, 1))
        self.assertEqual(parts["withheld_cache_mib"], round(2 * 5010 * 4096 / MIB, 1))
        self.assertEqual(g["reserve_mib"], round(parts["zones_mib"] + parts["withheld_cache_mib"], 1))
        self.assertEqual(g["untracked_mib"], round(g["used_mib"] - 1150.0 - g["reserve_mib"], 1))

    def test_a_dump_without_zones_is_unobservable(self):
        text = "\n".join(line for line in dump().splitlines() if not line.startswith("Z "))
        with self.assertRaises(core.Unobservable):
            footprint(text)

    def test_records_are_valid_kpis_in_one_unit(self):
        ctx = Recorder()
        resources.record_footprint(ctx, footprint(), 1272, "four_apps")
        for kpi in ("idle.ram_mib.anon.four_apps", "idle.ram_mib.shmem.four_apps", "idle.ram_mib.kernel.four_apps",
                    "idle.ram_mib.reserve.four_apps", "idle.ram_mib.untracked.four_apps",
                    "memory.idle_mib.greeter.four_apps", "memory.idle_mib.compositor.four_apps",
                    "memory.idle_mib.unattributed.four_apps", "memory.idle_anon_mib.shell.four_apps",
                    "memory.idle_anon_mib.tilo-greeter.four_apps"):
            self.assertIn(kpi, ctx.records)
        self.assertEqual(ctx.records["memory.idle_anon_mib.shell.four_apps"], [345.0])
        self.assertEqual(resources.kpi_word("(sd-pam)"), "sd-pam")

    def test_missing_shmem_field_is_unobservable(self):
        def no_shmem(mem):
            return {k: v for k, v in mem.items() if k != "Shmem"}
        with self.assertRaises(core.Unobservable):
            footprint(dump(no_shmem))

    def test_a_cgroup_without_memory_stat_kernel_is_unobservable(self):
        broken = dump().replace(cgroup("/system.slice/chronyd.service", 18, file=7),
                                "\t".join(["C", "/system.slice/chronyd.service", "/system.slice/chronyd.service",
                                           str(25 * MIB), str(18 * MIB), str(7 * MIB), "0", "0", "-", "0"]))
        with self.assertRaises(core.Unobservable):
            footprint(broken)


class ShellMemory(unittest.TestCase):
    def ctx(self, text):
        class Vm:
            def root_exec(self, script):
                assert "pid=42" in script
                return text

        class Ctx:
            vm = Vm()
        return Ctx()

    def test_without_the_page_monitor_the_status_is_the_evidence(self):
        m = core.shell_memory(self.ctx("S VmRSS: 600000\nS RssAnon: 256000\nPAGE-MONITOR no\n"), 42)
        self.assertEqual((m["anon_rss_mib"], m["page_monitor"], m["top_anon_mappings"]), (250.0, False, []))

    def test_with_the_page_monitor_an_unread_rollup_is_unobservable(self):
        with self.assertRaises(core.Unobservable):
            core.shell_memory(self.ctx("S VmRSS: 600000\nS RssAnon: 256000\n"), 42)


class Growth(unittest.TestCase):
    def test_open_close_slope(self):
        self.assertEqual(resources.slope([631, 653, 672, 690]), 19.6)
        self.assertEqual(resources.slope([631 + 13.6 * i for i in range(33)]), 13.6)
        self.assertEqual(resources.slope([500.0, 500.0, 500.0]), 0.0)
        self.assertIsNone(resources.slope([500.0]))

    def test_the_compositor_anon_is_read_from_the_process_table(self):
        table = core.parse_processes(dump())
        self.assertEqual(resources.shell_anon_mib(table, SLOT), 340.0)
        self.assertIsNone(resources.shell_anon_mib(table, 4242))


def shell_report(**overrides):
    """A `process_memory` answer as the compositor gives it (memory_report.rs)."""
    report = {
        "pid": 4299,
        "status_kib": {"VmRSS": 460676, "RssAnon": 197912, "RssFile": 189760, "RssShmem": 73004, "Threads": 87},
        "malloc": {"arena_kib": 90000, "mmapped_kib": 20000, "mmapped_blocks": 9, "in_use_kib": 70000,
                   "free_kib": 40000, "top_releasable_kib": 100},
        "threads": [{"name": "tokio-runtime-w", "count": 24}, {"name": "shell", "count": 1}],
        "mappings": {"classes": {
            "heap_arenas": {"resident_kib": 81920, "virtual_kib": 1048576, "mappings": 20},
            "gpu_memfd": {"resident_kib": 74240, "virtual_kib": 79067, "mappings": 3},
            "reserved": {"resident_kib": 0, "virtual_kib": 9000000, "mappings": 90},
            "kernel_pages": {"resident_kib": 0, "virtual_kib": 12, "mappings": 2},
            "thread_stacks": {"resident_kib": 10240, "virtual_kib": 180000, "mappings": 87},
        }, "top_mappings": [], "top_files": [], "unreadable_mappings": 0},
        "page_monitor": False,
    }
    report.update(overrides)
    return report


class ShellMemoryMap(unittest.TestCase):
    def test_parts_are_resident_mib_per_class_without_address_space(self):
        parts = resources.shell_parts_mib(resources.parse_shell_memory_map(shell_report()))
        self.assertEqual(list(parts), ["heap_arenas", "gpu_memfd", "thread_stacks", "malloc_in_use", "malloc_free"])
        self.assertEqual((parts["heap_arenas"], parts["gpu_memfd"], parts["malloc_free"]), (80.0, 72.5, 39.1))

    def test_a_failure_says_how_much_is_heap_in_use_free_and_by_kind(self):
        text = resources.describe_shell_parts(shell_report())
        self.assertTrue(text.startswith("heap in use "), text)
        self.assertIn("heap free 39.1 MiB", text)
        self.assertIn("heap_arenas 80.0 MiB, gpu_memfd 72.5 MiB", text)

    def test_records_are_valid_kpis_in_one_unit(self):
        ctx = Recorder()
        resources.record_shell_parts(ctx, shell_report(), "after_close")
        self.assertEqual(ctx.records["memory.shell_parts_mib.heap_arenas.after_close"], [80.0])
        self.assertEqual(ctx.records["memory.shell_threads.after_close"], [87])
        self.assertNotIn("memory.shell_parts_mib.reserved.after_close", ctx.records)

    def test_an_error_or_an_empty_report_is_unobservable(self):
        for answer in (None, "unknown op", shell_report(status_kib={}),
                       shell_report(mappings={"classes": {"reserved": {"resident_kib": 0}}})):
            with self.assertRaises(core.Unobservable):
                resources.parse_shell_memory_map(answer)

    def test_the_idle_compositor_is_held_to_its_anon_budget(self):
        from run import BUDGETS
        self.assertEqual(resources.shell_idle_anon_problems(159.0, BUDGETS), [])
        problems = resources.shell_idle_anon_problems(238.7, BUDGETS)
        self.assertEqual(len(problems), 1)
        self.assertIn("238.7 MB > 200 MB", problems[0])

    def test_the_compositor_is_asked_over_its_control_socket(self):
        class Vm:
            def raw(self, op):
                assert op == {"op": "process_memory"}
                return {"answer": shell_report()}

        class Ctx:
            vm = Vm()
        self.assertEqual(resources.shell_memory_map(Ctx())["pid"], 4299)


class DiskTest(unittest.TestCase):
    def test_parses_the_slot_users_runtime_and_store_bytes(self):
        out = "tilo-runtime 430080000\ntilo-apps 12582912\nbuilds 2\n"
        self.assertEqual(resources.parse_disk(out), {
            "runtime_bytes": 430080000, "app_store_bytes": 12582912, "runtime_builds": 2})

    def test_a_missing_line_is_unobservable_not_zero(self):
        with self.assertRaises(core.Unobservable):
            resources.parse_disk("tilo-runtime 1\nbuilds 0\n")


# Two `SAMPLE` snapshots 10 s apart: the compositor (100) with two broker threads, Notes
# (300) and Files (400) with a shovel each, and the relay (500), whose own threads count
# for nothing here. Fields: pid tid comm ticks context-switches.
SHARED_SAMPLE = """\
@@0
U 100.0
P 100 shell 0 0 1 1000
H 100 100 shell 0 0
H 100 101 tilo-collab-bro 0 10
H 100 102 tilo-collab-bro 0 4
P 300 app 0 0 1 2000
H 300 301 ui-collab-shove 0 7
P 400 app 0 0 1 2000
H 400 401 ui-collab-shove 0 3
P 500 tilo-relay 0 0 1 2000
H 500 501 ui-collab-shove 0 0
T 10000 9000
@@1
U 110.0
P 100 shell 5 30 1 1000
H 100 100 shell 2 10
H 100 101 tilo-collab-bro 1 16
H 100 102 tilo-collab-bro 0 6
H 100 103 tilo-collab-bro 0 2
P 300 app 1 0 1 2000
H 300 301 ui-collab-shove 0 7
P 400 app 1 0 1 2000
H 400 401 ui-collab-shove 0 4
P 500 tilo-relay 9 50 1 2000
T 11000 9900
HZ 100
"""
HOST = bytes(range(32))
BUDGETS = {"idle_shared_broker_wakeups_per_s": 1.0, "idle_shared_shovel_wakeups_per_s": 0.0}


def shared_state(governor_caps: dict | None, room="7a1e", pending=()):
    """A state dump whose workspace 2 is in `room`, held under HOST's governance."""
    rooms = {} if governor_caps is None else {room: {"governor": {
        "device_public_key": list(HOST), "capabilities": governor_caps}}}
    return {"workspace_tree": {
        "active": 1,
        "workspaces": [{"id": 1, "room": "0000"}, {"id": 2, "room": room}],
        "native_authority": {"revision": 3, "rooms": rooms, "pending": {r: {} for r in pending}}}}


class SharedWorkspaceTest(unittest.TestCase):
    def measured(self):
        sample = resources._parse_sample(SHARED_SAMPLE)
        return {"compositor_pid": resources.compositor_pid(sample["procs"][0]),
                "named_threads": resources.named_threads(
                    sample, (resources.BROKER_THREAD, resources.SHOVEL_THREAD), 10)}

    def test_named_threads_sum_per_process_and_count_new_threads_from_zero(self):
        rows = {(r["pid"], r["thread"]): r for r in self.measured()["named_threads"]}
        self.assertEqual(rows[(100, "tilo-collab-bro")]["threads"], 3)
        # 6 + 2 switches by the old threads, 2 by the one born in the window.
        self.assertEqual(rows[(100, "tilo-collab-bro")]["ctxt_switches_per_s"], 1.0)
        self.assertEqual(rows[(100, "tilo-collab-bro")]["cpu_pct"], 0.1)
        self.assertEqual(rows[(300, "ui-collab-shove")]["ctxt_switches_per_s"], 0.0)
        self.assertEqual(rows[(400, "ui-collab-shove")]["ctxt_switches_per_s"], 0.1)
        self.assertNotIn((100, "shell"), rows)
        self.assertNotIn((500, "ui-collab-shove"), rows, "a thread that ended has no window")

    def test_the_broker_and_the_shovels_are_judged_apart(self):
        judged, problems = resources.shared_idle(self.measured(), BUDGETS)
        self.assertEqual((judged["broker_wakeups_per_s"], judged["broker_threads"]), (1.0, 3))
        self.assertEqual((judged["shovel_wakeups_per_s"], judged["shovels"]), (0.1, 2))
        self.assertEqual(len(problems), 1)
        self.assertIn("shovels woke 0.1/s > 0.0/s (app[400] 0.1/s)", problems[0])
        judged, problems = resources.shared_idle(self.measured(), dict(BUDGETS, idle_shared_broker_wakeups_per_s=0.5,
                                                                       idle_shared_shovel_wakeups_per_s=0.1))
        self.assertEqual(len(problems), 1)
        self.assertIn("broker woke 1.0/s > 0.5/s", problems[0])

    def test_without_a_broker_or_a_shovel_nothing_was_measured(self):
        m = self.measured()
        no_broker = dict(m, named_threads=[r for r in m["named_threads"] if r["thread"] != "tilo-collab-bro"])
        with self.assertRaises(core.Unobservable):
            resources.shared_idle(no_broker, BUDGETS)
        # A shovel in the compositor is not an app's.
        compositor_only = dict(m, named_threads=[dict(r, pid=100) for r in m["named_threads"]])
        with self.assertRaises(core.Unobservable):
            resources.shared_idle(compositor_only, BUDGETS)

    def test_a_room_a_host_governs_is_committed(self):
        host = {"can_own_workspaces": False, "can_act_as_host": True, "foreign": False}
        self.assertEqual(resources.committed_workspace(shared_state(host), HOST.hex())["id"], 2)

    def test_a_node_stewards_its_rooms_another_device_governs_none_and_pending_is_not_yet(self):
        node = {"can_own_workspaces": True, "can_act_as_host": True, "foreign": False}
        self.assertIsNone(resources.committed_workspace(shared_state(node), HOST.hex()))
        host = {"can_own_workspaces": False, "can_act_as_host": True, "foreign": False}
        self.assertIsNone(resources.committed_workspace(shared_state(host), bytes(32).hex()))
        pending = shared_state(None, pending=("7a1e",))
        self.assertIsNone(resources.committed_workspace(pending, HOST.hex()))
        self.assertIn("pending: 7a1e", resources.describe_native_rooms(pending))
        self.assertIn("7a1e (governor 000102030405)", resources.describe_native_rooms(shared_state(host)))

    def test_only_the_runs_relay_leaves_the_home_peer_registry(self):
        mine, other = "ab" * 32, "cd" * 32
        text = json.dumps([{"device_node_id": mine, "label": "Acceptance home relay"},
                           {"device_node_id": other, "label": "Home server"}])
        self.assertEqual(json.loads(resources.without_home_peer(text, mine)),
                         [{"device_node_id": other, "label": "Home server"}])
        self.assertIsNone(resources.without_home_peer(text, "ef" * 32))
        self.assertIsNone(resources.without_home_peer("", mine))
        self.assertIsNone(resources.without_home_peer('{"not": "a list"}', mine))

    def test_each_slot_user_has_its_own_relay_port_below_the_ephemeral_range(self):
        ports = {resources.home_relay_port(uid) for uid in range(1000, 1010)}
        self.assertEqual(len(ports), 10)
        self.assertTrue(all(1024 < p < 32768 for p in ports))

    def test_setup_failures_name_what_the_guest_lacked(self):
        key = '{"device_public_key": "%s", "relay_service_node_id": "%s"}' % ("ab" * 32, "cd" * 32)

        def ctx_answering(setup):
            class Vm:
                def exec(self, script, timeout=120, check=True):
                    return key if "device-key" in script and "certify" not in script else setup

                def slot_uid(self):
                    return 1001

            class Ctx:
                vm = Vm()

                def write(self, name, text):
                    pass
            return Ctx()

        cases = {"HOME-RELAY no-identity-root\n": "identity root of its own",
                 "error: unexpected argument '--home-peer-service' found\n": "predates",
                 "Failed to start transient service unit\n": "did not start"}
        for out, said in cases.items():
            with self.assertRaises(core.Unobservable) as raised:
                resources.start_home_relay(ctx_answering(out))
            self.assertIn(said, str(raised.exception))
        relay = resources.start_home_relay(ctx_answering("HOME-RELAY started\n"))
        self.assertEqual(relay, {"device": "ab" * 32, "service": "cd" * 32, "port": 24000 + 1001 % 8000})


if __name__ == "__main__":
    unittest.main()
