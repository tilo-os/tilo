"""Every Rust source of a crate is reached by one of its targets.

Cargo compiles the target roots and what their ``mod`` declarations,
``#[path]`` attributes and ``include!`` macros reach; anything else in the
crate is never built or tested, yet reads as code. Each test below builds a
crate and proves the check follows one of those ways, and names the file that
none of them reaches.
"""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

POLICY = Path(__file__).resolve().parents[1] / "policy"


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, POLICY / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


check = load("check_module_reachability")

MANIFEST = '[package]\nname = "crate"\nversion = "0.0.0"\nedition = "2024"\n'


class Tree:
    """A workspace holding one crate, at ``crates/crate``, committed to git."""

    def __init__(self, files: dict[str, str]):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.git("init", "-q", "-b", "main")
        self.write(
            {
                "Cargo.toml": '[workspace]\nmembers = ["crates/crate"]\n',
                "crates/crate/Cargo.toml": MANIFEST,
                **files,
            }
        )
        self.stage(".")

    def git(self, *args: str) -> None:
        subprocess.run(
            [
                "git", "-C", str(self.root),
                "-c", "user.name=Policy Test", "-c", "user.email=policy@example.org",
                *args,
            ],
            check=True, capture_output=True, text=True,
        )

    def write(self, files: dict[str, str]) -> None:
        for name, text in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def stage(self, *names: str) -> None:
        self.git("add", "--", *names)
        self.git("commit", "-qm", "the tree")

    def stage_only(self, *names: str) -> None:
        self.git("add", "--", *names)

    def orphans(self, staged: bool = False) -> list[str]:
        return [entry.split(":", 1)[0] for entry in check.orphans(self.root, staged)]


class ReachabilityTest(unittest.TestCase):
    def tree(self, files: dict[str, str]) -> "Tree":
        tree = Tree(files)
        self.addCleanup(tree.temporary.cleanup)
        return tree


class ModuleTests(ReachabilityTest):
    def test_a_module_declared_from_the_crate_root_is_reached(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "mod kept;\n",
                "crates/crate/src/kept.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_source_no_declaration_reaches_is_reported(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "mod kept;\n",
                "crates/crate/src/kept.rs": "",
                "crates/crate/src/stray.rs": "pub fn twice() {}\n",
            }
        )
        self.assertEqual(["crates/crate/src/stray.rs"], tree.orphans())

    def test_a_module_file_owns_a_directory_of_its_own(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "mod view;\n",
                "crates/crate/src/view.rs": "mod rows;\n",
                "crates/crate/src/view/rows.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_module_inside_an_inline_module_keeps_its_component(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "mod view { mod rows { mod leaf; } }\n",
                "crates/crate/src/view/rows/leaf.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_path_attribute_names_the_file(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": '#[path = "platform/linux.rs"]\npub(crate) mod platform;\n',
                "crates/crate/src/platform/linux.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_platform_attribute_reaches_both_files(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": (
                    'mod surface;\n'
                    'mod other;\n'
                ),
                "crates/crate/src/surface.rs": (
                    '#[cfg_attr(not(feature = "native"), path = "surface/absent.rs")]\n'
                    "mod host;\n"
                ),
                "crates/crate/src/surface/host.rs": "",
                "crates/crate/src/surface/absent.rs": "",
                "crates/crate/src/other.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_an_include_of_a_manifest_path_reaches_the_file(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": (
                    'mod support {\n'
                    '    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/media.rs"));\n'
                    "}\n"
                ),
                "crates/crate/tests/support/media.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_macro_that_declares_modules_by_a_listed_path_reaches_them(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "mod surface;\n",
                "crates/crate/src/surface.rs": 'scenarios!(gallery, "gallery.rs", "a gallery");\n',
                "crates/crate/src/gallery.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_declaration_named_in_a_comment_reaches_nothing(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "// mod stray;\n/* mod stray; */\n",
                "crates/crate/src/stray.rs": "",
            }
        )
        self.assertEqual(["crates/crate/src/stray.rs"], tree.orphans())


class TargetTests(ReachabilityTest):
    def test_a_main_binary_is_a_root(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "",
                "crates/crate/src/main.rs": "mod runner;\n",
                "crates/crate/src/runner.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_a_build_script_is_a_root(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "",
                "crates/crate/build.rs": "mod generate;\n",
                "crates/crate/generate.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())

    def test_an_undeclared_example_is_reported_when_auto_discovery_is_off(self):
        tree = self.tree(
            {
                "crates/crate/Cargo.toml": MANIFEST + (
                    'autoexamples = false\n\n'
                    '[[example]]\nname = "kept"\npath = "examples/kept.rs"\n'
                ),
                "crates/crate/src/lib.rs": "",
                "crates/crate/examples/kept.rs": "",
                "crates/crate/examples/stray.rs": "",
            }
        )
        self.assertEqual(["crates/crate/examples/stray.rs"], tree.orphans())

    def test_a_test_target_compiles_the_modules_it_declares(self):
        tree = self.tree(
            {
                "crates/crate/Cargo.toml": MANIFEST + (
                    'autotests = false\n\n'
                    '[[test]]\nname = "it"\npath = "tests/it.rs"\n'
                ),
                "crates/crate/src/lib.rs": "",
                "crates/crate/tests/it.rs": "mod support;\n",
                "crates/crate/tests/support.rs": "",
                "crates/crate/tests/stray.rs": "",
            }
        )
        self.assertEqual(["crates/crate/tests/stray.rs"], tree.orphans())

    def test_modules_of_it_rs_require_autotests_off(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "",
                "crates/crate/tests/it.rs": "mod kept;\n",
                "crates/crate/tests/kept.rs": "",
            }
        )
        self.assertEqual(["crates/crate/Cargo.toml"], tree.orphans())

    def test_a_fixture_tree_and_a_template_are_not_the_crates_sources(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "",
                "crates/crate/tests/fixtures/worker/src/main.rs": "",
                "crates/crate/templates/starter.rs": "",
            }
        )
        self.assertEqual([], tree.orphans())


class StagedTests(ReachabilityTest):
    def test_a_commit_judges_the_crates_it_touches(self):
        tree = self.tree(
            {
                "crates/crate/src/lib.rs": "",
                "crates/crate/src/stray.rs": "",
            }
        )
        tree.write({"other.txt": "a change\n"})
        tree.stage_only("other.txt")
        self.assertEqual([], tree.orphans(staged=True))
        tree.write({"crates/crate/src/lib.rs": "// touched\n"})
        tree.stage_only("crates/crate/src/lib.rs")
        self.assertEqual(["crates/crate/src/stray.rs"], tree.orphans(staged=True))


if __name__ == "__main__":
    unittest.main()
