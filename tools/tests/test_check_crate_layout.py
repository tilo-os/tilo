#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).parents[1] / "policy/check_crate_layout.py"
SPEC = importlib.util.spec_from_file_location("check_crate_layout", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


def crate(root: Path, path: str, name: str, deps: str = "") -> None:
    (root / path).mkdir(parents=True)
    (root / path / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n{deps}')


class CrateLayoutTests(unittest.TestCase):
    def repository(self, exceptions: str) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/*/*", "tools/*"]\n\n'
            '[workspace.dependencies]\ntilo-paths = { path = "crates/platform/tilo-paths" }\n'
        )
        crate(root, "crates/platform/tilo-paths", "tilo-paths")
        crate(root, "tools/test-env", "tilo-test-env")
        crate(root, "crates/ui/ui-paint", "tilo-ui-paint", "[dependencies]\ntilo-paths.workspace = true\n")
        crate(root, "crates/desktop/compositor", "tilo-compositor")
        crate(
            root,
            "crates/core/tilo-runtime",
            "tilo-runtime",
            "[dependencies]\ntilo-paths.workspace = true\n"
            'tilo-ui-paint = { path = "../../ui/ui-paint" }\n'
            "[target.'cfg(target_os = \"linux\")'.dependencies]\n"
            'tilo-compositor = { path = "../../desktop/compositor" }\n'
            "[dev-dependencies]\n"
            'tilo-test-env = { path = "../../../tools/test-env" }\n',
        )
        (root / "tools/policy").mkdir(parents=True)
        (root / checker.EXCEPTIONS).write_text(exceptions)
        return root

    def test_a_core_crate_links_only_core_and_platform(self) -> None:
        root = self.repository('core = ["tilo-runtime -> tilo-ui-paint"]\n')
        problems = checker.check(root)
        # The UI library is listed, the target-specific desktop crate is not, the
        # platform crate is allowed and the test environment is a dev-dependency.
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("tilo-runtime -> tilo-compositor: a crates/core/ crate"))

    def test_a_service_links_core_but_the_core_never_links_a_service(self) -> None:
        root = self.repository('core = ["tilo-runtime -> tilo-ui-paint", "tilo-runtime -> tilo-compositor"]\n')
        crate(
            root,
            "crates/services/speaker-service",
            "speaker-service",
            "[dependencies]\ntilo-paths.workspace = true\n"
            'tilo-runtime = { path = "../../core/tilo-runtime" }\n'
            'tilo-compositor = { path = "../../desktop/compositor" }\n',
        )
        crate(
            root,
            "crates/core/tilo-daemon-core",
            "tilo-daemon-core",
            '[dependencies]\nspeaker-service = { path = "../../services/speaker-service" }\n',
        )
        problems = sorted(checker.check(root))
        self.assertEqual(len(problems), 2, problems)
        self.assertTrue(problems[0].startswith("speaker-service -> tilo-compositor: a crates/services/ crate"))
        self.assertTrue(problems[1].startswith("tilo-daemon-core -> speaker-service: a crates/core/ crate"))

    def test_the_list_only_shrinks(self) -> None:
        root = self.repository(
            'core = ["tilo-runtime -> tilo-ui-paint", "tilo-runtime -> tilo-compositor",\n'
            '        "tilo-runtime -> tilo-search"]\n'
        )
        self.assertEqual(
            checker.check(root),
            [f"{checker.EXCEPTIONS}: `tilo-runtime -> tilo-search` under [core] no longer exists: delete the line"],
        )

    def test_a_crate_in_a_group_the_layout_does_not_name_fails(self) -> None:
        root = self.repository('core = ["tilo-runtime -> tilo-ui-paint", "tilo-runtime -> tilo-compositor"]\n')
        crate(root, "crates/core/actions", "tilo-actions")
        problems = checker.check(root)
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("crates/core/actions: crates/collab/ is not a group"))

    def test_the_repository_keeps_its_layout(self) -> None:
        self.assertEqual(checker.check(), [])


if __name__ == "__main__":
    unittest.main()
