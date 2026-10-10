"""Unit tests for the idle memory breakdown (resources.py footprint, core.PROCESSES).

    python3 -m unittest scripts/vm-acceptance/test_resources.py

The fixture is the shape of one `core.PROCESSES` dump from a vm-acceptance guest: the
cgroups and process names of run 90408's dump (seat0 greeter as uid 974, the slot as
1001, the dev image's services, a root ssh session), with the dump's newer fields.
"""

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


def proc(pid, ppid, uid, comm, cgroup, pss, anon=0, app="-"):
    """A `P` line: RSS is PSS plus a little shared; shmem and PSS splits follow."""
    kib = pss * 1024
    return "\t".join(map(str, ("P", pid, ppid, uid, comm, kib + 2048, anon * 1024, 512,
                               kib, anon * 1024, 256, app, cgroup)))


def cgroup(path, anon, file=0, shmem=0, kernel=1, at=None):
    """A `C` line in MiB: memory.current is anon + file + kernel; file holds shmem."""
    values = (anon + file + kernel, anon, file, shmem, file // 2, kernel, 0)
    return "\t".join(["C", path, at or path] + [str(v * MIB) for v in values])


def dump(meminfo=None, extra=()):
    mem = {"MemTotal": 4509084, "MemAvailable": 3207004, "AnonPages": 600 * 1024,
           "Shmem": 420 * 1024, "SUnreclaim": 60 * 1024, "KernelStack": 12 * 1024,
           "PageTables": 20 * 1024, "SecPageTables": 0, "VmallocUsed": 30 * 1024,
           "Percpu": 8 * 1024, "Cached": 900 * 1024, "HugePages_Total": 0}
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
    def test_every_process_carries_its_pss_split_and_every_cgroup_its_charge_point(self):
        table = core.parse_processes(dump())
        greeter = table["procs"][1291]
        self.assertEqual((greeter["pss_mb"], greeter["pss_anon_mb"], greeter["pss_shmem_mb"]),
                         (210.0, 145.0, 0.2))
        self.assertEqual(table["procs"][6001]["app"], "so.tilo.notes")
        self.assertEqual(table["cgroups"][f"{UDEV}/udev"]["charged_at"], UDEV)
        self.assertIsNone(table["cgroups"][f"{UDEV}/udev"]["memory_mb"])
        self.assertEqual(table["cgroups"][GREETER_CG]["shmem_mb"], 180.0)
        self.assertEqual(table["greeter_uid"], 974)
        self.assertEqual(table["meminfo_kib"]["Shmem"], 420 * 1024)
        self.assertEqual(table["meminfo_mb"]["MemTotal"], 4403)
        self.assertEqual(table["tmpfs"][0], {"mount": "/run", "used_mib": 60.0})

    def test_the_script_prints_what_the_parser_reads(self):
        # The production script names every field the parser expects, in its order.
        for needle in ("RssShmem", "Pss_Anon", "Pss_Shmem", "smaps_rollup", "id -u greetd",
                       "df -kP -t tmpfs", "anon file shmem file_mapped kernel sock"):
            self.assertIn(needle, core.PROCESSES)


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

    def test_the_guest_splits_into_anon_shmem_and_kernel(self):
        g = footprint()["guest"]
        self.assertEqual((g["anon_mib"], g["shmem_mib"], g["kernel_mib"]), (600.0, 420.0, 130.0))
        self.assertEqual(g["used_mib"], round((4509084 - 3207004) / 1024, 1))
        self.assertEqual(g["other_mib"], round(g["used_mib"] - 1150.0, 1))

    def test_records_are_valid_kpis_in_one_unit(self):
        ctx = Recorder()
        resources.record_footprint(ctx, footprint(), 1272, "four_apps")
        for kpi in ("idle.ram_mib.anon.four_apps", "idle.ram_mib.shmem.four_apps", "idle.ram_mib.kernel.four_apps",
                    "memory.idle_mib.greeter.four_apps", "memory.idle_mib.compositor.four_apps",
                    "memory.idle_mib.unattributed.four_apps", "memory.idle_pss_mib.shell.four_apps",
                    "memory.idle_pss_mib.tilo-greeter.four_apps"):
            self.assertIn(kpi, ctx.records)
        self.assertEqual(ctx.records["memory.idle_pss_mib.shell.four_apps"], [420.0])
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


if __name__ == "__main__":
    unittest.main()
