#!/usr/bin/env python3
"""The runtime surface check reads Cargo's forest the way the release resolves it."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_runtime_dylib_surface.py"
SPEC = importlib.util.spec_from_file_location("check_runtime_dylib_surface", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

TARGET = "x86_64-unknown-linux-gnu"


def forest(*lines: str) -> str:
    return "\n".join(lines) + "\n"


# Two apps and the runtime, deduplicated as `cargo tree --prefix depth` prints
# it: a package's dependencies appear once, under its first appearance.
CLEAN = forest(
    "0notes v0.1.0 (/repo/apps/notes)",
    "1runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib)",
    "2tilo-app v0.1.0 (/repo/crates/sdk/tilo-app)",
    "3tilo-ui v0.1.0 (/repo/crates/sdk/ui-core)",
    "4serde v1.0.228",
    "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)",
    "0store v0.1.2 (/repo/apps/store)",
    "1app-host v0.1.0 (/repo/crates/packaging/app-host)",
    "2serde v1.0.228 (*)",
    "1runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib) (*)",
    "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)",
    "0runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib) (*)",
)


class RuntimeSurfaceTests(unittest.TestCase):
    def test_an_app_may_link_an_os_crate_the_runtime_does_not_carry(self) -> None:
        self.assertEqual(checker.violations(CLEAN, ["notes", "store"], TARGET), [])

    def test_a_denied_crate_reached_through_a_dependency_fails(self) -> None:
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n4package-store v0.1.0 (/repo)\n5tilo-fs v0.1.0 (/repo)\n",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1)
        self.assertIn("reaches tilo-fs", found[0])

    def test_an_engine_with_its_own_library_back_in_the_runtime_fails(self) -> None:
        # Typst lives in libtilo_typst_engine.so; a runtime crate depending on
        # its font table again carries it into every app process, even though
        # Typst Editor reaches the crate itself.
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n4typst-assets v0.15.1\n",
        ).replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n1typst-assets v0.15.1 (*)\n0store",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1, found)
        self.assertIn("reaches typst-assets", found[0])
        self.assertIn("libtilo_typst_engine.so", found[0])

    def test_the_workspace_hack_alone_puts_no_engine_in_the_runtime(self) -> None:
        # The hack's library names none of its dependencies, so rustc links
        # none of them: an engine crate only the hack lists is on no link line
        # of the runtime (the release's link of libruntime_dylib.so names no
        # crate only the hack reaches).
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2workspace-hack v0.1.0 (/repo)\n3typst-assets v0.15.1\n",
        ).replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n1typst-assets v0.15.1 (*)\n"
            "1workspace-hack v0.1.0 (/repo) (*)\n0store",
        )
        self.assertEqual(checker.violations(tree, ["notes", "store"], TARGET), [])

    def test_the_youtube_client_back_in_the_runtime_fails(self) -> None:
        # Music and Tube load rustypipe from libtilo_youtube_engine.so; the
        # runtime depending on it again maps it into every app process.
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2rustypipe v0.11.4\n",
        ).replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n1rustypipe v0.11.4 (*)\n0store",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1, found)
        self.assertIn("reaches rustypipe", found[0])
        self.assertIn("libtilo_youtube_engine.so", found[0])

    def test_account_custody_or_the_zone_database_back_in_the_runtime_fails(self) -> None:
        # Mail and Settings load account custody from
        # libtilo_accounts_engine.so, Calendar and Clocks the zone database
        # from libtilo_timezone_engine.so; the runtime depending on either
        # again maps it into every app process, even though apps reach it.
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2account-custody v0.1.0 (/repo/crates/system/account-custody)\n"
            "2chrono-tz v0.10.4\n",
        ).replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n"
            "1account-custody v0.1.0 (/repo/crates/system/account-custody) (*)\n"
            "1chrono-tz v0.10.4 (*)\n0store",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 2, found)
        self.assertIn("reaches account-custody", found[0])
        self.assertIn("libtilo_accounts_engine.so", found[0])
        self.assertIn("reaches chrono-tz", found[1])
        self.assertIn("libtilo_timezone_engine.so", found[1])

    def _with_runtime_crates(self, *crates: str) -> str:
        """CLEAN with `crates` under the toolkit and reached by Notes too."""
        under_ui = "".join(f"4{crate}\n" for crate in crates)
        under_notes = "".join(f"1{crate} (*)\n" for crate in crates)
        return CLEAN.replace("4serde v1.0.228\n", f"4serde v1.0.228\n{under_ui}").replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            f"1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n{under_notes}0store",
        )

    def test_sqlite_in_the_runtime_fails(self) -> None:
        # A feature an app enabled (browser-continuity/native) carried
        # rusqlite and its bundled SQLite into the runtime through the
        # toolkit; imager-backend and db-client then failed to link, because
        # rusqlite's inline API calls sqlite3_* the runtime does not export.
        tree = self._with_runtime_crates("rusqlite v0.37.0", "libsqlite3-sys v0.35.0")
        found = checker.violations(tree, ["notes", "store"], TARGET, frozenset({"libsqlite3-sys"}))
        self.assertEqual(len(found), 2, found)
        self.assertIn("reaches libsqlite3-sys", found[0])
        self.assertIn("reaches rusqlite", found[1])
        self.assertIn("link it themselves", found[1])

    def test_an_unclassified_c_library_in_the_runtime_fails(self) -> None:
        tree = self._with_runtime_crates("libfoo-sys v1.0.0", "bar v1.0.0")
        found = checker.violations(tree, ["notes", "store"], TARGET, frozenset({"bar"}))
        self.assertEqual(len(found), 2, found)
        self.assertIn("reaches bar, a C-library crate", found[0])
        self.assertIn("reaches libfoo-sys, a C-library crate", found[1])
        self.assertIn("C_LIBRARIES", found[1])

    def test_a_classified_c_library_in_the_runtime_passes(self) -> None:
        tree = self._with_runtime_crates("glib-sys v0.21.0", "zstd-sys v2.0.16+zstd.1.5.7")
        self.assertEqual(
            checker.violations(tree, ["notes", "store"], TARGET, frozenset({"zstd-sys"})), []
        )

    def test_a_crate_no_app_reaches_fails(self) -> None:
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2unused-engine v1.0.0\n",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1)
        self.assertIn("carries unused-engine", found[0])

    def test_a_graph_without_the_runtime_or_its_toolkit_fails(self) -> None:
        without_runtime = forest("0notes v0.1.0 (/repo/apps/notes)", "1serde v1.0.228")
        self.assertIn(
            "did not describe runtime-dylib",
            checker.violations(without_runtime, ["notes"], TARGET)[0],
        )
        hollow = forest("0runtime-dylib v0.1.0 (/repo)", "1serde v1.0.228")
        self.assertIn("lacks tilo-app, tilo-ui", checker.violations(hollow, [], TARGET)[0])

    def test_an_app_missing_from_the_graph_fails(self) -> None:
        found = checker.violations(CLEAN, ["notes", "store", "mail"], TARGET)
        self.assertEqual(found, [f"cargo tree did not describe mail for {TARGET}"])


if __name__ == "__main__":
    unittest.main()
