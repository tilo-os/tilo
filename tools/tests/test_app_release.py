#!/usr/bin/env python3
from pathlib import Path
import sys
import subprocess
import tempfile
import tomllib
import unittest
from types import SimpleNamespace
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_release  # noqa: E402


CATALOG = {name: {} for name in ("files", "notes", "settings")}
ROOT = Path(__file__).resolve().parents[2]


class AppReleaseTests(unittest.TestCase):
    def test_ordinary_builds_default_to_both_native_lanes(self) -> None:
        for ref, event in [("refs/heads/main", "push"), ("refs/pull/17/merge", "pull_request")]:
            with self.subTest(event=event):
                self.assertEqual(
                    app_release.architecture_matrix(ref, event),
                    {"include": [
                        {"arch": "aarch64", "runner": "runner-arm64"},
                        {"arch": "x86_64", "runner": "runner"},
                    ]},
                )

    def test_manual_build_can_explicitly_select_one_architecture(self) -> None:
        self.assertEqual(
            app_release.architecture_matrix("refs/heads/main", "workflow_dispatch", requested="aarch64"),
            {"include": [{"arch": "aarch64", "runner": "runner-arm64"}]},
        )

    def test_main_architecture_switch_and_tags_cover_both(self) -> None:
        both = {
            "include": [
                {"arch": "aarch64", "runner": "runner-arm64"},
                {"arch": "x86_64", "runner": "runner"},
            ]
        }
        self.assertEqual(
            app_release.architecture_matrix(
                "refs/heads/main", "push", main_architectures="x86_64,aarch64"
            ),
            both,
        )
        self.assertEqual(
            app_release.architecture_matrix(
                "refs/tags/apps-v1.0.0", "push", requested="x86_64"
            ),
            both,
        )

    def test_invalid_release_architecture_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "architectures must contain only"):
            app_release.architecture_matrix(
                "refs/heads/main", "push", main_architectures="riscv64"
            )

    def test_wildcard_collection_tracks_catalog(self) -> None:
        cfg = {"collections": {"default": {"include": ["*"]}}}
        self.assertEqual(
            app_release.resolve_collection("default", CATALOG, cfg),
            ["files", "notes", "settings"],
        )

    def test_collection_extension_and_exclusion(self) -> None:
        cfg = {
            "collections": {
                "default": {"include": ["*"]},
                "small": {"extends": ["default"], "exclude": ["settings"]},
            }
        }
        self.assertEqual(
            app_release.resolve_collection("small", CATALOG, cfg),
            ["files", "notes"],
        )

    def test_single_app_component_tag(self) -> None:
        self.assertEqual(
            app_release.app_from_tag("app-notes-v1.2.3", CATALOG), "notes"
        )
        self.assertIsNone(app_release.app_from_tag("apps-v1.2.3", CATALOG))

    def test_direct_app_change_does_not_select_fleet(self) -> None:
        self.assertEqual(
            app_release.apps_from_changes(
                ["apps/notes/src/main.rs"], CATALOG
            ),
            ["notes"],
        )

    def test_actual_packaging_closure_inputs_never_silently_select_zero(self) -> None:
        inputs = {
            "tools/rustc-thin-app-wrapper.sh": "binary",
            "os/scripts/bake-runtime-bundle.sh": "packaging",
            "os/scripts/verify-elf-hardening.sh": "verification",
            "os/Containerfile.builder": "binary",
            "os/external-assets/github-assets.tsv": "binary",
            "tools/canonical_app_verifier.py": "verification",
            "tools/create_app_receipt.py": "verification",
            "tools/final_app_receipt.py": "verification",
            "tools/app_channel_manifest.py": "verification",
            "tools/resolve_ci_builder.py": "binary",
        }
        for path, kind in inputs.items():
            with self.subTest(path=path):
                self.assertTrue((ROOT / path).is_file())
                self.assertEqual(sorted(CATALOG), app_release.apps_from_changes([path], CATALOG))
                classified = app_release.input_classification([path])
                self.assertEqual([path], classified[kind])
                self.assertEqual(1, sum(len(values) for values in classified.values()))
        # Non-input prose and an unrelated CI helper must not become fleet rebuilds.
        self.assertEqual([], app_release.apps_from_changes(
            ['docs/architecture.md', 'tools/ci_queue_coalesce.py'], CATALOG))

    def test_plan_exposes_receipt_only_invalidation_without_binary_claim(self) -> None:
        args = SimpleNamespace(root=ROOT, config=ROOT / 'dist/apps/app-collections.toml',
                               tag='', apps='', base='baseline', collection='default')
        with mock.patch.object(app_release, 'changed_paths', return_value=['tools/final_app_receipt.py']), \
                mock.patch.object(app_release.ci_select, 'cargo_metadata', return_value={'packages': []}):
            result = app_release.plan(args)
        self.assertGreater(result['count'], 0)
        self.assertEqual('release inputs changed: verification', result['reason'])
        self.assertEqual({'binary': [], 'packaging': [], 'verification': ['tools/final_app_receipt.py']},
                         result['input_classification'])

    def test_build_tool_dependency_and_shared_runtime_changes_select_fleet(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def package(name, directory, dependencies=()):
                return {'name': name, 'manifest_path': str(root / directory / 'Cargo.toml'),
                        'dependencies': [{'path': str(root / dep)} for dep in dependencies]}
            metadata = {'packages': [
                package('format', 'crates/format'),
                package('pkg-cli', 'crates/packaging/pkg-cli', ['crates/format']),
                package('runtime-dylib', 'crates/runtime/runtime-dylib'),
                package('notes-bin', 'apps/notes'), package('settings-bin', 'apps/settings'),
            ]}
            catalog = {'notes': {'package': 'notes-bin'}, 'settings': {'package': 'settings-bin'}}
            for path, kind in [('crates/format/src/lib.rs', 'packaging'),
                               ('crates/runtime/runtime-dylib/src/lib.rs', 'binary')]:
                self.assertEqual(sorted(catalog), app_release.apps_from_changes([path], catalog, metadata, root))
                self.assertEqual([path], app_release.input_classification([path], metadata, root)[kind])
            self.assertEqual(sorted(catalog), app_release.apps_from_changes(
                ['crates/deleted-owner/src/lib.rs'], catalog, metadata, root))
            # A shared dependency can invalidate packaging tools AND app code;
            # do not label that as packaging-only for a future reuse worker.
            metadata['packages'][-2]['dependencies'] = [{'path': str(root / 'crates/format')}]
            classified = app_release.input_classification(['crates/format/src/lib.rs'], metadata, root)
            self.assertEqual(['crates/format/src/lib.rs'], classified['binary'])
            self.assertEqual(['crates/format/src/lib.rs'], classified['packaging'])

    def test_real_git_rename_selects_source_and_destination_without_quoting(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], text=True).strip()
            git('init', '-q'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@invalid')
            old = root / 'apps/notes/naïve.rs'
            old.parent.mkdir(parents=True)
            old.write_text('unchanged source\n')
            git('add', 'apps'); git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            new = root / 'apps/settings/naïve.rs'
            new.parent.mkdir(parents=True)
            old.rename(new)
            git('add', 'apps'); git('commit', '-qm', 'rename')
            paths = app_release.changed_paths(base, root)
            self.assertEqual({'apps/notes/naïve.rs', 'apps/settings/naïve.rs'}, set(paths))
            self.assertEqual(['notes', 'settings'], app_release.apps_from_changes(paths, CATALOG))

    def test_lockfile_change_selects_the_fleet(self) -> None:
        self.assertEqual(
            app_release.apps_from_changes(["Cargo.lock"], CATALOG),
            sorted(CATALOG),
        )

    def test_global_change_excludes_os_integrated_apps(self) -> None:
        catalog = {**CATALOG, "browser": {}}
        self.assertEqual(
            app_release.apps_from_changes(["Cargo.lock"], catalog),
            sorted(CATALOG),
        )

    def test_shared_runtime_change_selects_reverse_dependent_apps(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = {
                "packages": [
                    {
                        "name": "ui",
                        "manifest_path": str(root / "crates/sdk/ui-core/Cargo.toml"),
                        "dependencies": [],
                    },
                    {
                        "name": "notes-bin",
                        "manifest_path": str(root / "apps/notes/Cargo.toml"),
                        "dependencies": [{"path": str(root / "crates/sdk/ui-core")}],
                    },
                    {
                        "name": "settings-bin",
                        "manifest_path": str(root / "apps/settings/Cargo.toml"),
                        "dependencies": [],
                    },
                ]
            }
            catalog = {
                "notes": {"package": "notes-bin"},
                "settings": {"package": "settings-bin"},
            }
            self.assertEqual(
                app_release.apps_from_changes(
                    ["crates/sdk/ui-core/src/lib.rs"], catalog, metadata, root
                ),
                ["notes"],
            )

    def test_default_collection_is_the_complete_publication_boundary(self) -> None:
        catalog = app_release.app_catalog(ROOT)
        self.assertEqual(
            app_release.resolve_collection(
                "default", catalog, app_release.config(ROOT / "dist/apps/app-collections.toml")
            ),
            sorted(set(catalog).difference(app_release.OS_INTEGRATED_APPS)),
        )

    def test_os_integrated_browser_change_does_not_use_generic_builder(self) -> None:
        catalog = {**CATALOG, "browser": {}}
        self.assertEqual(
            app_release.apps_from_changes(["apps/browser/src/main.rs"], catalog), []
        )

    def test_release_graph_keeps_image_codecs_explicit(self) -> None:
        """One app must not enable unused image codecs in every thin qpkg."""
        expected = {
            ROOT / "apps/slides/Cargo.toml": ["jpeg", "png", "gif", "webp"],
            ROOT / "crates/doc/qdoc-pdf/Cargo.toml": ["jpeg", "png"],
        }
        for manifest, formats in expected.items():
            dependency = tomllib.loads(manifest.read_text())["dependencies"]["image"]
            self.assertFalse(dependency["default-features"], manifest)
            self.assertEqual(dependency["features"], formats, manifest)


if __name__ == "__main__":
    unittest.main()
