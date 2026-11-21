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


def from_key(ops):
    for op in ops:
        (name, value), = op.items()
        yield name, value


class TheStepFiles(unittest.TestCase):
    def test_every_walk_names_both_versions_and_their_apps(self):
        for path, spec in steps():
            with self.subTest(walk=path.stem):
                self.assertEqual(set(walk.VARIANTS) & set(spec), set(walk.VARIANTS))
                self.assertEqual(spec["app"], path.stem)
                for variant in walk.VARIANTS:
                    self.assertTrue(spec[variant]["id"].startswith("so.tilo."))

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

    def test_a_value_that_differs_between_versions_names_both(self):
        """A selector written for one version alone would press nothing in the
        other, and the walk would report a missing control where there is one.
        A step with several controls to try names each of them for the versions
        it is written for, and one for every version."""
        for path, spec in steps():
            for step in spec["steps"]:
                for variant, name, value in ops_of(step):
                    if name in ("press_any", "offered"):
                        for each in walk.VARIANTS if variant is None else [variant]:
                            found = [alternative for alternative in value
                                     if walk.alternative(alternative, each) is not None]
                            with self.subTest(walk=path.stem, step=step["id"], op=name,
                                              variant=each):
                                self.assertTrue(found, f"{name} in {step['id']} offers "
                                                       f"nothing in the {each} version")
                    else:
                        self.assert_named_both(path, step, name, value)

    def assert_named_both(self, path, step, op, value):
        if isinstance(value, dict) and set(value) & set(walk.VARIANTS):
            with self.subTest(walk=path.stem, step=step["id"], op=op):
                self.assertLessEqual(set(walk.VARIANTS), set(value),
                                     f"{op} in {step['id']} names only some versions")
        elif isinstance(value, list):
            for inner in value:
                self.assert_named_both(path, step, op, inner)
        elif isinstance(value, dict):
            for key, inner in value.items():
                if key in ("control", "selectors"):
                    self.assert_named_both(path, step, op, inner)


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
