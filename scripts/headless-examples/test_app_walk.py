"""What the app walk reads and how it names a control: no session and no app is started.

The walk drives two versions of one app from one step file, so the file itself is
what can drift: an op the driver stopped having, a selector that names neither
version, a read-back that no longer finds the notes an app lists. Each is held
here, on the walk's own helpers and on every step file in `app-walks/`.
"""
import importlib.util
import json
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("app_walk", HERE / "app-walk.py")
walk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(walk)


def steps():
    """Every step file in `app-walks/`: the walks themselves, not the record
    of where a capability went, which `compare` reads as data."""
    for path in sorted((HERE / "app-walks").glob("*.json")):
        if path == walk.MOVED:
            continue
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
        _device, op = walk.split_on(op, None)
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
                starts = {json.dumps([walk.session_args_of(spec, variant, d["name"])
                                      for d in walk.devices_of(spec)])
                          for _p, spec in members if variant in spec}
                with self.subTest(family=family, variant=variant):
                    self.assertLessEqual(len(starts), 1)

    def test_every_device_a_step_names_is_one_the_walk_has(self):
        """An op on a device the walk never paired would fail on the run, after minutes of
        start: the step file is held to its own `devices` here."""
        for path, spec in steps():
            names = {d["name"] for d in walk.devices_of(spec)}
            for step in spec["steps"]:
                ops = step.get("ops") or []
                listed = [op for each in ops.values() for op in each] \
                    if isinstance(ops, dict) else ops
                said = [step.get("on")] + [op.get("on") for op in listed]
                for device in filter(None, said):
                    with self.subTest(walk=path.stem, step=step["id"], device=device):
                        self.assertIn(device, names - {""})

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


class TheSessionAWalkStartsIn(unittest.TestCase):
    """Every walk's own session starts from the environment app-walk.py gives it, a walk of
    one device included: that one has no device environment, and its start crashed on it."""

    def test_every_walk_starts_its_own_session_beside_its_local_services(self):
        state = Path("/walk-state")
        for path, spec in steps():
            with self.subTest(walk=path.stem):
                env = walk.first_device_env(state, spec)
                self.assertTrue(all(isinstance(k, str) and isinstance(v, str)
                                    for k, v in env.items()), env)
                self.assertEqual(env["TILO_PROJECTOR_RUNTIME_DIR"], str(state / "projector"))
                self.assertEqual(walk.LAN_NETWORKS_ENV in env,
                                 len(walk.devices_of(spec)) > 1, env)

    def test_a_walk_of_one_device_and_one_of_two_both_start(self):
        state = Path("/walk-state")
        one = walk.first_device_env(state, {"app": "one"})
        self.assertNotIn(walk.LAN_NETWORKS_ENV, one)
        two = walk.first_device_env(state, {"app": "two", "devices": ["laptop", "phone"]})
        self.assertEqual(two[walk.LAN_NETWORKS_ENV], "127.0.0.1")


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


class NoStepThatFailedIsExcused(unittest.TestCase):
    """Every step that does not walk fails its walk, whether or not main's
    baseline shares the failure: one excused there would read green on main, and
    neither its fix nor its getting worse would be seen."""

    def test_every_step_that_did_not_walk_fails_the_walk(self):
        now = found(("open", True, 900), ("add", False, 30000), ("delete", False, 10000), variant="tl")
        self.assertEqual([(s["version"], s["step"], s["why"]) for s in walk.unwalked(now)],
                         [("tl", "add", "not on screen"), ("tl", "delete", "not on screen")])

    def test_a_walk_that_walked_every_step_has_nothing_to_excuse(self):
        self.assertEqual(walk.unwalked(found(("open", True, 900), ("add", True, 400))), [])


class ABaselineHoldsTheSlowestOfMainsRuns(unittest.TestCase):
    """One quick run on an idle node must not make main's next ordinary run read
    as slower: the baseline keeps each step's slowest time main has shown."""

    def test_a_quick_run_does_not_lower_the_time_a_step_is_held_to(self):
        before = found(("open-agent", True, 5100), ("open-today", True, 3700))
        quick = found(("open-agent", True, 1200), ("open-today", True, 4000))
        kept = walk.rebaseline(quick, before)
        steps = kept["versions"]["rust"]["steps"]
        self.assertEqual((steps["open-agent"]["took_ms"], steps["open-today"]["took_ms"]), (5100, 4000))
        ordinary = found(("open-agent", True, 5000), ("open-today", True, 3900))
        self.assertEqual(walk.compare(ordinary, kept)["regressions"], [])

    def test_what_the_new_run_found_is_what_the_baseline_says(self):
        before = found(("open", True, 900), ("alt-tab", False, 10000))
        now = found(("open", False, 30000), ("alt-tab", True, 400))
        steps = walk.rebaseline(now, before)["versions"]["rust"]["steps"]
        self.assertEqual({k: (v["walked"], v["took_ms"]) for k, v in steps.items()},
                         {"open": (False, 30000), "alt-tab": (True, 400)})

    def test_a_real_slowdown_is_still_one(self):
        kept = walk.rebaseline(found(("open", True, 1300)), found(("open", True, 1000)))
        regressions = walk.compare(found(("open", True, 4000)), kept)["regressions"]
        self.assertEqual([r["step"] for r in regressions], ["open"])


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

    def test_deleting_the_version_a_person_got_holds_what_is_live_now_to_it(self):
        """The Rust shell goes and the tilolang one stays: an action only the Rust
        one published must be published by the tilolang one or named in the
        record, even though the tilolang one is held to its own baseline too."""
        banner = "so.tilo.session notifications.banners#snooze"
        before = self.catalogue("so.tilo.session desktop:shell#search", banner)
        before["versions"]["tl"] = self.catalogue(
            "so.tilo.session desktop:shell#search", variant="tl")["versions"]["tl"]
        now = self.catalogue("so.tilo.session desktop:shell#search", variant="tl")
        now["live"] = "tl"
        original, walk.moved = walk.moved, lambda: {}
        try:
            regressions = walk.compare(now, before)["regressions"]
        finally:
            walk.moved = original
        self.assertEqual([(r["version"], r["against"]) for r in regressions], [("tl", "rust")])
        self.assertIn(banner, regressions[0]["why"])

    def test_new_capabilities_are_never_a_regression(self):
        before = self.catalogue("so.tilo.tasks tasks.task#check")
        now = self.catalogue("so.tilo.tasks tasks.task#check", "so.tilo.tasks tasks.task#snooze")
        self.assertEqual(walk.compare(now, before)["regressions"], [])


class TheRecordOfWhereACapabilityWent(unittest.TestCase):
    """`app-walks/moved.json`: the change that moved or deleted a capability
    names its new place and a test that drives it there. A key without both
    would silence a regression the walk should still report."""

    def test_every_moved_capability_names_a_place_and_a_test(self):
        record = walk.moved()
        self.assertTrue(record, "the record of moved capabilities is written")
        for key, place in record.items():
            with self.subTest(key=key):
                self.assertRegex(key, r"^\S+ \S+#\S+$")
                self.assertIsInstance(place, dict)
                self.assertRegex(place.get("to", ""), r"^\S+ \S+#\S+$")
                self.assertRegex(place.get("test", ""), r"^[^:]+:\d+$")

    def test_every_named_test_is_in_the_repository(self):
        repo = HERE.parents[1]
        for key, place in walk.moved().items():
            with self.subTest(key=key):
                path, line = place["test"].rsplit(":", 1)
                where = repo / path
                self.assertTrue(where.is_file(), f"{path} does not exist")
                self.assertLessEqual(
                    int(line), len(where.read_text().splitlines()),
                    f"{path}:{line} is past the end")


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


class AWindowThatShowsNothing(unittest.TestCase):
    """A step leaves the app's window open: it has to show the app. A window that draws only
    its bar is a blank window, which no wait on a word of the app catches where the step's
    own state still draws the words it waited for."""

    class Session:
        def __init__(self, windows, targets):
            self.found = targets
            self.listed = windows

        def windows(self):
            return self.listed

        def targets(self):
            return self.found

    def walked(self, targets):
        session = self.Session(
            [{"app_id": "so.tilo.weather", "rect": [0, 64, 640, 500], "frames": 3}],
            targets)
        return walk.Walk({"tl": {"id": "so.tilo.weather"}, "steps": []}, "tl",
                         Path("/nonexistent"), Path("/nonexistent"), session=session)

    def test_a_window_showing_only_its_bar_is_a_blank_window(self):
        with self.assertRaisesRegex(walk.Failed, "blank window"):
            self.walked([{"name": "Close", "rect": [608, 64, 28, 28]}]).require_shown()

    def test_a_window_that_draws_the_app_is_not_blank(self):
        self.walked([{"name": "Add place", "rect": [16, 140, 90, 32]}]).require_shown()

    def test_a_window_that_is_not_open_is_nothing_to_check(self):
        walked = self.walked([])
        walked.session.listed = []
        walked.require_shown()


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


class ReadingWhatAnAppHolds(unittest.TestCase):
    def test_the_words_a_person_made_are_in_the_read(self):
        read = {"fields": {"messages": {"items": [{"body": "Hello team"}]}}}
        self.assertEqual(walk.held_in(read, ["Hello team"]), [])

    def test_the_read_names_what_it_does_not_hold(self):
        read = {"fields": {"messages": {"items": [{"body": "Hello team"}]}}}
        self.assertEqual(walk.held_in(read, ["Hello team", "Find the number"]),
                         ["Find the number"])

    def test_a_read_answer_that_names_no_words_is_missing_them(self):
        self.assertEqual(walk.held_in(None, ["Buy milk"]), ["Buy milk"])


class OpsOnAnotherDevice(unittest.TestCase):
    """Every op runs on the walk's first device unless it, or its step, names another: the
    one place a device is picked is `Walk.op`, which hands the op that device's session."""

    class Session:
        def __init__(self, name, targets=()):
            self.name = name
            self.found = [{"name": n, "rect": [0, 0, 10, 10]} for n in targets]
            self.state = Path("/nonexistent") / name

        def targets(self):
            return self.found

    def walk_of(self, spec=None):
        laptop = self.Session("laptop", ["Files"])
        phone = self.Session("phone", ["Accept"])
        spec = spec or {"devices": ["laptop", "phone"], "rust": {}, "steps": []}
        devices = walk.Devices("laptop", laptop)
        devices.order.append("phone")
        devices.sessions["phone"] = phone
        return walk.Walk(spec, "rust", Path("/nonexistent"), Path("/nonexistent"),
                         session=laptop, devices=devices), laptop, phone

    def test_an_op_runs_on_the_first_device_unless_it_names_another(self):
        walked, laptop, _phone = self.walk_of()
        self.assertEqual(walked.op({"must": ["Files"]}), {"saw": ["Files"]})
        self.assertEqual(walked.op({"on": "phone", "must": ["Accept"]}), {"saw": ["Accept"]})
        with self.assertRaises(walk.Failed):
            walked.op({"on": "phone", "must": {"see": ["Files"], "within": 0}})
        self.assertIs(walked.session, laptop)

    def test_the_first_device_is_the_walk_s_own_and_the_rest_are_further(self):
        walked, laptop, phone = self.walk_of()
        self.assertEqual(walked.devices.first, "laptop")
        self.assertEqual(walked.devices.further(), ["phone"])
        self.assertIs(walked.devices.session(None), laptop)
        self.assertIs(walked.devices.session("phone"), phone)

    def test_a_step_on_a_device_runs_each_op_there_unless_one_names_the_first(self):
        walked, laptop, _phone = self.walk_of()
        walked.step_on = "phone"
        self.assertEqual(walked.op({"must": ["Accept"]}), {"saw": ["Accept"]})
        self.assertEqual(walked.op({"on": "laptop", "must": ["Files"]}), {"saw": ["Files"]})
        self.assertIs(walked.session, laptop)

    def test_each_device_keeps_its_own_appearance_action(self):
        walked, _laptop, _phone = self.walk_of()
        walked.appearance = {"provider": "laptop's"}

        def finds():
            self.assertIsNone(walked.appearance)
            walked.appearance = {"provider": "phone's"}

        walked.on_device("phone", finds)
        self.assertEqual(walked.appearance, {"provider": "laptop's"})
        walked.on_device("phone", lambda: self.assertEqual(walked.appearance,
                                                           {"provider": "phone's"}))

    def test_a_device_the_walk_has_not_is_refused_by_name(self):
        walked, laptop, _phone = self.walk_of()
        with self.assertRaisesRegex(walk.Failed, "tablet"):
            walked.op({"on": "tablet", "must": ["Files"]})
        self.assertIs(walked.session, laptop)
        alone = walk.Walk({"rust": {}, "steps": []}, "rust", Path("/nonexistent"),
                          Path("/nonexistent"), session=laptop)
        with self.assertRaisesRegex(walk.Failed, "names no devices"):
            alone.op({"on": "phone", "must": ["Files"]})

    def test_a_device_starts_with_its_version_s_arguments_then_its_own(self):
        spec = {"devices": [{"name": "laptop", "session_args": ["--output", "B=1280x720"]},
                            "phone"],
                "rust": {}, "tl": {"session_args": ["--seed-apps"]}}
        self.assertEqual(walk.session_args_of(spec, "tl"),
                         ["--seed-apps", "--output", "B=1280x720"])
        self.assertEqual(walk.session_args_of(spec, "tl", "phone"), ["--seed-apps"])
        self.assertIsNone(walk.session_args_of(spec, "rust", "phone"))
        self.assertEqual(walk.devices_of({"rust": {}}), [{"name": ""}])


class WaitingForAnotherAppsActions(unittest.TestCase):
    """`wait_for: {"actions": {"app": ...}}` holds a step to the actions another app
    publishes. A window the shell restored beside the walk's own app is alive only
    when its app publishes them: a dead one publishes nothing, and a step that says
    it is alive must fail rather than pass on the walk's own app."""

    class Session:
        def __init__(self, rows):
            self.rows = rows

        def actions(self, _query=""):
            return self.rows

    @staticmethod
    def row(app, operation):
        return {"app_id": app,
                "action": {"address": {"resource": f"tl.{app}.Root", "action": operation}}}

    def walk_of(self, session):
        spec = {"rust": {"id": "so.tilo.calendar"}, "steps": []}
        return walk.Walk(spec, "rust", Path("/nonexistent"), Path("/nonexistent"),
                         session=session)

    def test_the_actions_counted_are_the_named_apps_and_not_the_walks_own(self):
        calendar = self.walk_of(self.Session([self.row("so.tilo.calendar", "new_event")]))
        self.assertEqual(calendar.op({"wait_for": "actions"}), {"actions": 1})

        restored = self.walk_of(self.Session([
            self.row("so.tilo.calendar", "new_event"),
            self.row("so.tilo.mail", "new_draft")]))
        self.assertEqual(
            restored.op({"wait_for": {"actions": {"app": "so.tilo.mail"}}}),
            {"actions": 1})

    def test_an_app_that_publishes_no_action_fails_the_step(self):
        calendar = self.walk_of(self.Session([self.row("so.tilo.calendar", "new_event")]))
        with self.assertRaisesRegex(walk.Failed, "so.tilo.mail to publish its actions"):
            calendar.op({"wait_for": {"actions": {"app": "so.tilo.mail", "within": 0.05}}})
        # A row that names the app but runs nothing is no action it publishes.
        listed_only = self.walk_of(self.Session([{"app_id": "so.tilo.mail", "action": None}]))
        with self.assertRaisesRegex(walk.Failed, "so.tilo.mail to publish its actions"):
            listed_only.op({"wait_for": {"actions": {"app": "so.tilo.mail", "within": 0.05}}})


class KeepingWhatAnAnswerSaid(unittest.TestCase):
    ANSWER = {"devices": [{"label": "laptop", "id": "a1"}, {"label": "phone", "id": "b2"}],
              "layout": {"revision": 7, "displays": [{"rect": [0, 0, 1920, 1080]}]}}

    def test_a_path_reads_keys_places_and_the_entry_a_field_names(self):
        self.assertEqual(walk.at_path(self.ANSWER, "layout.revision"), 7)
        self.assertEqual(walk.at_path(self.ANSWER, "layout.displays.0.rect"), [0, 0, 1920, 1080])
        self.assertEqual(walk.at_path(self.ANSWER, "devices[label=phone].id"), "b2")
        self.assertEqual(walk.at_path(self.ANSWER, "devices#len"), 2)

    def test_a_place_the_answer_lacks_is_none(self):
        self.assertIsNone(walk.at_path(self.ANSWER, "devices[label=tablet].id"))
        self.assertIsNone(walk.at_path(self.ANSWER, "layout.displays.3"))
        self.assertIsNone(walk.at_path(self.ANSWER, "layout.revision.x"))

    def test_a_kept_value_fills_args_keeping_its_type_when_alone(self):
        kept = {"rev": 7, "phone": "b2"}
        self.assertEqual(walk.filled({"revision": "{rev}", "display": "{phone}/edid",
                                      "xs": ["{rev}"]}, kept),
                         {"revision": 7, "display": "b2/edid", "xs": [7]})


class PickingWhatAnActionOffers(unittest.TestCase):
    def test_the_first_answer_offered_per_parameter(self):
        row = {"action": {"params": [
            {"name": "destination", "schema": {"type": "string", "enum": ["a1", "b2"]}},
            {"name": "mode", "schema": {"oneOf": [{"const": "copy"}, {"const": "move"}]}},
            {"name": "note", "schema": {"type": "string"}}]}}
        self.assertEqual(walk.choices_of(row), {"destination": "a1", "mode": "copy"})

    def test_a_parameter_whose_choices_the_action_names_is_read_by_them(self):
        # The catalogue carries what an action offers as the parameter's `choices` (a nearby
        # device's handle, each with the words it reads by), not only in its schema.
        row = {"action": {"params": [
            {"name": "target", "choices": ["0a1b", "0c2d"],
             "schema": {"type": "string"}}]}}
        self.assertEqual(walk.choices_of(row), {"target": "0a1b"})


class AValuePerVersion(unittest.TestCase):
    def test_a_value_both_versions_share_is_itself(self):
        self.assertEqual(walk.pick("Trip", "tl"), "Trip")

    def test_a_value_named_per_version_is_that_version_s(self):
        self.assertEqual(walk.pick({"rust": "new-note", "tl": "v7cub7aw\x1fqymo5lg2"}, "tl"),
                         "v7cub7aw\x1fqymo5lg2")

    def test_a_value_one_version_has_no_answer_for_is_refused(self):
        with self.assertRaises(walk.Failed):
            walk.pick({"rust": "new-note"}, "tl")


class ReadingAnAnswer(unittest.TestCase):
    def test_a_dotted_path_reads_through_dicts_and_lists(self):
        answer = {"theme": "dark", "fonts": {"faces": ["a", "b"]}, "rows": [{"id": 7}]}
        self.assertEqual(walk.at_path(answer, "theme"), "dark")
        self.assertEqual(walk.at_path(answer, "fonts.faces.1"), "b")
        self.assertEqual(walk.at_path(answer, "rows.0.id"), 7)

    def test_a_len_path_counts_what_it_lists(self):
        self.assertEqual(walk.at_path({"rows": [1, 2, 3]}, "rows#len"), 3)
        self.assertEqual(walk.at_path({"rows": []}, "rows#len"), 0)

    def test_a_path_that_is_not_there_reads_none(self):
        self.assertIsNone(walk.at_path({"theme": "dark"}, "mode"))
        self.assertIsNone(walk.at_path({"theme": "dark"}, "theme.deeper"))
        self.assertIsNone(walk.at_path("dark", "theme"))


class HoldingAReadToWhatWasChosen(unittest.TestCase):
    """A read's answer is the app's own: a walk holds the choices a person made
    to the paths that carry them, or to words in a document it reads."""

    class Session:
        def windows(self):
            return []

        def invoke(self, provider, resource, action, args=None):
            return {"Completed": {"output": {"value": {
                "theme": "dusk", "mode": "dark", "rows": [{"title": "a"}]}}}}

    def walker(self):
        spec = {"rust": {"id": "so.tilo.settings"}}
        return walk.Walk(spec, "rust", Path("/nonexistent-out"), Path("/nonexistent-state"),
                         session=self.Session())

    def test_the_paths_named_read_what_was_chosen(self):
        found = self.walker().op_read({"do": ["so.tilo.settings", "settings.root",
                                              "inspect-appearance"],
                                       "is": {"theme": "dusk", "mode": "dark",
                                              "rows#len": 1}})
        self.assertEqual(found["is"], {"theme": "dusk", "mode": "dark", "rows#len": 1})

    def test_an_answer_that_reads_something_else_is_the_finding(self):
        with self.assertRaises(walk.Failed):
            self.walker().op_read({"do": ["so.tilo.settings", "settings.root",
                                          "inspect-appearance"],
                                   "is": {"mode": "light"}})

    def test_words_in_the_whole_answer_hold_a_document_read(self):
        found = self.walker().op_read({"do": ["so.tilo.calendar", "calendar.calendar", "read"],
                                       "contains": "dusk"})
        self.assertEqual(found["contains"], "dusk")
        with self.assertRaises(walk.Failed):
            self.walker().op_read({"do": ["so.tilo.calendar", "calendar.calendar", "read"],
                                   "contains": "Walks walk"})


class CountingAnOpenWindowsFrames(unittest.TestCase):
    """A choice reaches an app that is already open on its next frame; a window
    that keeps its old frame is a person looking at what did not change."""

    class Session:
        def __init__(self, app_id="so.tilo.settings"):
            self.app_id = app_id
            self.drawn = 4

        def windows(self):
            return [{"app_id": self.app_id, "frames": self.drawn}]

    def walker(self, session):
        spec = {"rust": {"id": "so.tilo.settings"}}
        return walk.Walk(spec, "rust", Path("/nonexistent-out"), Path("/nonexistent-state"),
                         session=session)

    def briefly(self):
        """The walk's waits cut to a tenth of a second: nothing in this test can
        take the twenty a window has to draw again."""
        original = walk.until_value
        walk.until_value = lambda what, check, deadline=30.0, step=0.1: original(
            what, check, 0.3, 0.02)
        return original

    def test_two_calls_report_the_frames_and_whether_the_window_drew_again(self):
        session = self.Session()
        walker = self.walker(session)
        self.assertEqual(walker.op_window_frames(True), {"frames": 4})
        session.drawn = 5
        self.assertEqual(walker.op_window_frames(True), {"frames": 5, "drew_again": True})

    def test_a_window_that_does_not_draw_again_is_the_finding(self):
        session = self.Session()
        walker = self.walker(session)
        walker.op_window_frames(True)
        original = self.briefly()
        try:
            with self.assertRaises(walk.Failed):
                walker.op_window_frames(True)
        finally:
            walk.until_value = original

    def test_a_window_that_is_not_open_cannot_be_counted(self):
        with self.assertRaises(walk.Failed):
            self.walker(self.Session(app_id="so.tilo.notes")).op_window_frames(True)


class FindingAnAction(unittest.TestCase):
    ROWS = [
        {"provider": "so.tilo.session", "app_id": "so.tilo.session",
         "action": {"address": {"resource": "desktop:chrome", "action": "new-strip"}}},
        {"provider": "workspace:w1", "app_id": "so.tilo.shortcuts",
         "action": {"address": {"resource": "shortcuts", "action": "create"}}},
        {"provider": "workspace:w1", "app_id": "so.tilo.shortcuts",
         "resource": {"id": "app.shortcuts"}},
    ]

    def test_the_row_is_found_by_what_it_does_not_by_who_lists_it(self):
        found = walk.row_for(self.ROWS, "shortcuts", "create")
        self.assertEqual(found["provider"], "workspace:w1")

    def test_a_resource_row_with_no_action_is_not_an_action(self):
        self.assertIsNone(walk.row_for(self.ROWS, "app.shortcuts", "open"))

    def test_an_action_the_desktop_does_not_list_is_none(self):
        self.assertIsNone(walk.row_for(self.ROWS, "shortcuts", "delete"))

    def test_a_kind_finds_a_row_whose_id_the_run_minted(self):
        rows = [{"provider": "so.tilo.session",
                 "resource": {"id": "tl:so.tilo.shell:root-7ea1", "kind": "tl.so.tilo.shell.Viewing"},
                 "action": {"address": {"resource": "tl:so.tilo.shell:root-7ea1",
                                        "action": "show_style"}}}]
        found = walk.row_for(rows, "tl.so.tilo.shell.Viewing", "show_style",
                             kind="tl.so.tilo.shell.Viewing")
        self.assertEqual(found["resource"]["id"], "tl:so.tilo.shell:root-7ea1")


class ReadingAProvidersAddresses(unittest.TestCase):
    def test_each_printed_address_is_named_by_its_line(self):
        said = ("OPENAI_ISSUER=http://127.0.0.1:9/auth\n"
                "CHATGPT_BACKEND=http://127.0.0.1:9/backend-api/codex\n"
                "OPENAI_API=http://127.0.0.1:9/openai/v1\n"
                "ANTHROPIC_API=http://127.0.0.1:9/anthropic/v1\n")
        self.assertEqual(walk.provider_addresses(said), {
            "openai-issuer": "http://127.0.0.1:9/auth",
            "chatgpt-backend": "http://127.0.0.1:9/backend-api/codex",
            "openai-api": "http://127.0.0.1:9/openai/v1",
            "anthropic-api": "http://127.0.0.1:9/anthropic/v1",
        })


class NamingWhatTheRunMade(unittest.TestCase):
    def test_a_served_value_is_what_the_run_answered_under_that_name(self):
        served = {"provider": {"id": "6d0f"}, "openai-api": "http://127.0.0.1:9/openai/v1"}
        self.assertEqual(walk.resolved(served, {"served": "provider.id"}), "6d0f")

    def test_a_step_s_value_is_resolved_all_the_way_down(self):
        served = {"provider": {"id": "6d0f"}}
        value = {"provider": {"served": "provider.id"}, "names": ["a", {"served": "provider.id"}]}
        self.assertEqual(walk.resolved(served, value),
                         {"provider": "6d0f", "names": ["a", "6d0f"]})

    def test_a_name_the_run_never_answered_is_a_step_s_failure(self):
        with self.assertRaises(walk.Failed):
            walk.resolved({}, {"served": "provider.id"})


class CountingControls(unittest.TestCase):
    TARGETS = [{"name": "Strips", "rect": [0, 0, 80, 24]},
               {"name": "Strips", "rect": [0, 24, 80, 24]},
               {"name": "Settings for Strips", "rect": [0, 48, 80, 24]},
               {"name": "note-7f3a", "identity": "k7f\x1fdpj7scfe", "rect": [0, 0, 200, 24]}]

    def test_a_name_counts_the_controls_drawn_with_it(self):
        self.assertEqual(sum(walk.matches(t, "Strips") for t in self.TARGETS), 2)

    def test_a_prefix_counts_the_rows_named_by_an_id(self):
        self.assertEqual(sum(walk.matches(t, {"prefix": "note-"}) for t in self.TARGETS), 1)

    def test_an_identity_or_what_a_control_does_names_one(self):
        self.assertTrue(walk.matches(self.TARGETS[3], "k7f\x1fdpj7scfe"))
        self.assertTrue(walk.matches(self.TARGETS[3], {"doing": "dpj7scfe"}))


class ServingAPageAndAPicture(unittest.TestCase):
    """What the Browser and Photos walks need from the machine itself: a page on
    this machine's loopback, because a browser refuses a file path, and a real
    picture, because a photograph is not a text file."""

    def walk(self, out):
        return walk.Walk({"app": "browser", "rust": {"id": "so.tilo.browser"}},
                         "rust", out, out / "state")

    def test_a_page_is_served_on_this_machine_s_loopback(self):
        import tempfile
        import urllib.request
        out = Path(tempfile.mkdtemp(prefix="app-walk-pages-"))
        w = self.walk(out)
        try:
            served = w.op_serve({"pages": {"dir": "crates/engines/chromium-host/test-pages",
                                            "page": "links.html"}})
            body = urllib.request.urlopen(served["pages"], timeout=10).read().decode()
            self.assertIn("Second section", body)
            self.assertEqual(w.served["pages"], served["pages"])
        finally:
            w.stop_services()

    def test_the_picture_a_photograph_walk_imports_is_a_png(self):
        import tempfile
        out = Path(tempfile.mkdtemp(prefix="app-walk-image-"))
        found = self.walk(out).op_image({"name": "beach.png"})
        self.assertTrue(found["image"].endswith("beach.png"))
        self.assertTrue(Path(found["image"]).read_bytes().startswith(b"\x89PNG"))


class HoldingEachOpenToTheBar(unittest.TestCase):
    """Every open in a walk that starts its app, read from the session's launch timings and held
    to the open bar: the session's log is all a walk's verdict reads, so the log here is one a
    session writes."""

    APP = "so.tilo.lang.gallery"

    class Session:
        def __init__(self, log):
            self.text = log

        def log(self):
            return self.text

    @staticmethod
    def line(at: str, message: str) -> str:
        """A line the session logs `at` seconds from now: after the walk asked, as the session's
        lines of an open always are."""
        from datetime import datetime, timezone
        when = datetime.fromtimestamp(time.time() + int(at), timezone.utc)
        return f"{when.strftime('%Y-%m-%dT%H:%M:%S.%f')}Z  INFO tilo::launch_latency: {message}"

    def opened(self, launch: int, at: str, spawn_us: int, present_us: int | None,
               app: str = APP, generation: int = 41) -> list[str]:
        named = f"launch_id={launch} launch_generation={generation} package_id={app}"
        lines = [self.line(at, f"application launch requested {named}"),
                 self.line(at, f"application launcher spawned {named} pid=7 "
                               f"request_to_spawn_us={spawn_us}"),
                 self.line(at, f"application toplevel mapped {named} spawn_to_map_us=31000")]
        if present_us is not None:
            lines.append(self.line(at, f"application first frame presented {named} "
                                       f"request_to_first_present_us={present_us}"))
        return lines

    def walk_with(self, lines: list[str]):
        spec = {"tl": {"id": self.APP, "package": "gallery"}, "steps": []}
        walked = walk.Walk(spec, "tl", Path("/nonexistent"), Path("/nonexistent"),
                           session=self.Session("\n".join(lines)))
        # The walk began before every line here.
        walked.began_epoch = 0
        return walked

    @staticmethod
    def timed(opens: list[dict]) -> list[tuple]:
        return [(o["open"], o.get("ms"), o["spawn_ms"], o.get("map_ms")) for o in opens]

    def held(self, *walked) -> tuple[int, str, list]:
        """The `app-walk/open` check over walks that recorded these opens, as their checks keep
        them: its verdict, its page and its rows."""
        import tempfile
        root = Path(tempfile.mkdtemp(prefix="app-walk-opens-"))
        for index, opens in enumerate(walked):
            where = root / f"walk-{index}"
            where.mkdir()
            (where / "walk.json").write_text(json.dumps(
                {"walk": "gallery", "variants": {"tl": {"app": self.APP, "opens": opens}}}))
        verdict = walk.hold_opens(root, root / "open")
        return (verdict, (root / "open" / "opens.md").read_text(),
                json.loads((root / "open" / "opens.json").read_text()))

    def test_the_first_open_is_cold_and_each_later_one_follows_a_quit(self):
        walked = self.walk_with(self.opened(1, "01", 9000, 80000)
                                + self.opened(2, "05", 8000, 95000)
                                + self.opened(3, "09", 7000, 60000, app="so.tilo.notes"))
        self.assertEqual(self.timed(walked.opens()),
                         [("cold", 80, 9, 31), ("after a quit", 95, 8, 31)])
        verdict, _page, rows = self.held(walked.opens())
        self.assertEqual((verdict, len(rows)), (0, 2))

    def test_a_restarted_session_s_launches_are_not_taken_for_earlier_ones(self):
        walked = self.walk_with(self.opened(1, "01", 9000, 80000)
                                + self.opened(1, "05", 8000, 95000, generation=42))
        self.assertEqual([(o["open"], o["ms"]) for o in walked.opens()],
                         [("cold", 80), ("after a quit", 95)])

    def test_each_open_says_how_busy_the_machine_was(self):
        walked = self.walk_with(self.opened(1, "01", 9000, 80000))
        walked.asking()
        opened, = walked.opens()
        self.assertGreaterEqual(opened["cpus"], 1)
        self.assertGreaterEqual(opened["load"], 0)
        self.assertIn(f"load {opened['load']} on {opened['cpus']} cores", walk.open_words(opened))

    def test_an_open_over_the_bar_is_red_with_its_numbers(self):
        walked = self.walk_with(self.opened(1, "01", 9000, 80000)
                                + self.opened(2, "05", 41000, 412000))
        verdict, page, _rows = self.held(walked.opens())
        self.assertEqual(verdict, 1)
        self.assertIn("| after a quit | 412 ms | 412 ms over the bar (launcher started after 41 ms",
                      page)
        self.assertIn(f"against the {walk.OPEN_BAR_MS} ms bar", page)

    def test_an_open_never_shown_is_red(self):
        walked = self.walk_with(self.opened(1, "01", 9000, None))
        verdict, page, _rows = self.held(walked.opens())
        self.assertEqual(verdict, 1)
        self.assertIn("| cold | never shown | never shown (launcher started after 9 ms", page)

    def test_an_open_asked_for_and_not_in_the_timings_is_red_not_a_pass(self):
        walked = self.walk_with([])
        self.assertEqual(walked.opens(), [])
        walked.asked_open = True
        verdict, page, _rows = self.held(walked.opens())
        self.assertEqual(verdict, 1)
        self.assertIn("record no open of it", page)

    def test_walks_that_recorded_no_open_are_red_not_a_pass(self):
        self.assertEqual(self.held([])[0], 1)

    def test_each_walk_s_opens_are_rows_of_the_one_check(self):
        cold = self.walk_with(self.opened(1, "01", 9000, 70000)).opens()
        verdict, _page, rows = self.held(cold, cold)
        self.assertEqual((verdict, len(rows)), (0, 2))

    def test_the_walk_s_page_lists_each_open_against_the_bar(self):
        import tempfile
        out = Path(tempfile.mkdtemp(prefix="app-walk-test-"))
        walked = self.walk_with(self.opened(1, "01", 9000, 412000))
        report = {"walk": "gallery", "steps": [],
                  "variants": {"tl": {"app": self.APP, "session_start_ms": 900, "steps": [],
                                      "opens": walked.opens()}}}
        page = walk.write_report(report, out).read_text()
        self.assertIn("**tl** cold: 412 ms over the bar", page)

if __name__ == "__main__":
    unittest.main()
