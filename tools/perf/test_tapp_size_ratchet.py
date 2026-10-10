"""Tests of the tapp size ratchet (tools/perf/tapp_size_ratchet.py)."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import tapp_size_ratchet as ratchet  # noqa: E402

HEADER = """headroom = 0.01
architectures = ["x86_64", "aarch64"]

[fleet]
p50 = 2000000
p90 = 4000000
max = 6000000
"""
APPS = """
[apps."so.tilo.a"]
x86_64 = 1000000
aarch64 = 900000

[apps."so.tilo.b"]
x86_64 = 3000000
aarch64 = 2800000
"""
TEXT = HEADER + APPS
REASON = 'reason = "the toolkit gained guarded dictation"\nrun = 90210\n'
COLLECTION = {"so.tilo.a", "so.tilo.b"}


def budget(text: str = TEXT, **raises: str) -> ratchet.Budget:
    return ratchet.Budget(text, {f"{name}.toml": body for name, body in raises.items()})


def app_raise(app: str = "so.tilo.a", arch: str = "x86_64", added: int = 65536, reason: str = REASON) -> str:
    return f'{reason}[apps."{app}"]\n{arch} = {added}\n'


class Measure(unittest.TestCase):
    def package(self, sizes: dict[str, int], arch: str = "x86_64") -> pathlib.Path:
        """A directory shaped like package-apps.sh's output."""
        directory = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(directory)], check=False))
        (directory / "tapp").mkdir()
        apps = []
        for app, size in sizes.items():
            name = f"{app.rsplit('.', 1)[1]}-0.1.0-{arch}.tapp"
            (directory / "tapp" / name).write_bytes(b"\0" * size)
            apps.append({"id": app, "file": name})
        (directory / "index.json").write_text(json.dumps({"architecture": arch, "apps": apps}))
        return directory

    def test_an_app_at_its_budget_passes(self):
        status, lines = ratchet.measure(self.package({"so.tilo.a": 1_000_000}), budget(), "x86_64")
        self.assertEqual(status, 0, lines)
        self.assertIn("within its budget of 1000000", lines[0])

    def test_one_byte_over_the_budget_fails_and_names_the_raise(self):
        status, lines = ratchet.measure(self.package({"so.tilo.a": 1_000_001}), budget(), "x86_64")
        self.assertEqual(status, 1)
        self.assertIn("over its budget of 1000000 (+1)", lines[0])
        hint = lines[-1]
        self.assertIn("raises/<change>.toml", hint)
        self.assertIn('[apps."so.tilo.a"]', hint)
        # 1 % headroom on the 64 KiB grid: 1,048,576 is the next budget.
        self.assertIn("x86_64 = 48576", hint)

    def test_a_raise_file_adds_to_the_budget(self):
        raised = budget(dictation=app_raise(added=1))
        self.assertEqual(ratchet.measure(self.package({"so.tilo.a": 1_000_001}), raised, "x86_64")[0], 0)

    def test_a_raise_for_the_other_architecture_does_not_count(self):
        raised = budget(dictation=app_raise(arch="aarch64", added=65536))
        self.assertEqual(ratchet.measure(self.package({"so.tilo.a": 1_000_001}), raised, "x86_64")[0], 1)

    def test_an_app_without_a_budget_fails(self):
        status, lines = ratchet.measure(self.package({"so.tilo.new": 500_000}), budget(), "x86_64")
        self.assertEqual(status, 1)
        self.assertIn('add [apps."so.tilo.new"] x86_64 = 524288', lines[0])

    def test_a_shrunk_app_prints_the_lower_budget(self):
        status, lines = ratchet.measure(self.package({"so.tilo.b": 1_000_000}), budget(), "x86_64")
        self.assertEqual(status, 0)
        self.assertIn('lower it: [apps."so.tilo.b"] x86_64 = 1048576', lines[0])

    def test_the_architecture_must_match(self):
        directory = self.package({"so.tilo.a": 10}, arch="aarch64")
        self.assertEqual(ratchet.measure(directory, budget(), "x86_64")[0], 1)

    def test_an_empty_package_run_fails(self):
        self.assertEqual(ratchet.measure(self.package({}), budget(), "x86_64")[0], 1)


def entries(sizes: dict[tuple[str, str], int], sequence: int = 805) -> dict:
    by_package: dict[str, list] = {}
    for (package, arch), size in sizes.items():
        by_package.setdefault(package, []).append(
            {"tapp_size_bytes": size, "released_at_unix": 2,
             "target": {"platform": "tilo-os", "architecture": arch}})
    return {"sequence": sequence, "entries": [{"package_id": package, "versions": versions}
                                              for package, versions in by_package.items()]}


class Fleet(unittest.TestCase):
    SIZES = {("so.tilo.a", "x86_64"): 990_000, ("so.tilo.b", "x86_64"): 2_900_000,
             ("so.tilo.a", "aarch64"): 890_000, ("so.tilo.b", "aarch64"): 2_700_000,
             ("so.tilo.runtime", "x86_64"): 11_776, ("so.tilo.withdrawn", "x86_64"): 3_264_512}

    def test_only_budgeted_apps_count_and_the_rest_are_named(self):
        status, lines = ratchet.fleet(entries(self.SIZES), budget())
        self.assertEqual(status, 0, lines)
        self.assertIn("not counted: so.tilo.runtime, so.tilo.withdrawn", lines[1])
        self.assertIn("fleet x86_64: 2 apps, 3890000 bytes in all", lines)
        # Nearest rank: the median of two apps is the smaller.
        self.assertIn("fleet x86_64 p50: 990000 bytes, within the KPI target 2000000", lines)

    def test_the_newest_version_is_measured(self):
        index = entries(self.SIZES)
        index["entries"][0]["versions"].append(
            {"tapp_size_bytes": 1_500_000, "released_at_unix": 1,
             "target": {"platform": "tilo-os", "architecture": "x86_64"}})
        self.assertEqual(ratchet.fleet(index, budget())[0], 0)

    def test_a_statistic_over_its_target_fails(self):
        sizes = {**self.SIZES, ("so.tilo.a", "x86_64"): 2_100_000}
        roomy = TEXT.replace("x86_64 = 1000000", "x86_64 = 3000000")
        status, lines = ratchet.fleet(entries(sizes), budget(roomy))
        self.assertEqual(status, 1)
        self.assertTrue(any(line.startswith("error: fleet x86_64 p50: 2100000 bytes is over its ceiling of 2000000")
                            for line in lines), lines)

    def test_an_interim_ceiling_holds_a_missed_target(self):
        sizes = {**self.SIZES, ("so.tilo.a", "x86_64"): 2_100_000}
        roomy = (HEADER + "\n[fleet.interim.x86_64]\np50 = 2228224\n"
                 + APPS.replace("x86_64 = 1000000", "x86_64 = 3000000"))
        status, lines = ratchet.fleet(entries(sizes), budget(roomy))
        self.assertEqual(status, 0, lines)
        self.assertIn("fleet x86_64 p50: 2100000 bytes, KPI target 2000000 not met yet; held to 2228224; "
                      "lower it: [fleet.interim.x86_64] p50 = 2162688, without its raise files", lines)

    def test_an_interim_ceiling_whose_target_holds_asks_to_be_removed(self):
        text = HEADER + "\n[fleet.interim.x86_64]\np50 = 2228224\n" + APPS
        _, lines = ratchet.fleet(entries(self.SIZES), budget(text))
        self.assertIn("fleet x86_64 p50: 990000 bytes meets the KPI target 2000000; "
                      "remove [fleet.interim.x86_64] p50", lines)

    def test_a_published_app_over_its_budget_fails(self):
        sizes = {**self.SIZES, ("so.tilo.b", "aarch64"): 2_800_001}
        self.assertEqual(ratchet.fleet(entries(sizes), budget())[0], 1)

    def test_an_architecture_with_no_budgeted_app_fails(self):
        sizes = {key: size for key, size in self.SIZES.items() if key[1] == "x86_64"}
        status, lines = ratchet.fleet(entries(sizes), budget())
        self.assertEqual(status, 1)
        self.assertIn("error: aarch64: the registry lists none of the budgeted apps", lines)

    def test_an_unpublished_app_is_named_not_counted(self):
        sizes = {key: size for key, size in self.SIZES.items() if key != ("so.tilo.b", "aarch64")}
        status, lines = ratchet.fleet(entries(sizes), budget())
        self.assertEqual(status, 0, lines)
        self.assertIn("aarch64: not published yet, not counted: so.tilo.b", lines)


class Select(unittest.TestCase):
    """Which affected apps a pull request packages (tools/ci/select.sh)."""

    IDS = {"a": "so.tilo.a", "b": "so.tilo.b", "tool": "so.tilo.tool"}

    def test_the_app_closest_to_a_budget_on_either_architecture_goes_first(self):
        # a: 10,000 bytes left on x86_64, 900 on aarch64; b: 2,000 on each.
        published = {("so.tilo.a", "x86_64"): 990_000, ("so.tilo.a", "aarch64"): 899_100,
                     ("so.tilo.b", "x86_64"): 2_998_000, ("so.tilo.b", "aarch64"): 2_798_000}
        chosen, lines = ratchet.select_apps(["b", "a"], self.IDS, budget(), published, limit=1)
        self.assertEqual(chosen, ["a"])
        self.assertIn("  a: 900 bytes of headroom on its tightest architecture", lines)
        self.assertIn("further from their budgets, measured on main by Release / apps: b", lines)

    def test_a_raise_counts_toward_the_headroom(self):
        published = {("so.tilo.a", "x86_64"): 999_000, ("so.tilo.b", "x86_64"): 2_998_000}
        raised = budget(dictation=app_raise(added=65536))
        self.assertEqual(ratchet.select_apps(["a", "b"], self.IDS, raised, published, limit=1)[0], ["b"])

    def test_an_unpublished_app_goes_before_every_published_one(self):
        published = {("so.tilo.a", "x86_64"): 999_999}
        self.assertEqual(ratchet.select_apps(["a", "b"], self.IDS, budget(), published, limit=1)[0], ["b"])

    def test_an_app_without_a_budget_is_not_measured(self):
        chosen, lines = ratchet.select_apps(["tool"], self.IDS, budget(), {}, limit=8)
        self.assertEqual(chosen, [])
        self.assertIn("no tapp to measure", lines[0])

    def test_without_a_registry_index_the_apps_are_taken_by_name(self):
        chosen, lines = ratchet.select_apps(["b", "a"], self.IDS, budget(), None, limit=8)
        self.assertEqual(chosen, ["a", "b"])
        self.assertTrue(any(line.startswith("warning: no registry index") for line in lines))

    def test_the_command_writes_the_selection_for_the_workflow(self):
        directory = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(directory)], check=False))
        index, output = directory / "entries.json", directory / "output"
        index.write_text(json.dumps(entries({("so.tilo.settings", "x86_64"): 1_000})))
        self.assertEqual(ratchet.main(["select", "--apps", "settings not-an-app",
                                       "--entries", str(index), "--github-output", str(output)]), 0)
        self.assertEqual(output.read_text(), "tapp_apps=settings\ntapp_required=true\n")
        output.unlink()
        self.assertEqual(ratchet.main(["select", "--apps", "", "--entries", str(index),
                                       "--github-output", str(output)]), 0)
        self.assertEqual(output.read_text(), "tapp_apps=\ntapp_required=false\n")


class CheckBudget(unittest.TestCase):
    def test_an_unchanged_budget_passes(self):
        self.assertEqual(ratchet.check(budget(), budget(), COLLECTION), [])

    def test_a_raised_budget_without_a_raise_file_fails(self):
        after = budget(TEXT.replace("x86_64 = 1000000", "x86_64 = 1000001"))
        problems = ratchet.check(budget(), after, COLLECTION)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn('[apps."so.tilo.a"] x86_64: raised from 1000000 to 1000001', problems[0])

    def test_a_new_raise_file_allows_its_growth(self):
        self.assertEqual(ratchet.check(budget(), budget(dictation=app_raise()), COLLECTION), [])

    def test_a_raise_file_from_before_allows_nothing_new(self):
        before = budget(dictation=app_raise())
        after = budget(TEXT.replace("x86_64 = 1000000", "x86_64 = 1065536"), dictation=app_raise())
        self.assertEqual(len(ratchet.check(before, after, COLLECTION)), 1)

    def test_a_lower_budget_passes(self):
        after = budget(TEXT.replace("x86_64 = 3000000", "x86_64 = 2097152"))
        self.assertEqual(ratchet.check(budget(), after, COLLECTION), [])

    def test_a_raise_file_needs_a_reason_and_a_run(self):
        problems = ratchet.check(budget(), budget(lazy=app_raise(reason='reason = "bigger"\n')), COLLECTION)
        self.assertTrue(any("needs a reason" in problem for problem in problems), problems)

    def test_a_raise_file_may_not_name_an_app_without_a_budget(self):
        problems = ratchet.check(budget(), budget(stray=app_raise(app="so.tilo.c")), COLLECTION)
        self.assertTrue(any("names nothing with a budget" in problem for problem in problems), problems)

    def test_a_raise_may_not_more_than_double_a_budget(self):
        problems = ratchet.check(budget(), budget(huge=app_raise(added=1_000_001)), COLLECTION)
        self.assertTrue(any("more than doubles" in problem for problem in problems), problems)

    def test_a_raised_fleet_ceiling_needs_a_raise_file(self):
        interim = HEADER + "\n[fleet.interim.x86_64]\np90 = 4500000\n" + APPS
        problems = ratchet.check(budget(), budget(interim), COLLECTION)
        self.assertIn("fleet x86_64 p90: raised from 4000000 to 4500000 beyond the 0 bytes its new raise files add",
                      problems)
        fleet_raise = REASON + "[fleet.x86_64]\np90 = 500000\n"
        raised = budget(settings=fleet_raise)
        self.assertEqual(raised.fleet_ceilings()[("x86_64", "p90")], 4_500_000)
        self.assertEqual(ratchet.check(budget(), raised, COLLECTION), [])

    def test_a_kpi_target_and_the_headroom_only_decrease(self):
        after = budget(TEXT.replace("p50 = 2000000", "p50 = 2100000").replace("headroom = 0.01", "headroom = 0.05"))
        problems = ratchet.check(budget(), after, COLLECTION)
        self.assertIn("headroom raised", problems)
        self.assertTrue(any("KPI target p50 raised" in problem for problem in problems), problems)

    def test_a_new_app_starts_at_most_at_the_max_target(self):
        new = TEXT + '\n[apps."so.tilo.c"]\nx86_64 = 6000000\naarch64 = 6000001\n'
        problems = ratchet.check(budget(), budget(new), COLLECTION | {"so.tilo.c"})
        self.assertEqual(problems, ['[apps."so.tilo.c"] aarch64: a new app starts at most at the KPI max target, '
                                    "6000000 bytes"])

    def test_the_budgeted_apps_are_the_default_collection(self):
        problems = ratchet.check(None, budget(), {"so.tilo.a", "so.tilo.c"})
        self.assertIn('so.tilo.c is in the default collection without a budget; add [apps."so.tilo.c"] '
                      "(a new app may start at the KPI max target, 6000000 bytes, until it is measured)", problems)
        self.assertIn("so.tilo.b has a budget but is not in the default collection; remove it", problems)

    def test_every_app_has_a_budget_per_architecture(self):
        problems = ratchet.check(None, budget(TEXT.replace("aarch64 = 900000\n", "")), COLLECTION)
        self.assertEqual(problems, ['[apps."so.tilo.a"] needs a positive byte budget for each of x86_64, aarch64'])

    def test_an_interim_ceiling_at_or_below_its_target_is_removed(self):
        text = HEADER + "\n[fleet.interim.aarch64]\np50 = 2000000\n" + APPS
        self.assertEqual(ratchet.check(None, budget(text), COLLECTION),
                         ["[fleet.interim.aarch64] p50 is not above the KPI target; remove it"])


class Revisions(unittest.TestCase):
    def repository(self) -> pathlib.Path:
        root = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(root)], check=False))

        def git(*args: str) -> None:
            subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@invalid", *args],
                           cwd=root, check=True, capture_output=True)

        git("init", "-q")
        (root / "README").write_text("before the budget\n")
        git("add", "README")
        git("commit", "-q", "-m", "first")
        directory = root / ratchet.DIRECTORY
        (directory / "raises").mkdir(parents=True)
        (directory / "budget.toml").write_text(TEXT)
        (directory / "raises" / "dictation.toml").write_text(app_raise())
        git("add", ".")
        git("commit", "-q", "-m", "second")
        return root

    def test_the_budget_and_its_raises_are_read_at_a_revision(self):
        root = self.repository()
        at = ratchet.load_at("HEAD", root)
        self.assertEqual(at.app_totals()[("so.tilo.a", "x86_64")], 1_065_536)
        self.assertEqual(ratchet.load(root).app_totals(), at.app_totals())

    def test_a_revision_before_the_budget_has_none(self):
        self.assertIsNone(ratchet.load_at("HEAD~1", self.repository()))

    def test_an_unknown_revision_fails_instead_of_passing(self):
        with self.assertRaises(ValueError):
            ratchet.load_at("0" * 40, self.repository())


class CommittedBudget(unittest.TestCase):
    def test_the_committed_budget_is_well_formed_and_names_the_default_collection(self):
        self.assertEqual(ratchet.check(None, ratchet.load(), ratchet.default_collection()), [])

    def test_its_raise_files_are_valid(self):
        current = ratchet.load()
        for name, raised in sorted(current.raises.items()):
            with self.subTest(raise_file=name):
                self.assertEqual(ratchet.raise_problems(name, raised, current), [])


if __name__ == "__main__":
    unittest.main()
