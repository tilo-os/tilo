"""The allowance-marker check: a marker goes only with the line it covers."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import re
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from git_fixture import isolated_git_environment

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_allowance_markers", REPO / "tools/policy/check_allowance_markers.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

AUDIT = 'Pattern::new(\n    "raw_paint",\n    r"scene\\.fill\\(",\n    "raw paint",\n),\n'


def drawing(marker: bool) -> str:
    """A drawing function, with or without the allowance its `scene.fill` call needs."""
    allowance = (
        "    // ratchet-allow(raw_paint): the back of a playing card is the picture of a card\n"
        if marker
        else ""
    )
    return f"fn draw(scene: &mut Scene) {{\n{allowance}    scene.fill(paint);\n}}\n"


def layout(marker: bool, note: str = "") -> str:
    """A size comparison, with or without the trailing marker that excuses it."""
    trailing = ""
    if marker:
        trailing = (f" // {note}" if note else "") + " // allow-breakpoint: not a layout width"
    return f"fn rule(w: f64) {{\n    if w < 640.0 {{ hairline(); }}{trailing}\n}}\n"


class MarkerRuleTests(unittest.TestCase):
    def problems(self, before, after, declared=frozenset({"raw_paint"})):
        return checker.diff_problems("apps/poker/src/card.rs", before, after, set(declared))

    def test_a_marker_goes_only_with_the_line_it_covers(self):
        with self.subTest(phase="dropping the marker while the line stays is refused"):
            problems = self.problems(drawing(marker=True), drawing(marker=False))
            self.assertEqual(len(problems), 1)
            self.assertIn("apps/poker/src/card.rs:2:", problems[0])
            self.assertIn("`raw_paint`", problems[0])
            self.assertIn("Keep the marker, or delete or change the line", problems[0])
        with self.subTest(phase="dropping the marker with the line it covers passes"):
            self.assertEqual(self.problems(drawing(marker=True), "fn draw(_: &mut Scene) {}\n"), [])
        with self.subTest(phase="a line that now reads a token passes"):
            token = (
                "fn draw(scene: &mut Scene) {\n"
                "    scene.fill(theme.surface);\n}\n"
            )
            self.assertEqual(self.problems(drawing(marker=True), token), [])
        with self.subTest(phase="trimming the comments around the marker passes"):
            trimmed = (
                "// The card's back.\n"
                "    // ratchet-allow(raw_paint): the back of a playing card is the picture of a card\n"
                "    scene.fill(paint);\n}\n"
            )
            self.assertEqual(self.problems(drawing(marker=True), trimmed), [])
        with self.subTest(phase="a pattern no audit declares any more is a dead marker"):
            self.assertEqual(
                self.problems(drawing(marker=True), drawing(marker=False), declared=set()), []
            )
        with self.subTest(phase="trimming one key of a marker keeps the other review"):
            both = (
                "fn draw(scene: &mut Scene) {\n"
                "    // ratchet-allow(raw_paint, literal_alpha): the back of a card is content\n"
                "    scene.fill(paint);\n}\n"
            )
            one = (
                "fn draw(scene: &mut Scene) {\n"
                "    // ratchet-allow(literal_alpha): the back of a card is content\n"
                "    scene.fill(paint);\n}\n"
            )
            problems = self.problems(both, one, declared={"raw_paint", "literal_alpha"})
            self.assertEqual(len(problems), 1)
            self.assertIn("`raw_paint`", problems[0])
        with self.subTest(phase="a marker moved above its own line still allows it"):
            moved = "// The card's back.\n\n" + drawing(marker=True)
            self.assertEqual(self.problems(drawing(marker=True), moved), [])

    def test_only_the_lowest_of_stacked_markers_allows_anything(self):
        # `Ratchet::count` replaces a pending allowance with the next marker, so
        # the outer marker allows nothing and removing it changes no count.
        stacked = (
            "fn draw(scene: &mut Scene) {\n"
            "    // ratchet-allow(raw_paint): the back of a playing card is content\n"
            "    // ratchet-allow(literal_alpha): its field is washed under the lattice\n"
            "    scene.fill(paint);\n}\n"
        )
        unstacked = (
            "fn draw(scene: &mut Scene) {\n"
            "    // ratchet-allow(literal_alpha): its field is washed under the lattice\n"
            "    scene.fill(paint);\n}\n"
        )
        self.assertEqual(
            self.problems(stacked, unstacked, declared={"raw_paint", "literal_alpha"}), []
        )

    def test_an_inline_marker_goes_only_with_its_line(self):
        with self.subTest(phase="trimming the trailing marker off the line is refused"):
            problems = self.problems(layout(marker=True), layout(marker=False))
            self.assertEqual(len(problems), 1)
            self.assertIn("apps/poker/src/card.rs:2:", problems[0])
            self.assertIn("`allow-breakpoint: <why>`", problems[0])
        with self.subTest(phase="trimming every comment off the line is refused too"):
            bare = "fn rule(w: f64) {\n    if w < 640.0 { hairline(); }\n}\n"
            reasoned = layout(marker=True, note="a hairline of the edge, not a width")
            self.assertEqual(len(self.problems(reasoned, bare)), 1)
        with self.subTest(phase="dropping the marker with the comparison passes"):
            shared = "fn rule(w: f64) {\n    if nav(w) == NavMode::Narrow { hairline(); }\n}\n"
            self.assertEqual(self.problems(layout(marker=True), shared), [])
        with self.subTest(phase="reformatting the line the marker stays on passes"):
            respaced = layout(marker=True).replace("if w < 640.0", "if w<640.0")
            self.assertEqual(self.problems(layout(marker=True), respaced), [])
        with self.subTest(phase="a socket path marker follows the same rule"):
            bound = ("fn serve(path: &Path) {\n    UnixListener::bind(path) "
                     "// allow-socket-path: the check's own directory, short by construction\n}\n")
            without = "fn serve(path: &Path) {\n    UnixListener::bind(path)\n}\n"
            problems = self.problems(bound, without, declared=set())
            self.assertEqual(len(problems), 1)
            self.assertIn("`allow-socket-path: <why>`", problems[0])

    def test_a_marker_over_a_test_item_allows_nothing(self):
        # The checks skip `#[cfg(test)]` items: a test asserts what a surface
        # does at a size, so nothing there is counted and no marker is needed.
        with_allowance = (
            "fn rule(w: f64) {\n"
            "    if w < 640.0 { hairline(); }\n}\n"
            "#[cfg(test)]\nmod tests {\n"
            "    fn fits() {\n"
            "        assert!(w < 640.0); // allow-breakpoint: a fixture's width\n    }\n}\n"
        )
        without = with_allowance.replace(" // allow-breakpoint: a fixture's width", "")
        self.assertEqual(self.problems(with_allowance, without), [])

    def test_every_marker_of_the_tree_parses_as_one(self):
        # The ratchet owns the marker grammar (`support::ratchet::allow_marker`)
        # and this check mirrors it: a marker that stops parsing here would be one
        # this check silently lets go, so the suite fails as soon as one does.
        listed = subprocess.check_output(
            ["git", "-C", str(REPO), "grep", "-l", "-E",
             "ratchet-allow[(]|allow-breakpoint:|allow-socket-path:", "--", "*.rs"],
            text=True,
        ).splitlines()
        markers = 0
        inline = dict.fromkeys(checker.INLINE, 0)
        for path in listed:
            if path.startswith("vendor/"):
                continue
            text = (REPO / path).read_text(encoding="utf-8", errors="replace")
            for number, line in enumerate(text.splitlines(), 1):
                # A line written as a marker: prose about one (`//!`) is not.
                if not re.match(r"\s*//\s+ratchet-allow\(", line):
                    continue
                markers += 1
                self.assertIsNotNone(
                    checker.marker_keys(line), f"{path}:{number} is no marker this check reads"
                )
        self.assertGreater(markers, 0, "the tree holds no ratchet marker to read")
        # The inline markers are the checks' own patterns, so what matters is that
        # a line carrying one is an allowance of the code beside it -- unless it
        # sits in a `#[cfg(test)]` item, where no check counts a hit.
        for path in listed:
            if path.startswith("vendor/"):
                continue
            lines = (REPO / path).read_text(encoding="utf-8", errors="replace").splitlines()
            found = {(a.family, a.index) for a in checker.allowances(lines)}
            tests = checker._test_items(lines)
            for index, line in enumerate(lines):
                for family, pattern in checker.INLINE.items():
                    if not pattern.search(line):
                        continue
                    inline[family] += 1
                    if index in tests:
                        self.assertNotIn((family, index), found, f"{path}:{index + 1}")
                    else:
                        self.assertIn(
                            (family, index), found, f"{path}:{index + 1} is no allowance"
                        )
        for family, count in inline.items():
            self.assertGreater(count, 0, f"the tree holds no {family} marker to read")


class StagedMarkerTests(unittest.TestCase):
    """`staged` against a real index: what a commit drops."""

    def setUp(self):
        environment = mock.patch.dict(os.environ, isolated_git_environment(), clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "Test")
        audit = self.root / "crates/sdk/ui-core/tests/design_token_audit.rs"
        audit.parent.mkdir(parents=True)
        audit.write_text(AUDIT)
        card = self.root / "apps/poker/src/card.rs"
        card.parent.mkdir(parents=True)
        card.write_text(drawing(marker=True))
        self.card = card
        self.commit("draw a card's back")

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-C", str(self.root), *args], text=True, stderr=subprocess.PIPE
        ).strip()

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-qm", message)

    def stage(self, text):
        self.card.write_text(text)
        self.git("add", "apps/poker/src/card.rs")

    def test_a_commit_that_drops_a_marker_is_refused(self):
        self.stage(drawing(marker=False))
        problems = checker.staged(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("apps/poker/src/card.rs", problems[0])
        with mock.patch.object(checker, "ROOT", self.root):
            reported = io.StringIO()
            with contextlib.redirect_stderr(reported):
                self.assertEqual(checker.main(["staged"]), 1)
            self.assertIn("`raw_paint`", reported.getvalue())

    def test_a_commit_that_drops_a_marker_with_its_line_passes(self):
        self.stage("fn draw(_: &mut Scene) {}\n")
        self.assertEqual(checker.staged(self.root), [])

    def test_a_commit_that_trims_a_trailing_marker_is_refused(self):
        rule = self.root / "apps/calculator/src/chrome.rs"
        rule.parent.mkdir(parents=True)
        rule.write_text(layout(marker=True))
        self.commit("a hairline rule")
        rule.write_text(layout(marker=False))
        self.git("add", "apps/calculator/src/chrome.rs")
        problems = checker.staged(self.root)
        self.assertEqual(len(problems), 1)
        self.assertIn("apps/calculator/src/chrome.rs:2:", problems[0])
        self.assertIn("`allow-breakpoint: <why>`", problems[0])

    def test_audits_that_declare_no_pattern_fail_rather_than_judging_nothing(self):
        audit = self.root / "crates/sdk/ui-core/tests/design_token_audit.rs"
        # Sources that are there and no longer read: a parser that drifted would
        # call every marker dead and let a trimming pass through.
        audit.write_text("// an audit that declares no pattern\n")
        with self.assertRaises(ValueError):
            checker.declared_keys(self.root)
        # A tree with no ratchet at all: its markers allow nothing, so nothing
        # is lost by removing one, and a fixture repository needs no audits.
        shutil.rmtree(self.root / "crates/sdk/ui-core/tests")
        self.assertEqual(checker.declared_keys(self.root), set())
        self.stage(drawing(marker=False))
        self.assertEqual(checker.staged(self.root), [])


if __name__ == "__main__":
    unittest.main()
