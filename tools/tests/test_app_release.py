#!/usr/bin/env python3
from pathlib import Path
import fnmatch
import re
import sys
import subprocess
import tempfile
import tomllib
import unittest
from types import SimpleNamespace
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "apps"))
import app_release  # noqa: E402


CATALOG = {name: {} for name in ("files", "notes", "settings")}
ROOT = Path(__file__).resolve().parents[2]


class AppReleaseTests(unittest.TestCase):
    def test_release_architectures_default_to_both_lanes_and_fail_closed(self):
        with self.subTest(phase='ordinary builds default to both native lanes'):
            for ref, event in [("refs/heads/main", "push"), ("refs/pull/17/merge", "pull_request")]:
                with self.subTest(event=event):
                    self.assertEqual(
                        app_release.architecture_matrix(ref, event),
                        {"include": [
                            {"arch": "aarch64", "runner": "runner-arm64", "build_runner": "runner-large-arm64"},
                            {"arch": "x86_64", "runner": "runner", "build_runner": "runner-large"},
                        ]},
                    )
        with self.subTest(phase='manual build can explicitly select one architecture'):
            self.assertEqual(
                app_release.architecture_matrix("refs/heads/main", "workflow_dispatch", requested="aarch64"),
                {"include": [{"arch": "aarch64", "runner": "runner-arm64", "build_runner": "runner-large-arm64"}]},
            )
        with self.subTest(phase='main architecture switch and tags cover both'):
            both = {
                "include": [
                    {"arch": "aarch64", "runner": "runner-arm64", "build_runner": "runner-large-arm64"},
                    {"arch": "x86_64", "runner": "runner", "build_runner": "runner-large"},
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
        with self.subTest(phase='invalid release architecture fails closed'):
            with self.assertRaisesRegex(ValueError, "architectures must contain only"):
                app_release.architecture_matrix(
                    "refs/heads/main", "push", main_architectures="riscv64"
                )

    def test_collections_and_main_push_define_the_publication_boundary(self):
        with self.subTest(phase='wildcard collection tracks catalog'):
            cfg = {"collections": {"default": {"include": ["*"]}}}
            self.assertEqual(
                app_release.resolve_collection("default", CATALOG, cfg),
                ["files", "notes", "settings"],
            )
        with self.subTest(phase='collection extension and exclusion'):
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
        with self.subTest(phase='single app component tag'):
            self.assertEqual(
                app_release.app_from_tag("app-notes-v1.2.3", CATALOG), "notes"
            )
            self.assertIsNone(app_release.app_from_tag("apps-v1.2.3", CATALOG))
        with self.subTest(phase='default collection is the complete publication boundary'):
            catalog = app_release.app_catalog(ROOT)
            self.assertEqual(
                app_release.resolve_collection(
                    "default", catalog, app_release.config(ROOT / "dist/apps/app-collections.toml")
                ),
                sorted(catalog),
            )
        with self.subTest(phase='main push selects affected apps and publishes default collection'):
            workflow = (ROOT / ".forgejo/workflows/release-apps.yml").read_text(
                encoding="utf-8"
            )
            selection = workflow.split(
                "- name: Select only the affected apps or requested collection", 1
            )[1].split(
                'python3 -I tools/ci/python.py tools/apps/app_release.py "${args[@]}"', 1
            )[0]
            push = workflow.split("  push:\n", 1)[1].split("  workflow_dispatch:\n", 1)[0]
            self.assertIn("    branches: [main]", push)
            self.assertNotIn("refs/heads/main", selection)
            # Superseded queued runs must not drop their changes: select
            # relative to the published collection, else rebuild it all.
            self.assertNotIn("github.event.before", workflow)
            self.assertIn("COLLECTION_SOURCE: ${{ vars.TILO_APP_COLLECTION_SOURCE }}", selection)
            self.assertIn(
                'if ! [[ $base =~ ^[0-9a-f]{40}$ ]] || ! use_base "$base"; then\n'
                "              args+=(--collection default)",
                selection,
            )
            self.assertIn('update_variable TILO_APP_COLLECTION_SOURCE "$GITHUB_SHA"',
                          (ROOT / "tools/apps/publish-collections.sh").read_text(encoding="utf-8"))
            self.assertIn("if [[ $GITHUB_REF == refs/heads/main ]]; then", workflow)
            self.assertIn("collection=default", workflow)

            catalog = app_release.app_catalog(ROOT)
            expected = sorted(catalog)
            for path in (
                "tools/canonical_verifier_bundle.py",
                "tools/assemble_verified_apps.py",
                ".forgejo/actions/restore-cargo-downloads/action.yml",
                "dist/apps/new-release-helper.sh",
                "tools/ci/sccache.sh",
            ):
                with self.subTest(path=path):
                    self.assertEqual(app_release.apps_from_changes([path], catalog), expected)

    def test_app_manifest_changes_rebuild_the_shared_runtime_collection(self):
        catalog = app_release.app_catalog(ROOT)
        # Mail's new Contacts dependency changed the collection runtime while
        # ordinary reverse-dependency selection chose only four apps. Reusing
        # the old tapps then failed the production runtime-root check.
        self.assertEqual(
            app_release.apps_from_changes(["apps/mail/Cargo.toml"], catalog),
            sorted(catalog),
        )
        self.assertEqual(
            app_release.apps_from_changes(["apps/mail/src/app.rs"], catalog),
            ["mail"],
        )

    def test_direct_changes_select_exactly_their_packaging_closure(self):
        with self.subTest(phase='direct app change does not select fleet'):
            self.assertEqual(
                app_release.apps_from_changes(
                    ["apps/notes/src/main.rs"], CATALOG
                ),
                ["notes"],
            )
        with self.subTest(phase='actual packaging closure inputs never silently select zero'):
            inputs = {
                "tools/ci/rustc-thin-app-wrapper.sh": "binary",
                "tools/ci/runtime-dylib-lazy-link.py": "binary",
                "os/scripts/bake-runtime-bundle.sh": "packaging",
                "os/scripts/collect-runtime-dependencies.py": "packaging",
                "os/scripts/verify-runtime-bundle-closure.sh": "verification",
                "os/scripts/verify-elf-hardening.sh": "verification",
                "os/Containerfile.builder": "binary",
                "os/external-assets/github-assets.tsv": "binary",
                "tools/apps/canonical_app_verifier.py": "verification",
                "tools/apps/create_app_receipt.py": "verification",
                "tools/apps/final_app_receipt.py": "verification",
                "tools/apps/app_channel_manifest.py": "verification",
                "tools/ci/__init__.py": "verification",
                "tools/ci/workspace_graph.py": "verification",
                "tools/ci/resolve_builder.py": "binary",
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
                ['docs/architecture.md', 'tools/ci/timing_report.py'], CATALOG))
        with self.subTest(phase='check scheduling is not an app release dependency'):
            paths = ['tools/ci/affected_packages.py', 'tools/tests/test_ci_select.py',
                     'docs/ci-performance.md']
            self.assertEqual([], app_release.apps_from_changes(paths, CATALOG))
            self.assertEqual({'binary': [], 'packaging': [], 'verification': []},
                             app_release.input_classification(paths))
            # The shared graph remains a release input, even with scheduling edits.
            self.assertEqual(sorted(CATALOG), app_release.apps_from_changes(
                paths + ['tools/ci/workspace_graph.py'], CATALOG))
        with self.subTest(phase='real git scheduling then graph edit replans release'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def git(*args):
                    return subprocess.check_output(['git', '-C', directory, *args], text=True).strip()
                git('init', '-q'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@invalid')
                for relative in ('tools/ci/affected_packages.py', 'tools/ci/workspace_graph.py'):
                    target = root / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text((ROOT / relative).read_text())
                git('add', 'tools'); git('commit', '-qm', 'shared graph baseline')
                base = git('rev-parse', 'HEAD')
                scheduler = root / 'tools/ci/affected_packages.py'
                scheduler.write_text(scheduler.read_text().replace('MAX_DEVELOPER_SHARDS = 2', 'MAX_DEVELOPER_SHARDS = 3'))
                git('commit', '-qam', 'change check fanout')
                args = SimpleNamespace(root=root, config=ROOT / 'dist/apps/app-collections.toml',
                                       tag='', apps='', base=base, collection='default')
                with mock.patch.object(app_release, 'app_catalog', return_value=app_release.app_catalog(ROOT)), \
                     mock.patch.object(app_release.ci_select, 'cargo_metadata', return_value={'packages': []}):
                    self.assertEqual(app_release.plan(args)['count'], 0)
                    graph = root / 'tools/ci/workspace_graph.py'
                    graph.write_text(graph.read_text().replace('pending = list(selected)', 'pending = sorted(selected)'))
                    git('commit', '-qam', 'change graph traversal')
                    result = app_release.plan(args)
                self.assertGreater(result['count'], 0)
                self.assertEqual(result['input_classification']['verification'], ['tools/ci/workspace_graph.py'])
        with self.subTest(phase='plan exposes receipt only invalidation without binary claim'):
            args = SimpleNamespace(root=ROOT, config=ROOT / 'dist/apps/app-collections.toml',
                                   tag='', apps='', base='baseline', collection='default')
            with mock.patch.object(app_release, 'changed_paths', return_value=['tools/apps/final_app_receipt.py']), \
                    mock.patch.object(app_release.ci_select, 'cargo_metadata', return_value={'packages': []}):
                result = app_release.plan(args)
            self.assertGreater(result['count'], 0)
            self.assertEqual('release inputs changed: verification', result['reason'])
            self.assertEqual({'binary': [], 'packaging': [], 'verification': ['tools/apps/final_app_receipt.py']},
                             result['input_classification'])
        with self.subTest(phase='real git rename selects source and destination without quoting'):
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

    def test_shared_and_global_inputs_fan_out_to_their_generic_app_dependants(self):
        with self.subTest(phase='build tool dependency and shared runtime changes select fleet'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def package(name, directory, dependencies=()):
                    return {'name': name, 'manifest_path': str(root / directory / 'Cargo.toml'),
                            'dependencies': [{'path': str(root / dep)} for dep in dependencies]}
                metadata = {'packages': [
                    package('format', 'crates/format'),
                    package('tilo-app-package-cli', 'crates/packaging/tilo-app-package-cli', ['crates/format']),
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
                # A runtime engine is signed into the runtime bundle:
                # changing it changes the runtime every app names, not only
                # the one app that loads it.
                engine = root / 'crates/runtime/engines/typst/Cargo.toml'
                engine.parent.mkdir(parents=True)
                engine.write_text('[package]\nname = "tilo-typst-engine"\n')
                metadata['packages'].append(package('tilo-typst-engine', 'crates/runtime/engines/typst'))
                self.assertEqual({'tilo-typst-engine'}, app_release.runtime_engines(root))
                self.assertEqual(sorted(catalog), app_release.apps_from_changes(
                    ['crates/runtime/engines/typst/src/lib.rs'], catalog, metadata, root))
                metadata['packages'].pop()
                # A shared dependency can invalidate packaging tools AND app code;
                # do not label that as packaging-only for a future reuse worker.
                metadata['packages'][-2]['dependencies'] = [{'path': str(root / 'crates/format')}]
                classified = app_release.input_classification(['crates/format/src/lib.rs'], metadata, root)
                self.assertEqual(['crates/format/src/lib.rs'], classified['binary'])
                self.assertEqual(['crates/format/src/lib.rs'], classified['packaging'])
        with self.subTest(phase='global change selects every app, the browser included'):
            catalog = {**CATALOG, "browser": {}}
            self.assertEqual(
                app_release.apps_from_changes(["Cargo.lock"], catalog),
                sorted(catalog),
            )
        with self.subTest(phase='shared runtime change selects reverse dependent apps'):
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
        with self.subTest(phase='real toolkit change fans out to every generic app'):
            catalog = app_release.app_catalog(ROOT)
            metadata = app_release.ci_select.cargo_metadata(ROOT)
            self.assertEqual(
                app_release.apps_from_changes(
                    ["crates/sdk/ui-core/src/lib.rs"], catalog, metadata, ROOT
                ),
                sorted(catalog),
            )
        with self.subTest(phase='every global release input triggers main packaging'):
            workflow = (ROOT / ".forgejo/workflows/release-apps.yml").read_text(
                encoding="utf-8"
            )
            push = workflow.split("  push:\n", 1)[1].split("  workflow_dispatch:\n", 1)[0]
            patterns = re.findall(r'^      - "([^"]+)"$', push, re.MULTILINE)
            inputs = set(app_release.GLOBAL_INPUTS)
            inputs.update(
                prefix + "unknown-input"
                for prefix in app_release.GLOBAL_PREFIXES + app_release.FULL_REBUILD_PREFIXES
            )
            for path in sorted(inputs):
                with self.subTest(path=path):
                    self.assertTrue(
                        any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns),
                        f"{path} does not trigger Release / apps",
                    )
        with self.subTest(phase='a browser change releases the browser tapp'):
            catalog = {**CATALOG, "browser": {}}
            self.assertEqual(
                app_release.apps_from_changes(["apps/browser/src/main.rs"], catalog), ["browser"]
            )
        with self.subTest(phase='release graph keeps image codecs explicit'):
            # One app must not enable unused image codecs in every thin tapp.
            expected = {
                ROOT / "apps/slides/Cargo.toml": ["jpeg", "png", "gif", "webp"],
                ROOT / "crates/doc/tilo-doc-pdf/Cargo.toml": ["jpeg", "png"],
            }
            for manifest, formats in expected.items():
                dependency = tomllib.loads(manifest.read_text())["dependencies"]["image"]
                self.assertFalse(dependency["default-features"], manifest)
                self.assertEqual(dependency["features"], formats, manifest)

    def test_lockfile_changes_select_proven_dependants_or_the_whole_fleet(self):
        with self.subTest(phase='lockfile change selects the fleet'):
            self.assertEqual(
                app_release.apps_from_changes(["Cargo.lock"], CATALOG),
                sorted(CATALOG),
            )
        with self.subTest(phase='proven lockfile closure selects only dependent apps'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                def package(name, directory, dependencies=()):
                    return {
                        "name": name,
                        "manifest_path": str(root / directory / "Cargo.toml"),
                        "dependencies": [
                            {"name": dependency, "path": None}
                            for dependency in dependencies
                        ],
                    }

                metadata = {
                    "packages": [
                        package("notes-bin", "apps/notes", ["markdown-parser"]),
                        package("settings-bin", "apps/settings", ["settings-format"]),
                    ]
                }
                catalog = {
                    "notes": {"package": "notes-bin"},
                    "settings": {"package": "settings-bin"},
                }
                self.assertEqual(
                    app_release.apps_from_changes(
                        ["Cargo.lock"],
                        catalog,
                        metadata,
                        root,
                        {"notes-bin"},
                    ),
                    ["notes"],
                )
        with self.subTest(phase='plan uses proven lockfile closure'):
            args = SimpleNamespace(
                root=ROOT,
                config=ROOT / "dist/apps/app-collections.toml",
                tag="",
                apps="",
                base="baseline",
                collection="default",
            )
            metadata = {
                "packages": [
                    {
                        "name": "notes",
                        "manifest_path": str(ROOT / "apps/notes/Cargo.toml"),
                        "dependencies": [],
                    }
                ]
            }
            with mock.patch.object(
                app_release, "changed_paths", return_value=["Cargo.lock"]
            ), mock.patch.object(
                app_release.ci_select, "cargo_metadata", return_value=metadata
            ), mock.patch.object(
                app_release,
                "lockfile_affected_packages",
                return_value={"notes"},
            ):
                result = app_release.plan(args)
            self.assertEqual(result["apps"], ["notes"])
            self.assertEqual(result["input_classification"]["binary"], ["Cargo.lock"])
        with self.subTest(phase='lockfile dependency graph reaches workspace consumers'):
            old = b'''version = 4
[[package]]
name = "markdown-parser"
version = "1.0.0"
source = "registry+https://example.invalid/index"
checksum = "old"
[[package]]
name = "document-engine"
version = "1.0.0"
dependencies = ["markdown-parser 1.0.0"]
'''
            new = b'''version = 4
[[package]]
name = "markdown-parser"
version = "1.1.0"
source = "registry+https://example.invalid/index"
checksum = "new"
[[package]]
name = "document-engine"
version = "1.0.0"
dependencies = ["markdown-parser 1.1.0"]
'''
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "Cargo.lock").write_bytes(new)

                def package(name, directory, dependencies=()):
                    return {
                        "name": name,
                        "manifest_path": str(root / directory / "Cargo.toml"),
                        "dependencies": [
                            {"name": dependency, "path": None}
                            for dependency in dependencies
                        ],
                    }

                metadata = {
                    "packages": [
                        package("notes-bin", "apps/notes", ["document-engine"]),
                        package("settings-bin", "apps/settings", ["settings-format"]),
                    ]
                }
                completed = subprocess.CompletedProcess(
                    args=["git", "show"], returncode=0, stdout=old
                )
                with mock.patch.object(
                    app_release.ci_select,
                    "comparison_merge_base",
                    return_value="base",
                ), mock.patch.object(app_release.subprocess, "run", return_value=completed):
                    self.assertEqual(
                        app_release.lockfile_affected_packages("base", metadata, root),
                        {"notes-bin"},
                    )
        with self.subTest(phase='unreadable prior lockfile keeps full fleet fallback'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                failed = subprocess.CompletedProcess(
                    args=["git", "show"], returncode=128, stdout=b""
                )
                with mock.patch.object(
                    app_release.ci_select,
                    "comparison_merge_base",
                    return_value="base",
                ), mock.patch.object(app_release.subprocess, "run", return_value=failed):
                    affected = app_release.lockfile_affected_packages(
                        "base", {"packages": []}, root
                    )
                self.assertIsNone(affected)
                self.assertEqual(
                    app_release.apps_from_changes(
                        ["Cargo.lock"], CATALOG, {"packages": []}, root, affected
                    ),
                    sorted(CATALOG),
                )

    def test_a_builder_change_forces_a_full_collection_and_publication_reuses_the_planned_one(self):
        source = "a" * 40
        builder = "registry.tilo.so/tilo/tilo-builder@sha256:" + "1" * 64
        other = "registry.tilo.so/tilo/tilo-builder@sha256:" + "2" * 64
        with self.subTest(phase="same builder keeps partial reuse"):
            self.assertEqual(app_release.prior_collection_base(source, builder, builder), source)
        with self.subTest(phase="a new builder changes the shared runtime: rebuild all"):
            self.assertIsNone(app_release.prior_collection_base(source, other, builder))
        with self.subTest(phase="an unrecorded builder or source rebuilds all"):
            self.assertIsNone(app_release.prior_collection_base(source, "", builder))
            self.assertIsNone(app_release.prior_collection_base(source, builder, ""))
            self.assertIsNone(app_release.prior_collection_base("", builder, builder))
            self.assertIsNone(app_release.prior_collection_base("main", builder, builder))
        with self.subTest(phase="the CLI prints the base or nothing"):
            for recorded, expected in ((builder, source), (other, "")):
                result = subprocess.run(
                    [sys.executable, str(ROOT / "tools/apps/app_release.py"),
                     "--prior-collection-base", "--collection-source", source,
                     "--collection-builder", recorded, "--builder", builder],
                    capture_output=True, text=True, check=True,
                )
                self.assertEqual(result.stdout.strip(), expected)
        with self.subTest(phase="the workflow plans and publishes against one collection"):
            workflow = (ROOT / ".forgejo/workflows/release-apps.yml").read_text(encoding="utf-8")
            selection = workflow.split(
                "- name: Select only the affected apps or requested collection", 1
            )[1].split("python3 -I tools/ci/python.py tools/apps/app_release.py \"${args[@]}\"", 1)[0]
            self.assertIn("COLLECTION_BUILDER: ${{ vars.TILO_APP_COLLECTION_BUILDER }}", selection)
            self.assertIn("BUILDER: ${{ steps.builder.outputs.image }}", selection)
            self.assertIn("--prior-collection-base", selection)
            self.assertIn('echo "prior_x86_url=${PRIOR_X86_URL:-}"', selection)
            publish = workflow.split("- name: Publish reusable tapps and assemble immutable collections", 1)[1]
            self.assertIn("PRIOR_X86_URL: ${{ needs.plan.outputs.prior_x86_url }}", publish)
            self.assertIn("PRIOR_ARM64_SHA256: ${{ needs.plan.outputs.prior_arm64_sha256 }}", publish)
            self.assertNotIn("vars.TILO_APP_COLLECTION_URL", publish.split("run: ", 1)[0])
            self.assertIn("run: tools/apps/publish-collections.sh", publish)
            script = (ROOT / "tools/apps/publish-collections.sh").read_text(encoding="utf-8")
            self.assertIn('update_variable TILO_APP_COLLECTION_BUILDER "$BUILDER_IMAGE"', script)


if __name__ == "__main__":
    unittest.main()
