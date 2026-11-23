import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from git_fixture import isolated_git_environment

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_resource_placement", TOOLS / "policy/check_resource_placement.py"
)
placement = importlib.util.module_from_spec(spec)
sys.modules["check_resource_placement"] = placement
spec.loader.exec_module(placement)


SURFACE = '[[resources]]\nkind = "{kind}"\nlabel = "A resource"\nversion = 1\nfields = []\n'


class ResourcePlacementTests(unittest.TestCase):
    def git(self, root, *args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                       env=isolated_git_environment())

    def repository(self, surfaces):
        """A repository whose main holds `surfaces` ({app: [kind]})."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.git(root, "init", "-q", "-b", "main")
        self.git(root, "config", "user.email", "t@example.com")
        self.git(root, "config", "user.name", "t")
        self.surfaces(root, surfaces)
        self.git(root, "add", ".")
        self.git(root, "commit", "-qm", "base")
        return root

    def surfaces(self, root, surfaces):
        """`surfaces` ({app: [kind]}) as each app's `surface.toml`."""
        for app, kinds in surfaces.items():
            directory = root / "apps" / app
            (directory / "src").mkdir(parents=True, exist_ok=True)
            (directory / "surface.toml").write_text(
                "".join(SURFACE.format(kind=kind) for kind in kinds)
            )

    def entries(self, root, text):
        (root / "tools/policy").mkdir(parents=True, exist_ok=True)
        (root / "tools/policy/record_conversions.toml").write_text(text)

    def test_a_kind_moved_to_another_app_needs_an_entry(self):
        root = self.repository({"a": ["a.thing"], "b": ["b.thing"]})
        self.surfaces(root, {"a": [], "b": ["b.thing", "a.thing"]})
        found = placement.against("main", root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("'a.thing' is moved", found[0])
        self.assertIn("apps/a", found[0])
        self.entries(
            root,
            '[[change]]\nkind = "a.thing"\napp = "a"\nchange = "moved"\n'
            'conversion = "apps/a/src/move.rs:carry"\n',
        )
        (root / "apps/a/src/move.rs").write_text("pub fn carry() {}\n")
        self.assertEqual(placement.against("main", root), [])

    def test_a_tilolang_migration_carries_a_kind_its_package_took_over(self):
        root = self.repository({"a": ["a.thing"]})
        self.surfaces(root, {"a": []})
        self.entries(
            root,
            '[[change]]\nkind = "a.thing"\napp = "a"\nchange = "removed"\n'
            'conversion = "apps/tl/a/app.tl:carry_native"\n',
        )
        (root / "apps/tl/a").mkdir(parents=True)
        (root / "apps/tl/a/app.tl").write_text("action carry_native(t: Thing) {}\n")
        found = placement.against("main", root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("does not declare as a `migration action`", found[0])
        (root / "apps/tl/a/app.tl").write_text("migration action carry_native(t: Thing) {}\n")
        self.assertEqual(placement.against("main", root), [])

    def test_a_kind_gone_from_every_app_needs_an_entry(self):
        root = self.repository({"a": ["a.thing"]})
        self.surfaces(root, {"a": []})
        found = placement.against("main", root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("'a.thing' is removed", found[0])
        self.entries(
            root,
            '[[change]]\nkind = "a.thing"\napp = "a"\nchange = "removed"\n'
            'lost = "no app reads the kind any more, and its fields have no new place"\n',
        )
        self.assertEqual(placement.against("main", root), [])

    def test_one_entry_names_several_kinds_changed_alike(self):
        root = self.repository({"a": ["a.one", "a.two", "a.three"]})
        self.surfaces(root, {"a": []})
        self.entries(
            root,
            '[[change]]\nkinds = ["a.one", "a.two"]\napp = "a"\nchange = "removed"\n'
            'lost = "the document they were kept in is gone"\n',
        )
        found = placement.against("main", root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("'a.three' is removed", found[0])

    def test_an_entry_needs_a_conversion_or_a_loss(self):
        root = self.repository({"a": ["a.thing"]})
        self.entries(root, '[[change]]\nkind = "a.thing"\napp = "a"\nchange = "removed"\n')
        found = placement.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("says neither", found[0])
        self.entries(
            root,
            '[[change]]\nkind = "a.thing"\napp = "a"\nchange = "removed"\n'
            'conversion = "apps/a/src/move.rs:carry"\n',
        )
        found = placement.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("no `<path>:<fn>`", found[0])
        (root / "apps/a/src/move.rs").write_text("pub struct Carry;\n")
        found = placement.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("does not define as a function", found[0])

    def test_a_new_kind_and_a_copy_in_the_same_app_are_no_change(self):
        root = self.repository({"a": ["a.thing"]})
        self.surfaces(root, {"a": ["a.thing", "a.added"]})
        self.assertEqual(placement.against("main", root), [])


if __name__ == "__main__":
    unittest.main()
