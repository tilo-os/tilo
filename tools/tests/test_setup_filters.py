"""The ci profile's costly setup steps run for the packages whose code can use them.

tools/ci/setup_filters.py reads the two filters from the code. These tests hold
.config/nextest.toml to that reading, and exercise the reading on trees written for it: a
package that names an audio endpoint or the font catalog without being served fails here, before
its tests fail for want of one.
"""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


setup_filters = load("setup_filters", ROOT / "tools/ci/setup_filters.py")


def written(root: Path, layout: dict[str, dict]) -> dict[str, dict]:
    """Packages written below `root`, as `setup_filters.workspace` describes a workspace's: each
    with its files (`files`, by path), what it depends on (`needs`) and where it is (`at`)."""
    packages = {}
    for name, package in layout.items():
        directory = root / package.get("at", name)
        directory.mkdir(parents=True, exist_ok=True)
        for file, text in package.get("files", {}).items():
            path = directory / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        packages[name] = {"directory": directory, "dependencies": set(package.get("needs", ()))}
    return packages


class WorkspaceTests(unittest.TestCase):
    """The filters .config/nextest.toml carries, against this tree's code."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.packages = setup_filters.workspace()
        cls.named = setup_filters.evidence(cls.packages)

    def test_the_config_carries_the_filters_the_code_gives(self) -> None:
        # A package whose sources start naming an audio endpoint is a root the audio filter
        # lacks, and one that names the font catalog without depending on its reader gets no
        # catalog: both are said here, with the filter to write.
        found = setup_filters.problems(self.packages, self.named)
        self.assertEqual(found, [], "\n" + "\n".join(found))

    def test_the_reading_sees_the_sources_it_judges(self) -> None:
        # A reading that found no source, or none of the names it looks for, would give an
        # empty filter and call the config wrong for serving anyone.
        self.assertGreater(len(self.named), 1000, "the workspace's Rust sources are read")
        libraries, alone = setup_filters.audio_clients(self.named)
        self.assertIn("tilo-media", libraries, "the player that renders to the endpoint")
        self.assertIn("tilo-test-env", alone, "the test that requires the promised endpoint")
        self.assertIn(setup_filters.FONT_OWNER, setup_filters.catalog_namers(self.named))

    def test_every_test_gets_the_runtime_and_only_some_the_costly_steps(self) -> None:
        rules = setup_filters.configured()
        self.assertEqual(rules["test-runtime"], ["all()"])
        everything = set(self.packages)
        for setup, root in (("font-catalog", setup_filters.FONT_OWNER),
                            ("headless-audio", "tilo-media")):
            with self.subTest(setup=setup):
                (filter_,) = rules[setup]
                served = setup_filters.selected(self.packages, filter_)
                self.assertIn(root, served)
                # What the split is for: a step every package still paid for would be `all()`
                # under another name.
                self.assertLess(served, everything)

    def test_a_filter_the_config_does_not_carry_is_named_with_the_one_to_write(self) -> None:
        rules = {"test-runtime": ["all()"], "font-catalog": ["all()"]}
        with mock.patch.object(setup_filters, "configured", return_value=rules):
            found = setup_filters.problems(self.packages, self.named)
        self.assertEqual(len(found), 2, found)
        self.assertIn("runs `font-catalog` for ['all()']", found[0])
        self.assertIn(f'filter = "rdeps({setup_filters.FONT_OWNER})"', found[0])
        self.assertIn("runs `headless-audio` for no test", found[1])
        self.assertIn("rdeps(tilo-media)", found[1])


class StepSelectionTests(unittest.TestCase):
    """The costly steps a command that names packages pays for: the ones the ci profile runs for
    the packages it selected, and no others (tools/test-env.sh asks this before its steps)."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.packages = setup_filters.workspace()

    def steps(self, *chosen: str) -> list[str]:
        return setup_filters.steps_for(self.packages, set(chosen))

    def test_each_step_is_the_one_its_setup_runs(self) -> None:
        steps = setup_filters.setup_steps()
        self.assertEqual({name: steps[name] for name in ("font-catalog", "headless-audio")},
                         {"font-catalog": "fonts", "headless-audio": "audio"})

    def test_a_package_outside_both_closures_gets_no_step(self) -> None:
        self.assertEqual(self.steps("tilo-collab-types"), [])

    def test_a_package_the_catalog_serves_gets_the_fonts_step(self) -> None:
        owner = setup_filters.FONT_OWNER
        served = setup_filters.selected(self.packages, f"rdeps({owner})")
        self.assertGreater(len(served), 1, "the catalog serves more than its reader")
        for package in (owner, *sorted(served - {owner})[:3]):
            with self.subTest(package=package):
                self.assertIn("fonts", self.steps(package))

    def test_a_package_the_endpoint_serves_gets_the_audio_step(self) -> None:
        (filter_,) = setup_filters.configured()["headless-audio"]
        served = setup_filters.selected(self.packages, filter_)
        self.assertGreater(len(served), 1, "the endpoint serves more than one package")
        for package in sorted(served)[:3]:
            with self.subTest(package=package):
                self.assertIn("audio", self.steps(package))

    def test_a_selection_of_several_gets_the_steps_of_any_of_them(self) -> None:
        self.assertEqual(self.steps("tilo-collab-types", setup_filters.FONT_OWNER, "tilo-media"),
                         ["fonts", "audio"])

    def test_a_package_that_declares_no_test_decides_no_step(self) -> None:
        # t names the family's unify member beside every command of the family, and it depends on
        # every library the family builds: it starts no test, so it needs no step of its own.
        self.assertFalse(self.packages["workspace-unify"]["runs_tests"])
        self.assertEqual(self.steps("workspace-unify"), [])
        self.assertEqual(self.steps("workspace-unify", "tilo-collab-types"), [])

    def test_a_command_that_names_no_package_gets_every_step(self) -> None:
        # A whole-workspace run, and one that names only packages this workspace has not: the
        # command decides neither step's audience, so all of them are prepared, as they were.
        self.assertEqual(self.steps(), ["fonts", "audio"])
        self.assertEqual(self.steps("no-such-package"), ["fonts", "audio"])

    def test_a_filter_the_reading_cannot_read_pays_the_step_rather_than_leaving_it_out(self) -> None:
        # A step left out fails the tests that need it; one prepared needlessly only takes its
        # time, so a filter of another shape is taken as selecting the command's packages.
        packages = {"a": {"directory": Path("a"), "dependencies": set()}}
        with mock.patch.object(setup_filters, "configured",
                               return_value={"font-catalog": ["package(a) and test(/draws/)"]}), \
             mock.patch.object(setup_filters, "setup_steps",
                               return_value={"font-catalog": "fonts"}), \
             mock.patch.object(setup_filters, "COSTLY_STEPS", ["fonts"]):
            self.assertEqual(setup_filters.steps_for(packages, {"a"}), ["fonts"])


class ReadingTests(unittest.TestCase):
    """What the reading makes of sources written for it."""

    def test_library_code_serves_its_dependents_and_a_test_only_its_package(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            packages = written(Path(directory), {
                # Library code that renders to the endpoint: whatever links it can.
                "player": {"files": {"src/sink.rs": 'launch("audioconvert ! autoaudiosink")'}},
                "app": {"needs": {"player"}, "files": {"src/main.rs": "fn main() {}"}},
                # Only this package's own tests reach for one.
                "probe": {"files": {"src/lib.rs": "pub fn probe() {}",
                                    "tests/it.rs": 'std::env::var("PULSE_SERVER")'}},
                # A test's statement is its package's wherever it is written, as a unit test
                # is: what depends on the package does not run it.
                "strict": {"files": {"src/lib.rs": 'require_capability!(\n    Audio, found, "none");'}},
                "user": {"needs": {"strict"}, "files": {"src/lib.rs": ""}},
                # Words that only hold a name: "compactly" holds PulseAudio's `pactl`.
                "quiet": {"files": {"src/lib.rs": "// written compactly\nconst AGENT_PULSE_QUIET: u8 = 1;"}},
                # A package inside another's directory: its files are its own.
                "inner": {"at": "quiet/tools/inner", "files": {"src/lib.rs": 'run("pactl", &["info"])'}},
            })
            named = setup_filters.evidence(packages)
            libraries, alone = setup_filters.audio_clients(named)
            self.assertEqual((libraries, alone), ({"player", "inner"}, {"probe", "strict"}))
            filter_ = setup_filters.audio_filter(libraries, alone)
            self.assertEqual(
                filter_, "rdeps(inner) or rdeps(player) or package(probe) or package(strict)")
            self.assertEqual(setup_filters.selected(packages, filter_),
                             {"player", "app", "inner", "probe", "strict"})

    def test_a_package_naming_the_catalog_outside_its_readers_is_found(self) -> None:
        owner = setup_filters.FONT_OWNER
        with tempfile.TemporaryDirectory() as directory:
            packages = written(Path(directory), {
                owner: {"files": {"src/lib.rs": 'var_os("TILO_FONT_ASSET_CACHE")'}},
                "toolkit": {"needs": {owner}, "files": {"src/fonts.rs": "tilo_font_catalog::locate()"}},
                "app": {"needs": {"toolkit"}, "files": {"src/main.rs": "fn main() {}"}},
                "stray": {"files": {"tests/draws.rs": 'var_os("TILO_FONT_ASSET_CACHE")'}},
            })
            named = setup_filters.evidence(packages)
            self.assertEqual(setup_filters.catalog_namers(named), {owner, "toolkit", "stray"})
            self.assertEqual(setup_filters.selected(packages, f"rdeps({owner})"),
                             {owner, "toolkit", "app"})
            found = setup_filters.problems(packages, named)
        self.assertTrue(
            any(problem.startswith("stray names the font catalog") for problem in found), found)
        # An allowance nothing needs any more is said too: it would excuse the next package of
        # that name.
        for carrier in setup_filters.CARRIERS:
            self.assertTrue(
                any(problem.startswith(f"CARRIERS excuses {carrier},") for problem in found), found)

    def test_a_filter_is_read_only_as_terms_over_workspace_packages(self) -> None:
        packages = {"a": {"directory": Path("a"), "dependencies": set()},
                    "b": {"directory": Path("b"), "dependencies": {"a"}}}
        self.assertEqual(setup_filters.selected(packages, "rdeps(a)"), {"a", "b"})
        self.assertEqual(setup_filters.selected(packages, "package(a)"), {"a"})
        # nextest matches a name no package has against nothing: the step would run for no one.
        with self.assertRaisesRegex(ValueError, "no workspace package is named gone"):
            setup_filters.selected(packages, "rdeps(a) or package(gone)")
        # A predicate this reading does not know selects it cannot say what.
        with self.assertRaisesRegex(ValueError, "not a list of rdeps"):
            setup_filters.selected(packages, "rdeps(a) and test(/audio/)")


if __name__ == "__main__":
    unittest.main()
