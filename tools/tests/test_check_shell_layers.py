import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_shell_layers.py"
SPEC = importlib.util.spec_from_file_location("check_shell_layers", SCRIPT)
layers = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = layers
SPEC.loader.exec_module(layers)


WORKSPACE = """\
[workspace]
members = ["crates/session/shell", "crates/session/carved", "crates/session/middle"]

[workspace.dependencies]
toolkit = { package = "tilo-ui", version = "0.1" }
"""
SHELL_MANIFEST = """\
[package]
name = "shell"

[dependencies]
serde = "1"
workspace-hack = { version = "0.1" }

[target.'cfg(target_os = "linux")'.dependencies]
media = { package = "tilo-media", path = "../../platform/media" }

[dev-dependencies]
proptest = "1"
"""
CARVED_MANIFEST = """\
[package]
name = "carved"

[dependencies]
tracing = "0.1"
"""
MIDDLE_MANIFEST = """\
[package]
name = "middle"

[dependencies]
tracing = "0.1"
"""
BASELINE = """\
[modules]

[extracted]
crates = ["carved"]

[extracted.allow]

[shell_dependants]

[shell_dependencies]
grandfathered = ["serde", "tilo-media"]

[shell_dependencies.added]
"""


class ShellLayersTests(unittest.TestCase):
    def fixture(self, sources: dict[str, str], baseline: str = BASELINE,
                manifests: dict[str, str] | None = None) -> tuple[Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        files = {
            "Cargo.toml": WORKSPACE,
            "crates/session/shell/Cargo.toml": SHELL_MANIFEST,
            "crates/session/carved/Cargo.toml": CARVED_MANIFEST,
            "crates/session/middle/Cargo.toml": MIDDLE_MANIFEST,
            "crates/session/shell/src/lib.rs": "pub mod shell;\npub mod compositor;\n",
            "crates/session/shell/src/shell/state.rs": "pub struct WorkspaceTree;\n",
            **(manifests or {}),
            **{f"crates/session/shell/{path}": text for path, text in sources.items()},
        }
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        baseline_path = root / "baseline.toml"
        baseline_path.write_text(baseline, encoding="utf-8")
        return root, baseline_path

    def edges(self, sources: dict[str, str]) -> list[tuple[str, str]]:
        root, _ = self.fixture(sources)
        return [(edge.file, edge.target) for edge in layers.module_edges(root)]

    def test_a_clean_tree_passes(self):
        root, baseline = self.fixture(
            {"src/shell/persist.rs": "use crate::shell::state::WorkspaceTree;\n"}
        )
        self.assertEqual(layers.check(root, baseline), [])

    def test_grouped_imports_are_expanded(self):
        found = self.edges(
            {
                "src/shell/persist.rs": (
                    "use crate::{\n"
                    "    compositor::{state::State, input as keys},\n"
                    "    shell::{frame, state::WorkspaceTree},\n"
                    "};\n"
                    "use smithay::{desktop::Space, utils::Size};\n"
                ),
            }
        )
        self.assertEqual(
            found,
            [
                ("src/shell/persist.rs", "crate::compositor::input"),
                ("src/shell/persist.rs", "crate::compositor::state"),
                ("src/shell/persist.rs", "crate::shell::frame"),
                ("src/shell/persist.rs", "smithay"),
            ],
        )

    def test_relative_and_inline_paths_resolve_against_the_module(self):
        found = self.edges(
            {
                "src/shell/state/apply.rs": (
                    "use super::super::launcher;\n"
                    "fn f() { let _ = super::super::super::compositor::native::run(); }\n"
                    "mod inner {\n"
                    "    use super::super::super::overlay_host::Sheet;\n"
                    "}\n"
                ),
                "src/shell/remote_actions.rs": "fn g(r: vello::kurbo::Rect) {}\n",
            }
        )
        self.assertEqual(
            found,
            [
                ("src/shell/remote_actions.rs", "vello"),
                ("src/shell/state/apply.rs", "crate::compositor::native"),
                ("src/shell/state/apply.rs", "crate::shell::launcher"),
                ("src/shell/state/apply.rs", "crate::shell::overlay_host"),
            ],
        )

    def test_comments_strings_and_other_crates_are_not_edges(self):
        found = self.edges(
            {
                "src/shell/persist.rs": (
                    "//! See [`crate::compositor::state::State`].\n"
                    "/* crate::shell::frame */\n"
                    "const NOTE: &str = \"crate::shell::launcher\";\n"
                    "const RAW: &str = r#\"use crate::compositor::x;\"#;\n"
                    "const BRACE: char = '{';\n"
                    "fn f() { ::launcher::appdir::home(); other::smithay::x(); }\n"
                ),
            }
        )
        self.assertEqual(found, [])

    def test_test_modules_and_test_files_are_skipped(self):
        found = self.edges(
            {
                "src/shell/persist.rs": (
                    "pub fn live() {}\n"
                    "#[cfg(test)]\n"
                    "mod tests {\n"
                    "    use crate::compositor::state::State;\n"
                    "    fn brace() -> char { '}' }\n"
                    "}\n"
                    "#[cfg(all(test, target_os = \"linux\"))]\n"
                    "use crate::shell::launcher;\n"
                    "pub fn after() { crate::shell::doc_open::label(); }\n"
                ),
                "src/shell/persist/roundtrip_tests.rs": "use crate::compositor::state::State;\n",
                "src/network/invite/exchange_tests/cold.rs": "use crate::shell::launcher;\n",
            }
        )
        self.assertEqual(found, [("src/shell/persist.rs", "crate::shell::doc_open")])

    def test_only_service_modules_are_scanned(self):
        found = self.edges(
            {
                "src/shell/frame.rs": "use crate::compositor::state::State;\n",
                "src/network/surface_tracker.rs": "use crate::compositor::surface_readback::X;\n",
                "src/network/streaming.rs": "use crate::compositor::state::State;\n",
                "src/shell/collab_broker/session.rs": "use crate::shell::chrome_state::Chrome;\n",
            }
        )
        self.assertEqual(
            found,
            [
                ("src/network/streaming.rs", "crate::compositor::state"),
                ("src/shell/collab_broker/session.rs", "crate::shell::chrome_state"),
            ],
        )

    def test_a_new_edge_fails_naming_the_file_and_line(self):
        root, baseline = self.fixture(
            {"src/shell/persist.rs": "fn ok() {}\nuse crate::compositor::state::State;\n"}
        )
        problems = layers.check(root, baseline)
        self.assertEqual(len(problems), 1)
        self.assertIn("crates/session/shell/src/shell/persist.rs:2", problems[0])
        self.assertIn("crate::compositor::state", problems[0])

    def test_a_baselined_edge_passes_and_a_stale_one_fails(self):
        baseline = BASELINE.replace(
            "[modules]\n",
            "[modules]\n"
            '"src/shell/persist.rs" = ["crate::compositor::state"]\n'
            '"src/shell/state.rs" = ["crate::shell::launcher"]\n',
        )
        root, path = self.fixture(
            {
                "src/shell/persist.rs": "use crate::compositor::state::State;\n",
                "src/shell/state.rs": "pub struct WorkspaceTree;\n",
            },
            baseline,
        )
        problems = layers.check(root, path)
        self.assertEqual(len(problems), 1)
        self.assertIn("src/shell/state.rs no longer reaches crate::shell::launcher", problems[0])

    def test_write_baseline_drops_stale_entries_and_never_adds(self):
        baseline = BASELINE.replace(
            "[modules]\n", '[modules]\n"src/shell/state.rs" = ["crate::shell::launcher"]\n'
        )
        root, path = self.fixture(
            {
                "src/shell/state.rs": "pub struct WorkspaceTree;\n",
                "src/shell/persist.rs": "use crate::compositor::state::State;\n",
            },
            baseline,
        )
        written = layers.write_baseline(root, path)
        self.assertEqual(written.modules, {})
        self.assertEqual(written.extracted, ["carved"])
        reread = layers.load_baseline(path)
        self.assertEqual(reread.modules, {})
        self.assertEqual(reread.grandfathered, ["serde", "tilo-media"])
        # The new persist.rs edge still fails: writing never records it.
        self.assertEqual(len(layers.check(root, path)), 1)

    def test_an_extracted_crate_reaching_the_shell_through_a_workspace_crate_fails(self):
        root, baseline = self.fixture(
            {},
            manifests={
                "crates/session/carved/Cargo.toml": CARVED_MANIFEST
                + 'middle = { path = "../middle", optional = true }\n',
                "crates/session/middle/Cargo.toml": MIDDLE_MANIFEST
                + 'shell = { path = "../shell" }\ntoolkit = { workspace = true }\n',
            },
        )
        problems = layers.check(root, baseline)
        self.assertTrue(
            any("carved reaches shell (carved -> middle -> shell)" in p for p in problems), problems
        )
        self.assertTrue(
            any("carved reaches tilo-ui (carved -> middle -> tilo-ui)" in p for p in problems),
            problems,
        )
        self.assertTrue(any("middle depends on shell" in p for p in problems), problems)

    def test_an_extracted_crate_may_reach_an_allowed_crate_but_not_keep_a_stale_allowance(self):
        baseline = BASELINE.replace(
            "[extracted.allow]\n",
            '[extracted.allow]\ncarved = { "tilo-ui" = "draws the sheet", "wgpu" = "old" }\n',
        )
        root, path = self.fixture(
            {},
            baseline,
            manifests={
                "crates/session/carved/Cargo.toml": CARVED_MANIFEST + "toolkit.workspace = true\n"
            },
        )
        self.assertEqual(
            layers.check(root, path),
            [
                "baseline.toml: extracted crate carved no longer reaches wgpu: "
                "remove it from [extracted.allow]"
            ],
        )

    def test_dev_dependencies_on_the_shell_are_allowed(self):
        root, baseline = self.fixture(
            {},
            manifests={
                "crates/session/carved/Cargo.toml": CARVED_MANIFEST
                + '\n[dev-dependencies]\nshell = { path = "../shell" }\n'
            },
        )
        self.assertEqual(layers.check(root, baseline), [])

    def test_a_new_shell_dependency_needs_a_reasoned_entry(self):
        manifest = SHELL_MANIFEST.replace('serde = "1"\n', 'serde = "1"\nwgpu = "29"\n')
        root, baseline = self.fixture(
            {}, manifests={"crates/session/shell/Cargo.toml": manifest}
        )
        problems = layers.check(root, baseline)
        self.assertEqual(len(problems), 1)
        self.assertIn("shell gains the dependency wgpu", problems[0])

        reasoned = BASELINE + 'wgpu = "presents the output scene"\n'
        root, baseline = self.fixture(
            {}, reasoned, manifests={"crates/session/shell/Cargo.toml": manifest}
        )
        self.assertEqual(layers.check(root, baseline), [])

    def test_a_removed_shell_dependency_is_a_stale_entry(self):
        manifest = SHELL_MANIFEST.replace('serde = "1"\n', "")
        root, baseline = self.fixture(
            {}, manifests={"crates/session/shell/Cargo.toml": manifest}
        )
        self.assertEqual(
            layers.check(root, baseline),
            ["baseline.toml: shell no longer depends on serde: remove it from [shell_dependencies]"],
        )

    def test_entries_without_a_reason_are_rejected(self):
        root, baseline = self.fixture({}, BASELINE + 'wgpu = " "\n')
        with self.assertRaises(layers.BaselineError):
            layers.check(root, baseline)

    def test_a_missing_shell_package_fails_instead_of_passing(self):
        root, baseline = self.fixture(
            {}, manifests={"crates/session/shell/Cargo.toml": '[package]\nname = "other"\n'}
        )
        problems = layers.check(root, baseline)
        self.assertTrue(any("cannot see it" in p for p in problems), problems)

    def test_no_service_modules_fails_instead_of_passing(self):
        root, baseline = self.fixture({})
        (root / layers.SHELL / "src/shell/state.rs").unlink()
        problems = layers.check(root, baseline)
        self.assertTrue(any("cannot see them" in p for p in problems), problems)


if __name__ == "__main__":
    unittest.main()
