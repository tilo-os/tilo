#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_repository_layout.py"
OPENSSL_RUNTIME_INVARIANT = MODULE_PATH.parents[2] / "os/tests/no_openssl_runtime.sh"
SPEC = importlib.util.spec_from_file_location("check_repository_layout", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class RepositoryLayoutTests(unittest.TestCase):
    def test_generated_directories_are_rejected_only_by_directory_components(self) -> None:
        with self.subTest(phase='runtime source policy scans existing canonical roots'):
            source = OPENSSL_RUNTIME_INVARIANT.read_text(encoding="utf-8")

            self.assertIn(
                'runtime_source_roots=("$repo/os/usr" "$repo/apps" "$repo/crates")',
                source,
            )
            self.assertIn('if [ ! -d "$root" ]', source)
            self.assertIn('"${runtime_source_roots[@]}"', source)
            self.assertNotIn('"$repo/shell"', source)
        with self.subTest(phase='source and curated shot directories are allowed'):
            self.assertEqual(
                checker.generated_artifact_violations(
                    [
                        "apps/calculator/src/lib.rs",
                        "apps/calculator/shots/default.png",
                        "dist/target-contract.sh",
                    ]
                ),
                [],
            )
        with self.subTest(phase='nested generated directories are rejected'):
            self.assertEqual(
                checker.generated_artifact_violations(
                    [
                        "tools/__pycache__/check.cpython-314.pyc",
                        "apps/calculator/target/shots/default.png",
                        "web/node_modules/package/index.js",
                        "tests/.pytest_cache/v/cache/nodeids",
                    ]
                ),
                [
                    "tracked file is inside generated `.pytest_cache` directory: tests/.pytest_cache/v/cache/nodeids",
                    "tracked file is inside generated `__pycache__` directory: tools/__pycache__/check.cpython-314.pyc",
                    "tracked file is inside generated `node_modules` directory: web/node_modules/package/index.js",
                    "tracked file is inside generated `target` directory: apps/calculator/target/shots/default.png",
                ],
            )
        with self.subTest(phase='only directory components match'):
            self.assertEqual(
                checker.generated_artifact_violations(
                    ["docs/target/README.md", "dist/target-contract.sh", "src/node_modules.rs"]
                ),
                ["tracked file is inside generated `target` directory: docs/target/README.md"],
            )
        with self.subTest(phase='repository layout combines generated and retired root checks'):
            self.assertEqual(
                checker.repository_layout_violations(
                    ["fs/src/lib.rs", "tools/__pycache__/check.cpython-314.pyc"]
                ),
                [
                    "tracked file is inside generated `__pycache__` directory: tools/__pycache__/check.cpython-314.pyc",
                    "tracked file uses retired source root `fs/`; move it under `crates/system/fs/`: fs/src/lib.rs",
                ],
            )

    def test_tool_and_retired_roots_and_duplicate_fixtures_are_rejected(self) -> None:
        with self.subTest(phase='flat ci tool paths are rejected'):
            self.assertEqual(
                checker.retired_source_root_violations(
                    [
                        "tools/cargo-shim",
                        "tools/check-build-env.sh",
                        "tools/check_changed_rust_allows.py",
                        "tools/ci-sccache.sh",
                        "tools/ci/select.py",
                        "tools/ci_select.py",
                        "tools/clippy-diff.sh",
                        "tools/resolve_ci_builder.py",
                        "tools/Run-ImagerEphemeralRunner.ps1",
                        "tools/run-imager-ephemeral-runner.sh",
                        "tools/rustc-thin-app-wrapper.sh",
                    ]
                ),
                [
                    "tracked file uses retired source root `tools/Run-ImagerEphemeralRunner.ps1`; move it under `tools/ci/runners/imager-ephemeral-windows.ps1`: tools/Run-ImagerEphemeralRunner.ps1",
                    "tracked file uses retired source root `tools/cargo-shim`; move it under `tools/ci/cargo-shim`: tools/cargo-shim",
                    "tracked file uses retired source root `tools/check-build-env.sh`; move it under `tools/ci/check-build-env.sh`: tools/check-build-env.sh",
                    "tracked file uses retired source root `tools/check_changed_rust_allows.py`; move it under `tools/ci/check_changed_rust_allows.py`: tools/check_changed_rust_allows.py",
                    "tracked file uses retired source root `tools/ci-`; move it under `tools/ci/`: tools/ci-sccache.sh",
                    "tracked file uses retired source root `tools/ci/select.py`; move it under `tools/ci/affected_packages.py`: tools/ci/select.py",
                    "tracked file uses retired source root `tools/ci_`; move it under `tools/ci/`: tools/ci_select.py",
                    "tracked file uses retired source root `tools/clippy-`; move it under `tools/ci/clippy-`: tools/clippy-diff.sh",
                    "tracked file uses retired source root `tools/resolve_ci_builder.py`; move it under `tools/ci/resolve_builder.py`: tools/resolve_ci_builder.py",
                    "tracked file uses retired source root `tools/run-imager-ephemeral-runner.sh`; move it under `tools/ci/runners/imager-ephemeral-linux.sh`: tools/run-imager-ephemeral-runner.sh",
                    "tracked file uses retired source root `tools/rustc-thin-app-wrapper.sh`; move it under `tools/ci/rustc-thin-app-wrapper.sh`: tools/rustc-thin-app-wrapper.sh",
                ],
            )
        with self.subTest(phase='application tool root exception is closed and documented'):
            allowed = sorted(checker.ROOT_APPLICATION_CONTROLLER_FILES)
            self.assertEqual(checker.flat_application_tooling_violations(allowed), [])
            self.assertEqual(
                checker.flat_application_tooling_violations(
                    allowed
                    + [
                        "tools/app_release_notes.py",
                        "tools/accept_app_preview.py",
                    ]
                ),
                [
                    "new application tooling must live under `tools/apps/`; the root-level controller exception is closed: tools/accept_app_preview.py",
                    "new application tooling must live under `tools/apps/`; the root-level controller exception is closed: tools/app_release_notes.py",
                ],
            )
        with self.subTest(phase='retired source roots are rejected with their replacements'):
            self.assertEqual(
                checker.retired_source_root_violations(
                    [
                        "apps/imager/profile-tool/src/main.rs",
                        "crates/app/tilo-app/src/lib.rs",
                        "crates/format/format-host/src/main.rs",
                        "crates/identity/tilo-user-identity/src/lib.rs",
                        "crates/packaging/hello/src/main.rs",
                        "crates/packaging/notes-cli/src/main.rs",
                        "crates/session/tilo-power/src/main.rs",
                        "crates/system/windows-vm/src/lib.rs",
                        "docs/architecture/overview.md",
                        "fs/src/lib.rs",
                        "os/usr/lib/systemd/system/tilo-power.service",
                        "plan/security.md",
                        "release/local-models/recipe.sh",
                        "scripts/demo-home.py",
                        "scripts/yc-demo.py",
                        "shell/src/main.rs",
                        "tools/check-windows-start-apps.ps1",
                        "tools/gstreamer-licenses/sources.json",
                        "tools/install-windows-apps.ps1",
                        "tools/test-windows-gstreamer.ps1",
                        "tools/windows-install-path.ps1",
                        "ui/src/lib.rs",
                    ]
                ),
                [
                    "tracked file uses retired source root `apps/imager/profile-tool/`; move it under `tools/imager-profile/`: apps/imager/profile-tool/src/main.rs",
                    "tracked file uses retired source root `crates/app/`; move it under `crates/sdk/`: crates/app/tilo-app/src/lib.rs",
                    "tracked file uses retired source root `crates/format/`; move it under `crates/doc/`: crates/format/format-host/src/main.rs",
                    "tracked file uses retired source root `crates/identity/`; move it under `crates/collab/`: crates/identity/tilo-user-identity/src/lib.rs",
                    "tracked file uses retired source root `crates/packaging/hello/`; move it under `examples/packaging/hello/`: crates/packaging/hello/src/main.rs",
                    "tracked file uses retired source root `crates/packaging/notes-cli/`; move it under `examples/packaging/notes-cli/`: crates/packaging/notes-cli/src/main.rs",
                    "tracked file uses retired source root `crates/session/tilo-power/`; move it under `crates/system/power/`: crates/session/tilo-power/src/main.rs",
                    "tracked file uses retired source root `crates/system/windows-vm/`; move it under `crates/windows/windows-vm/`: crates/system/windows-vm/src/lib.rs",
                    "tracked file uses retired source root `fs/`; move it under `crates/system/fs/`: fs/src/lib.rs",
                    "tracked file uses retired source root `os/usr/lib/systemd/system/tilo-power.service`; move it under `crates/system/power/`: os/usr/lib/systemd/system/tilo-power.service",
                    "tracked file uses retired source root `plan/`; move it under `docs/architecture/`: plan/security.md",
                    "tracked file uses retired source root `release/`; move it under `dist/`: release/local-models/recipe.sh",
                    "tracked file uses retired source root `scripts/demo-home.py`; move it under `scripts/demo/home.py`: scripts/demo-home.py",
                    "tracked file uses retired source root `scripts/yc-demo.py`; move it under `scripts/demo/run.py`: scripts/yc-demo.py",
                    "tracked file uses retired source root `shell/`; move it under `crates/session/shell/`: shell/src/main.rs",
                    "tracked file uses retired source root `tools/check-windows-`; move it under `tools/windows/check-`: tools/check-windows-start-apps.ps1",
                    "tracked file uses retired source root `tools/gstreamer-licenses/`; move it under `tools/windows/gstreamer-licenses/`: tools/gstreamer-licenses/sources.json",
                    "tracked file uses retired source root `tools/install-windows-`; move it under `tools/windows/install-`: tools/install-windows-apps.ps1",
                    "tracked file uses retired source root `tools/test-windows-`; move it under `tools/windows/test-`: tools/test-windows-gstreamer.ps1",
                    "tracked file uses retired source root `tools/windows-`; move it under `tools/windows/`: tools/windows-install-path.ps1",
                    "tracked file uses retired source root `ui/`; move it under `crates/sdk/ui-core/`: ui/src/lib.rs",
                ],
            )
        with self.subTest(phase='identical tracked fixtures must share one canonical file'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                first = root / "one/fixtures/sample.bin"
                second = root / "two/tests/fixtures/copy.bin"
                outside = root / "three/sample.bin"
                for path in (first, second, outside):
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"same fixture bytes")

                self.assertEqual(
                    checker.duplicate_fixture_violations(
                        root,
                        [
                            "one/fixtures/sample.bin",
                            "two/tests/fixtures/copy.bin",
                            "three/sample.bin",
                        ],
                    ),
                    [
                        "duplicate tracked fixture content; use one canonical file: one/fixtures/sample.bin, two/tests/fixtures/copy.bin"
                    ],
                )


if __name__ == "__main__":
    unittest.main()
