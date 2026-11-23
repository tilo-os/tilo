"""What the app walk reads and how it names a control: no session and no app is started.

The walk drives two versions of one app from one step file, so the file itself is
what can drift: an op the driver stopped having, a selector that names neither
version, a read-back that no longer finds the notes an app lists. Each is held
here, on the walk's own helpers and on every step file in `app-walks/`.
"""
import importlib.util
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("app_walk", HERE / "app-walk.py")
walk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(walk)


def steps():
    for path in sorted((HERE / "app-walks").glob("*.json")):
        yield path, json.loads(path.read_text())


def ops_of(step):
    """Each op of a step, in either form: one list for both versions, or one
    list per version where the same errand is reached differently. Yields the
    version (None for a shared op), the op's name and its value."""
    ops = step.get("ops") or []
    if isinstance(ops, dict):
        for variant, listed in ops.items():
            for name, value in from_key(listed):
                yield variant, name, value
    else:
        for name, value in from_key(ops):
            yield None, name, value


def named_variants(spec):
    """The versions a step file drives: both while the Rust app still exists,
    only `tl` after the change that retires it leaves its entry out."""
    return [variant for variant in walk.VARIANTS if variant in spec]


def from_key(ops):
    for op in ops:
        (name, value), = op.items()
        yield name, value


class TheStepFiles(unittest.TestCase):
    def test_every_walk_names_the_versions_it_drives_and_their_apps(self):
        for path, spec in steps():
            with self.subTest(walk=path.stem):
                named = named_variants(spec)
                self.assertTrue(named, f"{path.stem} names no version")
                self.assertEqual(spec["app"], path.stem)
                for variant in named:
                    v = spec[variant]
                    # An app's version names the app it opens; a shell's names
                    # how its session starts, or nothing for the default one.
                    if "id" in v:
                        self.assertTrue(v["id"].startswith("so.tilo."))
                    else:
                        self.assertLessEqual(set(v), {"source", "session_args"})

    def test_every_walk_is_a_check_t_runs(self):
        """A walk t does not run guards nothing: each family, and each walk in
        no family, is in the `app-walk/{app}` matrix; each baseline belongs to a
        walk; and the walks of one family start their sessions the same way, so
        one session per version can walk them all."""
        import re
        toml = (HERE.parents[1] / "t.toml").read_text()
        block = toml[toml.index('name = "app-walk/{app}"'):]
        listed = set(re.findall(r'"([a-z0-9-]+)"',
                                re.search(r"^app = \[(.*?)\]", block, re.M).group(1)))
        self.assertEqual(listed, set(walk.checks()))
        files = {path.stem for path, _spec in steps()}
        self.assertFalse(listed & files - set(walk.checks()))
        for baseline in (HERE / "app-walks" / "baseline").glob("*.json"):
            self.assertIn(baseline.stem, files)
        for family in walk.checks():
            members = walk.family_of(family)
            for variant in walk.VARIANTS:
                starts = {json.dumps(spec[variant].get("session_args")) for _p, spec in members
                          if variant in spec}
                with self.subTest(family=family, variant=variant):
                    self.assertLessEqual(len(starts), 1)

    def test_every_op_is_one_the_driver_has(self):
        driver = [name[len("op_"):] for name in dir(walk.Walk) if name.startswith("op_")]
        for path, spec in steps():
            for step in spec["steps"]:
                for variant, name, _value in ops_of(step):
                    with self.subTest(walk=path.stem, step=step["id"], variant=variant, op=name):
                        self.assertIn(name, driver)

    def test_every_step_says_what_a_person_is_doing(self):
        for path, spec in steps():
            for step in spec["steps"]:
                with self.subTest(walk=path.stem, step=step["id"]):
                    self.assertTrue(step["said"].strip())
                    self.assertTrue(step["id"].strip())

    def test_a_value_that_differs_between_versions_names_each_of_them(self):
        """A selector written for one version alone would press nothing in the
        other, and the walk would report a missing control where there is one.
        A step with several controls to try names each of them for the versions
        it is written for, and one for every version the walk drives."""
        for path, spec in steps():
            named = named_variants(spec)
            for step in spec["steps"]:
                for variant, name, value in ops_of(step):
                    if name in ("press_any", "offered"):
                        for each in named if variant is None else [variant]:
                            found = [alternative for alternative in value
                                     if walk.alternative(alternative, each) is not None]
                            with self.subTest(walk=path.stem, step=step["id"], op=name,
                                              variant=each):
                                self.assertTrue(found, f"{name} in {step['id']} offers "
                                                       f"nothing in the {each} version")
                    else:
                        self.assert_named_both(path, step, name, value, named)

    def assert_named_both(self, path, step, op, value, named):
        if isinstance(value, dict) and set(value) & set(walk.VARIANTS):
            with self.subTest(walk=path.stem, step=step["id"], op=op):
                self.assertLessEqual(set(named), set(value),
                                     f"{op} in {step['id']} names only some versions")
        elif isinstance(value, list):
            for inner in value:
                self.assert_named_both(path, step, op, inner, named)
        elif isinstance(value, dict):
            for key, inner in value.items():
                if key in ("control", "selectors"):
                    self.assert_named_both(path, step, op, inner, named)


class ThePageItWrites(unittest.TestCase):
    def test_a_step_shows_both_versions_snapshots_and_what_each_found(self):
        import tempfile
        out = Path(tempfile.mkdtemp(prefix="app-walk-test-"))
        report = {
            "walk": "notes",
            "steps": [{"id": "open", "said": "Open Notes on an empty notebook"}],
            "variants": {
                "rust": {"app": "so.tilo.notes", "session_start_ms": 1200, "steps": [dict(
                    id="open", said="Open Notes on an empty notebook",
                    ops=[{"launch": True, "found": {"count": 0}}],
                    shot=str(out / "shots/rust/01-open.png"))]},
                "tl": {"app": "so.tilo.notes", "session_start_ms": 900, "steps": [dict(
                    id="open", said="Open Notes on an empty notebook",
                    ops=[{"launch": True, "found": {"notes": None}}],
                    shot=str(out / "shots/tl/01-open.png"),
                    failed="no control for it")]},
            },
        }
        page = walk.write_report(report, out).read_text()
        self.assertIn("shots/rust/01-open.png", page)
        self.assertIn("shots/tl/01-open.png", page)
        self.assertIn("0 held", page)
        self.assertIn("no control for it", page.replace("stopped: no control for it", "no control for it"))


def found(*steps, live="rust", variant="rust"):
    """A summary as the walk writes one: `steps` are (id, walked, took_ms[, controls])."""
    return {"walk": "switcher", "live": live, "commit": "c0ffee", "versions": {variant: {
        "steps": {s[0]: {"walked": s[1], "took_ms": s[2],
                         **({"controls": s[3]} if len(s) > 3 else {}),
                         **({} if s[1] else {"failed": "not on screen"})}
                  for s in steps}}}}


class HoldingAWalkToItsBaseline(unittest.TestCase):
    def test_the_same_walk_finds_nothing(self):
        before = found(("open", True, 800), ("alt-tab", True, 300))
        self.assertEqual(walk.compare(before, before)["regressions"], [])

    def test_a_step_main_walked_and_this_tree_does_not_is_a_regression(self):
        before = found(("open", True, 800), ("alt-tab", True, 300))
        now = found(("open", True, 800), ("alt-tab", False, 300))
        regressions = walk.compare(now, before)["regressions"]
        self.assertEqual([r["step"] for r in regressions], ["alt-tab"])
        self.assertIn("not on screen", regressions[0]["why"])

    def test_a_step_main_could_not_walk_either_is_not_one(self):
        before = found(("open", True, 800), ("people", False, 100))
        now = found(("open", True, 800), ("people", False, 100))
        self.assertEqual(walk.compare(now, before)["regressions"], [])

    def test_a_step_noticeably_slower_is_a_regression_and_noise_is_not(self):
        before = found(("open", True, 800), ("alt-tab", True, 300))
        noisy = found(("open", True, 1500), ("alt-tab", True, 900))
        self.assertEqual(walk.compare(noisy, before)["regressions"], [])
        slow = found(("open", True, 4000), ("alt-tab", True, 300))
        self.assertEqual([r["step"] for r in walk.compare(slow, before)["regressions"]],
                         ["open"])

    def test_the_replacement_is_held_to_what_a_person_got_on_main(self):
        """The tilolang shell, after the Rust one is deleted, is held to the
        Rust shell's baseline: the walk is of the switcher, not of a binary."""
        before = found(("open", True, 800), ("alt-tab", True, 300), live="rust")
        now = found(("open", True, 700), ("alt-tab", False, 300), live="tl", variant="tl")
        comparison = walk.compare(now, before)
        self.assertEqual(comparison["held_to"], {"tl": "rust"})
        self.assertEqual([r["step"] for r in comparison["regressions"]], ["alt-tab"])

    def test_controls_gone_are_listed_only_against_the_same_version(self):
        before = found(("open", True, 800, ["Launcher", "Search"]))
        now = found(("open", True, 800, ["Launcher"]))
        self.assertEqual(walk.compare(now, before)["controls_gone"][0]["controls"], ["Search"])
        other = found(("open", True, 800, ["Apps"]), variant="tl")
        self.assertEqual(walk.compare(other, before)["controls_gone"], [])

    def test_a_retired_version_is_not_walked_and_the_live_one_is_what_is_left(self):
        spec = {"rust": {"native": "no-such-app-anywhere"}, "tl": {"package": "notes"},
                "live": "rust"}
        self.assertFalse(walk.present(spec, "rust"))
        self.assertTrue(walk.present(spec, "tl"))
        self.assertEqual(walk.live_of(spec, ["tl"]), "tl")


class HoldingTheCapabilitiesToMain(unittest.TestCase):
    """Every action main published is still published, or the change that took
    it away names where it lives now (core.md, "The rule for every piece")."""

    def catalogue(self, *keys, variant="rust"):
        summary = found(("catalogue", True, 100), variant=variant)
        summary["versions"][variant]["steps"]["catalogue"]["catalogue"] = {
            key: {"label": key.rsplit("#", 1)[-1], "params": []} for key in keys}
        return summary

    def test_a_capability_that_went_with_no_place_named_is_a_regression(self):
        before = self.catalogue("so.tilo.tasks tasks.task#check", "so.tilo.tasks tasks.task#due-today")
        now = self.catalogue("so.tilo.tasks tasks.task#check")
        original, walk.moved = walk.moved, lambda: {}
        try:
            regressions = walk.compare(now, before)["regressions"]
        finally:
            walk.moved = original
        self.assertEqual(len(regressions), 1)
        self.assertIn("so.tilo.tasks tasks.task#due-today", regressions[0]["why"])

    def test_a_capability_moved_to_its_new_place_is_kept(self):
        before = self.catalogue("so.tilo.tasks tasks.task#check")
        now = self.catalogue("so.tilo.lang.tasks tl.so.tilo.lang.tasks.Task#check", variant="tl")
        original = walk.moved
        walk.moved = lambda: {"so.tilo.tasks tasks.task#check": {
            "to": "so.tilo.lang.tasks tl.so.tilo.lang.tasks.Task#check", "test": "walk/tasks"}}
        try:
            self.assertEqual(walk.compare(now, before)["regressions"], [])
        finally:
            walk.moved = original

    def test_new_capabilities_are_never_a_regression(self):
        before = self.catalogue("so.tilo.tasks tasks.task#check")
        now = self.catalogue("so.tilo.tasks tasks.task#check", "so.tilo.tasks tasks.task#snooze")
        self.assertEqual(walk.compare(now, before)["regressions"], [])


class NamingAControl(unittest.TestCase):
    TARGETS = [
        {"name": "new-note", "rect": [0, 0, 20, 20]},
        {"name": "Import Markdown", "rect": [0, 30, 80, 20]},
        {"name": "Something typed", "identity": "mavvhka7\x1fdpj7scfe", "rect": [10, 10, 1, 1]},
    ]

    class Session:
        def __init__(self, targets):
            self.found = targets

        def targets(self):
            return self.found

    def test_a_name_finds_the_control_that_answers_to_it(self):
        found = walk.control(self.Session(self.TARGETS), "new-note")
        self.assertEqual(found["name"], "new-note")

    def test_an_identity_finds_the_control_that_does_what_it_names(self):
        found = walk.control(self.Session(self.TARGETS), "mavvhka7\x1fdpj7scfe")
        self.assertEqual(found["identity"], "mavvhka7\x1fdpj7scfe")

    def test_a_control_that_is_not_there_is_none_not_a_wrong_one(self):
        self.assertIsNone(walk.control(self.Session(self.TARGETS), "new-note-under"))
        self.assertIsNone(walk.control(self.Session(self.TARGETS), "nomavvhka7\x1fnothing"))

    def test_a_row_the_app_names_by_its_id_is_found_by_the_start_of_its_name(self):
        rows = [{"name": "note-7f3a", "rect": [0, 0, 200, 24]},
                {"name": "note-9c11", "rect": [0, 24, 200, 24]}]
        found = walk.control(self.Session(rows), {"prefix": "note-"})
        self.assertEqual(found["name"], "note-7f3a")


class CountingNotes(unittest.TestCase):
    def test_a_tilolang_read_counts_with_the_count_it_gives(self):
        read = {"fields": {"notes": {"count": 2, "items": [
            {"type": "tl.so.tilo.notes.Note", "title": "a"},
            {"type": "tl.so.tilo.notes.Note", "title": "b"}]}}}
        self.assertEqual(walk.notes_held(read), 2)

    def test_a_native_read_counts_the_pages_it_lists(self):
        read = {"fields": {"pages": {"items": [
            {"type": "Page", "title": "one"}, {"type": "Page", "title": "two"},
            {"type": "Page", "title": "three"}]}}}
        self.assertEqual(walk.notes_held(read), 3)

    def test_a_notebook_of_no_notes_counts_none(self):
        self.assertEqual(walk.notes_held({"fields": {"notes": {"count": 0, "items": []}}}), 0)

    def test_a_read_with_no_notes_listed_counts_nothing_it_does_not_see(self):
        self.assertIsNone(walk.notes_held({"fields": {"title": "Notes"}}))


class AValuePerVersion(unittest.TestCase):
    def test_a_value_both_versions_share_is_itself(self):
        self.assertEqual(walk.pick("Trip", "tl"), "Trip")

    def test_a_value_named_per_version_is_that_version_s(self):
        self.assertEqual(walk.pick({"rust": "new-note", "tl": "v7cub7aw\x1fqymo5lg2"}, "tl"),
                         "v7cub7aw\x1fqymo5lg2")

    def test_a_value_one_version_has_no_answer_for_is_refused(self):
        with self.assertRaises(walk.Failed):
            walk.pick({"rust": "new-note"}, "tl")


if __name__ == "__main__":
    unittest.main()
