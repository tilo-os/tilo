#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest


MODULE_PATH = Path(__file__).parents[1] / "ci/affected_packages.py"
REPO_ROOT = MODULE_PATH.parents[2]
SPEC = importlib.util.spec_from_file_location("ci_select", MODULE_PATH)
assert SPEC and SPEC.loader
ci_select = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_select)


class NamedPackageTests(unittest.TestCase):
    def test_named_packages_exist_in_the_workspace(self):
        # The selector intersects these sets with the workspace, so a renamed
        # crate would otherwise silently drop out of the contract or heavy set.
        manifests = subprocess.run(["git", "ls-files", "*Cargo.toml"], cwd=REPO_ROOT, text=True,
                                   capture_output=True, check=True).stdout.split()
        names = {tomllib.loads((REPO_ROOT / path).read_text()).get("package", {}).get("name")
                 for path in manifests}
        self.assertFalse(ci_select.ALWAYS_PACKAGES - names)
        self.assertFalse(ci_select.HEAVY_PACKAGES - names)


class SelectorTests(unittest.TestCase):
    def setUp(self) -> None:
        # Merged tests call setUp() again at the start of every phase so each
        # phase gets a fresh temporary workspace and freshly built metadata.
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        def package(name: str, rel: str, deps: list[str] | None = None) -> dict:
            directory = self.root / rel
            return {
                "name": name,
                "manifest_path": str(directory / "Cargo.toml"),
                "targets": [{"name": name}],
                "dependencies": [
                    {"name": dep, "path": str(self.root / dep)} for dep in (deps or [])
                ],
            }
        self.metadata = {"packages": [
            package("base", "base"),
            package("middle", "middle", ["base"]),
            package("leaf", "leaf", ["middle"]),
            package("tilo-doc", "tilo-doc"),
            package("tilo-doc-codecs", "tilo-doc-codecs", ["tilo-doc"]),
            package("tilo-app-package", "tilo-app-package"),
            package("collab-types", "collab-types"),
        ]}

    def test_gateway_feature_recipe_selects_server_tests_without_client_reverse_closure(self):
        gateway = 'tilo-model-gateway'
        self.metadata['packages'].extend([
            {'name': gateway, 'manifest_path': str(self.root / gateway / 'Cargo.toml'),
             'targets': [{'name': gateway}], 'dependencies': []},
            {'name': 'client-ui', 'manifest_path': str(self.root / 'client-ui/Cargo.toml'),
             'targets': [{'name': 'client-ui'}],
             'dependencies': [{'name': gateway, 'path': str(self.root / gateway)}]},
        ])
        plan = ci_select.make_plan(self.metadata, self.root, ['tools/ci/package_features.py'], 2)
        self.assertFalse(plan['full'])
        self.assertIn(gateway, plan['tested'])
        self.assertNotIn('client-ui', plan['selected'])
        self.assertTrue(any('-p ' + gateway in row['test_packages']
                            for row in plan['matrix']['include']))

    def test_browser_only_library_is_checked_for_native_and_wasm_without_an_empty_test_run(self):
        directory = self.root / "web"
        (directory / "src").mkdir(parents=True)
        source = directory / "src/lib.rs"
        source.write_text('//! Browser exports.\n#![cfg(target_arch = "wasm32")]\n')
        package = {
            "name": "tilo-web-desktop", "manifest_path": str(directory / "Cargo.toml"),
            "targets": [{"name": "tilo-web-desktop", "kind": ["lib"], "test": True,
                         "src_path": str(source)}],
            "dependencies": [{"name": "base", "path": str(self.root / "base")},
                             {"name": "wasm-bindgen", "kind": None}],
        }
        self.metadata["packages"].append(package)
        plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
        self.assertIn("tilo-web-desktop", plan["selected"])
        self.assertIn("tilo-web-desktop", plan["wasm"])
        self.assertNotIn("tilo-web-desktop", plan["tested"])
        self.assertTrue(any("-p tilo-web-desktop" in row["packages"]
                            for row in plan["clippy_matrix"]["include"]))
        self.assertFalse(any("-p tilo-web-desktop" in row["test_packages"]
                             for row in plan["matrix"]["include"]))
        # An actual native integration target must restore test execution,
        # even while the library remains gated to wasm.
        native = directory / "tests/native.rs"
        native.parent.mkdir()
        native.write_text('#[test] fn native_subject() {}\n')
        package["targets"].append({"name": "native", "kind": ["test"], "test": True,
                                   "src_path": str(native)})
        plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
        self.assertIn("tilo-web-desktop", plan["tested"])
        self.assertTrue(any("-p tilo-web-desktop" in row["test_packages"]
                            for row in plan["matrix"]["include"]))

    def test_the_desktop_gate_runs_whenever_a_change_reaches_the_desktop(self):
        gate = ci_select.path_suites.ReaderScope((), "it", "binary(it) and test(desktop_smoke::)")
        saved = ci_select.DESKTOP_GATE
        self.addCleanup(setattr, ci_select, "DESKTOP_GATE", saved)
        # "leaf" stands in for the shell: the end of the dependency chain.
        ci_select.DESKTOP_GATE = {"leaf": gate}
        with self.subTest(phase="a foundation change reaching it without testing it still runs the smoke"):
            plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
            self.assertIn("leaf", plan["selected"])
            self.assertNotIn("leaf", plan["tested"])
            self.assertEqual(plan["scoped_tests"], {"leaf": "--test it -E 'binary(it) and test(desktop_smoke::)'"})
            rows = [json.loads(row["scoped_tests"]) for row in plan["matrix"]["include"] if row["scoped_tests"]]
            self.assertEqual(rows, [[{"package": "leaf", "test": "it", "filter": gate.filter}]])
        with self.subTest(phase="a change to it tests all of it, and the smoke is part of that"):
            plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/main.rs"], 2)
            self.assertIn("leaf", plan["tested"])
            self.assertEqual(plan["scoped_tests"], {})
        with self.subTest(phase="a change that cannot reach it does not run it"):
            plan = ci_select.make_plan(self.metadata, self.root, ["qdoc/src/lib.rs"], 2)
            self.assertNotIn("leaf", plan["selected"])
            self.assertEqual(plan["scoped_tests"], {})
        with self.subTest(phase="a reading scope on the same target is joined, not replaced"):
            manifest = ci_select.path_suites.parse(
                '[[cargo]]\npackage = "leaf"\ninputs = ["os/x.json"]\nreaders = ["leaf/tests/audit.rs"]\n'
                'scope = { inputs = ["os/x.json"], test = "it", filter = "test(/^audit::/)" }\n')
            plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs", "os/x.json"], 2,
                                       suite_manifest=manifest)
            self.assertEqual(plan["scoped_tests"], {
                "leaf": "--test it -E '(test(/^audit::/)) or (binary(it) and test(desktop_smoke::))'"})

    def test_a_recheck_keeps_only_what_the_base_changes_reach(self):
        with self.subTest(phase="the plan narrows to packages the base's own changes reach"):
            self.setUp()
            full = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
            delta = ci_select.make_plan(self.metadata, self.root, ["middle/src/lib.rs"], 2)
            self.assertEqual(delta["reached"], ["leaf", "middle"])
            narrowed = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2,
                                           only={"packages": set(delta["reached"]), "suites": set()})
            self.assertEqual(narrowed["selected"], ["leaf", "middle"])
            self.assertLess(set(narrowed["tested"]), set(full["tested"]))
            self.assertTrue(set(narrowed["tested"]) <= {"leaf", "middle"})
            self.assertIn("re-check", narrowed["reason"])
        with self.subTest(phase="a base change reaching nothing the pull request checks leaves nothing to run"):
            self.setUp()
            delta = ci_select.make_plan(self.metadata, self.root, ["docs/notes.md"], 2)
            self.assertEqual(delta["reached"], [])
            narrowed = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2,
                                           only={"packages": set(delta["reached"]), "suites": set()})
            self.assertEqual(narrowed["selected"], [])
            self.assertFalse(narrowed["rust_required"])
        with self.subTest(phase="a repository-wide base change reaches everything"):
            self.setUp()
            delta = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
            self.assertTrue(delta["full"])
            self.assertEqual(delta["reached"], delta["selected"])

    def test_path_suites_and_declared_readers_select_what_their_inputs_reach(self):
        with self.subTest(phase='hermetic suites follow changed paths and run all when asked'):
            self.setUp()
            manifest = ci_select.path_suites.parse(
                '[[group]]\nname = "os"\ninputs = ["os/"]\nsuites = ["os/tests/a.sh"]\n'
                '[[group]]\nname = "perf"\ninputs = ["scripts/perf/"]\nsuites = ["scripts/perf/b_test.py"]\n')
            def plan(changes, **options):
                return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                           suite_manifest=manifest, **options)
            selected = plan(["os/dev/tool.py"])
            self.assertTrue(selected["suites_required"])
            self.assertEqual(selected["suites"], ["os/tests/a.sh"])
            unrelated = plan(["leaf/src/lib.rs"])
            self.assertFalse(unrelated["suites_required"])
            self.assertEqual(unrelated["suites"], [])
            for options in ({"all_suites": True}, {"force_full": True}):
                with self.subTest(options=options):
                    self.assertEqual(plan(["leaf/src/lib.rs"], **options)["suites"],
                                     ["os/tests/a.sh", "scripts/perf/b_test.py"])
            with tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "outputs"
                ci_select.emit_github(selected, output)
                lines = output.read_text().splitlines()
            self.assertIn("suites_required=true", lines)
            self.assertIn("suites=os/tests/a.sh", lines)
        with self.subTest(phase='a pin only pull request takes the pins path'):
            self.setUp()
            (self.root / "os").mkdir(exist_ok=True)
            (self.root / "os/kernel-snapshot.env").write_text("PIN=x\n")
            manifest = ci_select.path_suites.parse(
                '[[group]]\nname = "os"\ninputs = ["os/"]\nsuites = ["os/tests/a.sh"]\n')
            def plan(changes, **options):
                return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                           suite_manifest=manifest, **options)
            pins = plan(["os/kernel-snapshot.env"], allow_security_skip=True)
            self.assertTrue(pins["pins_required"])
            self.assertEqual(pins["pins"], ["os/kernel-snapshot.env"])
            self.assertFalse(pins["rust_required"])
            self.assertFalse(pins["security_required"])
            self.assertFalse(pins["suites_required"])
            self.assertEqual(pins["matrix"], {"include": []})
            # Pushes, full runs and mixed changes keep every check.
            for changes, options in (
                (["os/kernel-snapshot.env"], {}),
                (["os/kernel-snapshot.env"], {"allow_security_skip": True, "force_full": True}),
                (["os/kernel-snapshot.env", "leaf/src/lib.rs"], {"allow_security_skip": True}),
                (["os/deleted-pin.env"], {"allow_security_skip": True}),
            ):
                with self.subTest(changes=changes, options=options):
                    other = plan(changes, **options)
                    self.assertFalse(other["pins_required"])
                    self.assertEqual(other["pins"], [])
                    self.assertTrue(other["security_required"])
            with tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "outputs"
                ci_select.emit_github(pins, output)
                lines = output.read_text().splitlines()
            self.assertIn("pins_required=true", lines)
            self.assertIn("pins=os/kernel-snapshot.env", lines)
        with self.subTest(phase='cross package readers are checked and tested when their inputs change'):
            self.setUp()
            # "base" reads leaf's sources and a shipped OS file from outside itself,
            # the way tilo-ui's design audit reads every app.
            manifest = ci_select.path_suites.parse(
                '[[cargo]]\npackage = "base"\ninputs = ["leaf/*.rs", "os/usr/share/x.json"]\n'
                'readers = ["base/tests/audit.rs"]\n')
            def plan(changes, **options):
                return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                           suite_manifest=manifest, **options)
            leaf = plan(["leaf/src/main.rs"])
            self.assertEqual(set(leaf["tested"]), {"leaf", "base", "tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types"})
            # The reader's code did not change, so its dependants are not re-checked.
            self.assertNotIn("middle", leaf["selected"])
            self.assertIn("read the changed files", leaf["reason"])
            row = leaf["matrix"]["include"][0]
            self.assertIn("base", row["test_packages"].split()[1::2])
            # A file no package owns selects the package that reads it.
            self.assertIn("base", plan(["os/usr/share/x.json"])["tested"])
            # Paths that no declared reader reads select nothing extra.
            self.assertNotIn("base", plan(["leaf/Cargo.toml"])["tested"])
            self.assertNotIn("base", plan(["os/usr/share/other.json"])["selected"])
            # Without a manifest nothing is attributed.
            self.assertNotIn("base", ci_select.make_plan(
                self.metadata, self.root, ["leaf/src/main.rs"], 2)["tested"])
        with self.subTest(phase='scoped readers run only their reading tests'):
            self.setUp()
            manifest = ci_select.path_suites.parse(
                '[[cargo]]\npackage = "base"\ninputs = ["leaf/*.rs", "os/usr/share/x.json"]\n'
                'readers = ["base/tests/audit.rs"]\n'
                'scope = { inputs = ["leaf/*.rs"], test = "it", filter = "test(/^audit::/)" }\n')
            def plan(changes):
                return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                           suite_manifest=manifest)
            leaf = plan(["leaf/src/main.rs"])
            # Still checked and linted, but not tested as a whole package.
            self.assertIn("base", leaf["selected"])
            self.assertNotIn("base", leaf["tested"])
            self.assertEqual(leaf["scoped_tests"], {"base": "--test it -E 'test(/^audit::/)'"})
            row = leaf["matrix"]["include"][0]
            self.assertNotIn("base", row["test_packages"].split()[1::2])
            self.assertEqual(json.loads(row["scoped_tests"]),
                             [{"package": "base", "test": "it", "filter": "test(/^audit::/)"}])
            # An unscoped input needs the whole suite, and then nothing is scoped.
            whole = plan(["leaf/src/main.rs", "os/usr/share/x.json"])
            self.assertIn("base", whole["tested"])
            self.assertEqual(whole["scoped_tests"], {})
            self.assertEqual(whole["matrix"]["include"][0]["scoped_tests"], "")
            # A package tested anyway (here: changed itself) is not also scoped.
            both = plan(["leaf/src/main.rs", "base/src/lib.rs"])
            self.assertIn("base", both["tested"])
            self.assertEqual(both["scoped_tests"], {})
        with self.subTest(phase='a read input is never treated as quality only'):
            self.setUp()
            (self.root / "DEVELOPMENT.md").write_text("")
            manifest = ci_select.path_suites.parse(
                '[[cargo]]\npackage = "leaf"\ninputs = ["DEVELOPMENT.md"]\nreaders = ["leaf/src/lib.rs"]\n')
            plan = ci_select.make_plan(self.metadata, self.root, ["DEVELOPMENT.md"], 2,
                                       suite_manifest=manifest)
            self.assertTrue(plan["rust_required"])
            self.assertIn("leaf", plan["tested"])
        with self.subTest(phase='production manifest selects the ui audit for an app change'):
            self.setUp()
            # The regression that motivated reader declarations: an app ported to
            # the resource framework broke tilo-ui's launcher audit, whose tests
            # the dependency graph never selected for an app-only change.
            manifest = ci_select.path_suites.load(REPO_ROOT)
            for path in ("apps/scratchpad/src/main.rs", "apps/scratchpad/src/lib.rs",
                         "apps/scratchpad/tilo-app.toml"):
                with self.subTest(path=path):
                    self.assertIn("tilo-ui", ci_select.path_suites.select_packages(manifest, [path]))
        with self.subTest(phase='suite manifest changes are quality only'):
            self.setUp()
            paths = ("tools/ci/suites.py", "tools/ci/suites.toml", "tools/tests/test_suites.py")
            for path in paths:
                (self.root / path).parent.mkdir(parents=True, exist_ok=True)
                (self.root / path).write_text("")
            for path in paths:
                with self.subTest(path=path):
                    plan = ci_select.make_plan(self.metadata, self.root, [path], 2,
                                               allow_security_skip=True)
                    self.assertFalse(plan["rust_required"])
                    self.assertFalse(plan["security_required"])

    def test_android_lane_follows_selected_apps_with_an_android_example(self):
        self.setUp()
        for package in self.metadata["packages"]:
            if package["name"] == "leaf":
                package["targets"] = [
                    {"name": "leaf", "kind": ["bin"]},
                    {"name": "leaf-android", "kind": ["example"]},
                ]
            if package["name"] == "middle":
                # Only an example named for its own package opts an app in.
                package["targets"] = [
                    {"name": "middle", "kind": ["lib"]},
                    {"name": "leaf-android", "kind": ["example"]},
                    {"name": "middle-android", "kind": ["bin"]},
                ]
        def plan(changes, **options):
            return ci_select.make_plan(self.metadata, self.root, changes, 2, **options)
        for changes, options in ((["base/src/lib.rs"], {}), (["leaf/src/android.rs"], {}),
                                 (["tilo-doc/src/lib.rs"], {"force_full": True})):
            with self.subTest(changes=changes, options=options):
                reached = plan(changes, **options)
                self.assertTrue(reached["android_required"])
                self.assertEqual(reached["android"], ["leaf"])
        unrelated = plan(["tilo-doc/src/lib.rs"])
        self.assertFalse(unrelated["android_required"])
        self.assertEqual(unrelated["android"], [])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            ci_select.emit_github(plan(["base/src/lib.rs"]), output)
            ci_select.emit_github(unrelated, output)
            lines = output.read_text().splitlines()
        self.assertEqual(
            [line for line in lines if line.startswith("android_")],
            ["android_required=true", "android_packages=-p leaf",
             "android_required=false", "android_packages="],
        )

    def test_wasm_lane_follows_selected_browser_crates(self):
        self.setUp()
        for package in self.metadata["packages"]:
            if package["name"] == "leaf":
                package["dependencies"] = [*package.get("dependencies", []),
                                           {"name": "wasm-bindgen", "kind": None}]
            if package["name"] == "tilo-doc":
                # A dev-dependency on wasm-bindgen does not make a browser crate.
                package["dependencies"] = [*package.get("dependencies", []),
                                           {"name": "wasm-bindgen", "kind": "dev"}]
        def plan(changes):
            return ci_select.make_plan(self.metadata, self.root, changes, 2)
        reached = plan(["base/src/lib.rs"])
        self.assertTrue(reached["wasm_required"])
        self.assertEqual(reached["wasm"], ["leaf"])
        unrelated = plan(["tilo-doc/src/lib.rs"])
        self.assertFalse(unrelated["wasm_required"])
        self.assertEqual(unrelated["wasm"], [])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            ci_select.emit_github(reached, output)
            ci_select.emit_github(unrelated, output)
            lines = output.read_text().splitlines()
        self.assertEqual(
            [line for line in lines if line.startswith("wasm_")],
            ["wasm_required=true", "wasm_packages=-p leaf",
             "wasm_required=false", "wasm_packages="],
        )

    def test_vendored_crate_tests_follow_their_own_files_only(self):
        self.setUp()
        def plan(changes, **options):
            return ci_select.make_plan(self.metadata, self.root, changes, 2, **options)
        every = sorted(ci_select.VENDORED_TEST_CRATES)
        self.assertIn("vendor/automerge", every)
        for changes, options in (
                (["vendor/automerge/src/op_set2/op_set/op_id_index.rs"], {}),
                (["vendor/automerge/Cargo.toml", "base/src/lib.rs"], {}),
                # The helpers its tests assert through.
                (["vendor/automerge-test/src/lib.rs"], {}),
                ([ci_select.VENDORED_TEST_RUNNER], {}),
                (["base/src/lib.rs"], {"force_full": True})):
            with self.subTest(changes=changes, options=options):
                reached = plan(changes, **options)
                self.assertTrue(reached["vendored_required"])
                self.assertEqual(reached["vendored"],
                                 every if changes[0] == ci_select.VENDORED_TEST_RUNNER
                                 or options else ["vendor/automerge"])
        # Workspace crates, the workspace lock, other vendored crates and a push
        # to main that runs every path suite leave them to the nightly.
        for changes, options in ((["base/src/lib.rs"], {}), (["Cargo.lock"], {}),
                                 (["vendor/automerge-other/src/lib.rs"], {}),
                                 (["vendor/iroh/src/lib.rs"], {}),
                                 (["crates/doc/tilo-automerge/src/lib.rs"], {}),
                                 (["base/src/lib.rs"], {"all_suites": True})):
            with self.subTest(changes=changes, options=options):
                unrelated = plan(changes, **options)
                self.assertFalse(unrelated["vendored_required"])
                self.assertEqual(unrelated["vendored"], [])
        # A re-check runs them only if the base's own changes reached them.
        fork = ["vendor/automerge/src/lib.rs"]
        self.assertEqual(plan(fork, only={"packages": set(), "suites": set(),
                                          "vendored": {"vendor/automerge"}})["vendored"],
                         ["vendor/automerge"])
        self.assertEqual(plan(fork, only={"packages": set(), "suites": set(),
                                          "vendored": set()})["vendored"], [])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            ci_select.emit_github(plan(fork), output)
            ci_select.emit_github(plan(["base/src/lib.rs"]), output)
            lines = output.read_text().splitlines()
        self.assertEqual(
            [line for line in lines if line.startswith("vendored_")],
            ["vendored_required=true", "vendored_crates=vendor/automerge",
             "vendored_required=false", "vendored_crates="],
        )
        # Every registered crate exists, runs as its own workspace, and is
        # what the runner tests when given no argument.
        for crate, inputs in ci_select.VENDORED_TEST_CRATES.items():
            with self.subTest(crate=crate):
                self.assertIn(f"{crate}/", inputs)
                self.assertIn("[workspace]",
                              (REPO_ROOT / crate / "Cargo.toml").read_text().splitlines())
                for path in inputs:
                    self.assertTrue((REPO_ROOT / path).is_dir(), path)
        runner = (REPO_ROOT / ci_select.VENDORED_TEST_RUNNER).read_text()
        self.assertIn("from ci.affected_packages import VENDORED_TEST_CRATES", runner)

    def test_wasm_lane_builds_selected_examples_and_all_when_the_web_build_changes(self):
        self.setUp()
        self.metadata["packages"].append({
            "name": "example-counter",
            "manifest_path": str(self.root / "examples/apps/counter/Cargo.toml"),
            "targets": [{"name": "example-counter"}],
            "dependencies": [{"name": "base", "path": str(self.root / "base")}],
        })
        def plan(changes):
            return ci_select.make_plan(self.metadata, self.root, changes, 2)
        reached = plan(["base/src/lib.rs"])
        self.assertTrue(reached["wasm_required"])
        self.assertEqual(reached["wasm"], [])
        self.assertEqual(reached["web_examples"], ["example-counter"])
        unrelated = plan(["tilo-doc/src/lib.rs"])
        self.assertFalse(unrelated["wasm_required"])
        self.assertEqual(unrelated["web_examples"], [])
        build = plan(["crates/browser/tilo-browser-runtime/scripts/build.sh"])
        self.assertTrue(build["wasm_required"])
        self.assertEqual(build["web_examples"], ["example-counter"])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            ci_select.emit_github(reached, output)
            ci_select.emit_github(unrelated, output)
            lines = output.read_text().splitlines()
        self.assertEqual(
            [line for line in lines if line.startswith("web_examples=")],
            ["web_examples=example-counter", "web_examples="],
        )

    def test_dependency_graph_selects_owners_and_dependants_or_falls_back_to_full(self):
        with self.subTest(phase='reverse dependency closure'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
            self.assertEqual(set(plan["selected"]), {"base", "middle", "leaf", "tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types"})
            self.assertFalse(plan["full"])
            # Direct dependants are tested; further reverse dependencies are checked.
            self.assertEqual(set(plan["tested"]), {"base", "middle", "tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types"})
            self.assertEqual(plan["dependants_tested_nightly"], [])
            self.assertEqual(len(plan["matrix"]["include"]), 1)
            row = plan["matrix"]["include"][0]
            self.assertEqual(row["arch"], "x86_64")
            self.assertEqual(set(row["test_packages"].split()[1::2]), set(plan["tested"]))
        with self.subTest(phase='direct dependants beyond the budget are left to nightly'):
            self.setUp()
            original = ci_select.REVERSE_TEST_COST_BUDGET
            try:
                ci_select.REVERSE_TEST_COST_BUDGET = 0
                plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
            finally:
                ci_select.REVERSE_TEST_COST_BUDGET = original
            self.assertIn("middle", plan["selected"])
            self.assertNotIn("middle", plan["tested"])
            self.assertEqual(plan["dependants_tested_nightly"], ["base"])
        with self.subTest(phase='cheapest direct dependants are tested first'):
            self.setUp()
            packages, _ = ci_select.package_maps(self.metadata, self.root)
            reverse = ci_select.reverse_dependencies(packages, self.root)
            one = ci_select.package_cost(packages["middle"])
            added, deferred = ci_select.direct_dependants_within_budget(
                {"base", "middle"}, reverse, packages, {"base", "middle"}, budget=one)
            # middle's dependant (leaf) and base's (middle, already tested) fit;
            # nothing is deferred.
            self.assertEqual(added, {"leaf"})
            self.assertEqual(deferred, [])
            added, deferred = ci_select.direct_dependants_within_budget(
                {"base", "middle"}, reverse, packages, {"base", "middle"}, budget=one - 1)
            self.assertEqual(added, set())
            self.assertEqual(deferred, ["middle"])
        with self.subTest(phase='leaf change does not select dependencies'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 2)
            self.assertIn("leaf", plan["selected"])
            self.assertNotIn("base", plan["selected"])
            self.assertEqual(plan["shard_count"], 1)
        with self.subTest(phase='package manifest change checks dependants and tests owner'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["base/Cargo.toml"], 2)
            self.assertIn("leaf", plan["selected"])
            self.assertIn("base", plan["tested"])
            self.assertNotIn("leaf", plan["tested"])
        with self.subTest(phase='build input forces full workspace'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
            self.assertTrue(plan["full"])
            self.assertEqual(plan["selected_count"], plan["workspace_count"])
            self.assertEqual(plan["shard_count"], 1)  # small workspace stays cheap
        with self.subTest(phase='vendored crates are build inputs'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["vendor/winit/src/lib.rs"], 2)
            self.assertTrue(plan["full"])
        with self.subTest(phase='unowned deleted crate path forces full workspace'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["apps/removed/Cargo.toml"], 2)
            self.assertTrue(plan["full"])
        with self.subTest(phase='dependency and unknown inputs keep full x86 feedback'):
            self.setUp()
            for changes in [["Cargo.lock"], ["apps/deleted/src/lib.rs"]]:
                with self.subTest(changes=changes):
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                    self.assertTrue(plan["full"])
                    self.assertEqual(plan["tested"], plan["selected"])
                    self.assertEqual(plan["selected_count"], plan["workspace_count"])
                    self.assertEqual([row["arch"] for row in plan["matrix"]["include"]], ["x86_64"])
        with self.subTest(phase='unknown inputs retain existing contract test fallback'):
            self.setUp()
            for changes in [["unknown-input"], []]:
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                self.assertEqual(set(plan["tested"]), ci_select.ALWAYS_PACKAGES)
                self.assertTrue(plan["rust_required"])

    def test_shards_and_matrices_cover_every_package_once_per_architecture(self):
        with self.subTest(phase='global inputs balance large workspaces without losing coverage'):
            self.setUp()
            # Enough genuine package targets/dependencies to exceed the budget,
            # with uneven costs so this exercises the production balancer.
            for index in range(100):
                name = f"app-{index:03}"
                self.metadata["packages"].append({
                    "name": name,
                    "manifest_path": str(self.root / "apps" / name / "Cargo.toml"),
                    "targets": [{"name": name}] * (1 + index % 5),
                    "dependencies": [{"name": f"external-{i}"} for i in range(index % 30)],
                })
            for path in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "crates/unmapped/src/lib.rs"):
                with self.subTest(path=path):
                    plan = ci_select.make_plan(self.metadata, self.root, [path], 99)
                    rows = plan["clippy_matrix"]["include"]
                    self.assertEqual(plan["shard_count"], 2)
                    self.assertEqual(len(rows), 2)
                    self.assertEqual({row["arch"] for row in rows}, {"x86_64"})
                    self.assertEqual({row["runner"] for row in rows}, {ci_select.NATIVE_RUNNERS["x86_64"]})
                    for field, field_rows in (("packages", rows),
                                              ("test_packages", plan["matrix"]["include"])):
                        names = [name for row in field_rows for name in row[field].split()[1::2]]
                        self.assertEqual(sorted(names), plan["selected"])
                        self.assertEqual(len(names), len(set(names)))
                    self.assertEqual(sum(row["estimated_cost"] for row in rows), plan["estimated_cost"])
                    self.assertLess(max(row["estimated_cost"] for row in rows), plan["estimated_cost"] * .6)
                    reversed_metadata = {"packages": list(reversed(self.metadata["packages"]))}
                    self.assertEqual(plan, ci_select.make_plan(reversed_metadata, self.root, [path], 99))
            single = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 1)
            self.assertEqual(single["shard_count"], 1)
            # A normal narrow edit does not fan out just because the workspace grew.
            narrow = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 99)
            self.assertEqual(narrow["shard_count"], 1)
        with self.subTest(phase='explicit full selects every package even without changes'):
            self.setUp()
            for changes in ([], ["docs/development-signing.md"]):
                with self.subTest(changes=changes):
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=True)
                    self.assertTrue(plan["full"])
                    self.assertEqual(plan["selected_count"], plan["workspace_count"])
                    for arch in ci_select.NATIVE_RUNNERS:
                        rows = [row for row in plan["matrix"]["include"] if row["arch"] == arch]
                        selected = " ".join(row["packages"] for row in rows).split()[1::2]
                        self.assertEqual(sorted(selected), plan["selected"])
        with self.subTest(phase='balancing assigns every package once'):
            self.setUp()
            packages, _ = ci_select.package_maps(self.metadata, self.root)
            shards = ci_select.balance(set(packages), packages, 2)
            args = " ".join(shard["packages"] for shard in shards).split()
            names = args[1::2]
            self.assertEqual(sorted(names), sorted(packages))
            self.assertEqual(len(names), len(set(names)))
        with self.subTest(phase='recorded test time separates slow suites'):
            self.setUp()
            packages = {name: {"name": name, "targets": [], "dependencies": []}
                        for name in ("slow-a", "slow-b", "c", "d")}
            # Equal structure: without timings the two slow suites may share a shard.
            seconds = {"slow-a": 500, "slow-b": 400, "c": 1}
            shards = ci_select.balance(set(packages), packages, 2, seconds)
            placed = [set(shard["packages"].split()[1::2]) for shard in shards]
            self.assertTrue(any("slow-a" in names for names in placed))
            self.assertFalse(any({"slow-a", "slow-b"} <= names for names in placed))
            self.assertEqual(sorted(shard["recorded_test_seconds"] for shard in shards), [401, 500])
            # The structural estimate keeps its meaning.
            self.assertEqual(sum(shard["estimated_cost"] for shard in shards),
                             sum(ci_select.package_cost(pkg) for pkg in packages.values()))
        with self.subTest(phase='fsync-heavy suites keep to separate test shards'):
            self.setUp()
            names = ("tilo-fs", "tilo-relay", "tilo-action-broker", "light")
            packages = {name: {"name": name, "targets": [], "dependencies": []} for name in names}
            # By time alone tilo-fs and the relay would share the lighter
            # shard. Placement breaks equal weights by name, so theirs differ.
            seconds = {"tilo-action-broker": 900, "tilo-fs": 100, "tilo-relay": 99, "light": 1}
            shards = ci_select.balance(set(packages), packages, 2, seconds,
                                       ci_select.TEST_SHARD_APART)
            placed = [set(shard["packages"].split()[1::2]) for shard in shards]
            self.assertIn({"tilo-relay", "tilo-action-broker"}, placed)
            self.assertTrue(any("tilo-fs" in names for names in placed))
            # One shard still takes everything.
            (only,) = ci_select.balance(set(packages), packages, 1, seconds,
                                        ci_select.TEST_SHARD_APART)
            self.assertEqual(only["package_count"], 4)
        with self.subTest(phase='unrecorded packages carry the default test time'):
            self.setUp()
            packages = {name: {"name": name, "targets": [], "dependencies": []}
                        for name in ("recorded", "new-a", "new-b", "new-c")}
            default = ci_select.UNRECORDED_TEST_SECONDS
            self.assertGreater(default, 0)
            # Counted as free, the three new suites would all join the shard
            # beside the recorded one; with the default they spread.
            recorded = {"recorded": default + 5}
            shards = ci_select.balance(set(packages), packages, 2, recorded)
            placed = [set(shard["packages"].split()[1::2]) for shard in shards]
            self.assertFalse(any({"new-a", "new-b", "new-c"} <= names for names in placed))
            self.assertEqual(sorted(shard["recorded_test_seconds"] for shard in shards),
                             [0, default + 5])
            self.assertEqual(ci_select.expected_test_seconds("new-a", recorded), default)
            # Without any recorded timings, balancing stays structural.
            self.assertEqual(ci_select.expected_test_seconds("new-a", {}), 0)
            names = {f"new-{index}" for index in range(ci_select.TEST_SHARD_SECONDS // default + 1)}
            self.assertEqual(ci_select.useful_test_shard_count(names, {"recorded": 1}), 2)
            self.assertEqual(ci_select.useful_test_shard_count(names, {}), 1)
        with self.subTest(phase='recorded test time splits test jobs, not type-check shards'):
            self.setUp()
            changes = ["base/src/lib.rs"]
            seconds = {"base": 400, "middle": 350, "tilo-doc": 150, "tilo-app-package": 10}
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2, test_seconds=seconds)
            self.assertEqual(plan["shard_count"], 1)
            self.assertEqual(len(plan["clippy_matrix"]["include"]), 1)
            rows = plan["matrix"]["include"]
            self.assertEqual(plan["test_shard_count"], 2)
            self.assertEqual([row["shard"] for row in rows], [1, 2])
            placed = [set(row["test_packages"].split()[1::2]) for row in rows]
            self.assertFalse(any({"base", "middle"} <= names for names in placed))
            names = [name for group in placed for name in group]
            self.assertEqual(sorted(names), plan["tested"])
            original = ci_select.MAX_TEST_SHARDS
            try:
                ci_select.MAX_TEST_SHARDS = 1
                capped = ci_select.make_plan(self.metadata, self.root, changes, 2, test_seconds=seconds)
            finally:
                ci_select.MAX_TEST_SHARDS = original
            self.assertEqual(capped["test_shard_count"], 1)
        with self.subTest(phase='a package too heavy to share a job gets its own shard'):
            self.setUp()
            changes = ["base/src/lib.rs"]
            packages, _ = ci_select.package_maps(self.metadata, self.root)
            heavy = ci_select.DEDICATED_TEST_WEIGHT  # seconds, so twice the target weight
            seconds = {"base": heavy, "middle": 100, "tilo-doc": 150, "tilo-app-package": 10}
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2, test_seconds=seconds)
            rows = plan["matrix"]["include"]
            # The dedicated shard comes first and holds only the heavy package.
            self.assertEqual(rows[0]["test_packages"], "-p base")
            self.assertEqual(rows[0]["scoped_tests"], "")
            self.assertEqual([row["shard"] for row in rows], list(range(1, len(rows) + 1)))
            others = [name for row in rows[1:] for name in row["test_packages"].split()[1::2]]
            self.assertNotIn("base", others)
            self.assertEqual(sorted(others + ["base"]), plan["tested"])
            self.assertEqual(plan["test_shard_count"], len(rows))
            self.assertNotIn("partition_matrix", plan)
            # A package whose build alone exceeds the target also gets one; one
            # without recorded time shares a shard.
            self.assertEqual(ci_select.dedicated_test_shards({"base"}, packages, seconds)[0]["packages"],
                             "-p base")
            self.assertEqual(ci_select.dedicated_test_shards(set(packages) - {"base"}, packages, {}), [])
        with self.subTest(phase='recorded test time never changes selection or budgets'):
            self.setUp()
            changes = ["leaf/src/lib.rs"]
            plain = ci_select.make_plan(self.metadata, self.root, changes, 2)
            packages, _ = ci_select.package_maps(self.metadata, self.root)
            timed = ci_select.make_plan(self.metadata, self.root, changes, 2,
                                        test_seconds={name: 10_000 for name in packages})
            for key in ("selected", "tested", "shard_count", "estimated_cost",
                        "dependants_tested_nightly"):
                self.assertEqual(plain[key], timed[key], key)
        with self.subTest(phase='checked in test seconds are valid'):
            self.setUp()
            seconds = ci_select.recorded_test_seconds()
            self.assertTrue(seconds, "tools/ci/recorded_test_time.json should carry recorded timings")
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "seconds.json"
                self.assertEqual(ci_select.recorded_test_seconds(path), {})
                for invalid in ('[]', '{"a": 1.5}', '{"a": -1}', '{"a": "1"}', '{"a": true}'):
                    with self.subTest(invalid=invalid):
                        path.write_text(invalid)
                        with self.assertRaises(ValueError):
                            ci_select.recorded_test_seconds(path)
        with self.subTest(phase='each selected package runs once on each native architecture'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2, force_full=True)
            rows = plan["matrix"]["include"]
            self.assertEqual({row["arch"] for row in rows}, {"x86_64", "aarch64"})
            for arch, runner in ci_select.NATIVE_RUNNERS.items():
                native = [row for row in rows if row["arch"] == arch]
                self.assertEqual({row["runner"] for row in native}, {runner})
                names = " ".join(row["packages"] for row in native).split()[1::2]
                self.assertEqual(sorted(names), plan["selected"])
                self.assertEqual(len(native), plan["test_shard_count"])
                tested = " ".join(row["test_packages"] for row in native).split()[1::2]
                self.assertEqual(sorted(tested), plan["selected"])
        with self.subTest(phase='cost threshold adds shards only when useful'):
            self.setUp()
            packages, _ = ci_select.package_maps(self.metadata, self.root)
            names = set(packages)
            original = ci_select.SHARD_COST_BUDGET
            try:
                ci_select.SHARD_COST_BUDGET = 1
                self.assertEqual(ci_select.useful_shard_count(names, packages, 2), 2)
                ci_select.SHARD_COST_BUDGET = 10_000
                self.assertEqual(ci_select.useful_shard_count(names, packages, 2), 1)
            finally:
                ci_select.SHARD_COST_BUDGET = original
        with self.subTest(phase='github outputs include adaptive shard metrics'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 2)
            output = self.root / "github-output"
            ci_select.emit_github(plan, output)
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(values["shard_count"], "1")
            self.assertEqual(values["estimated_cost"], str(plan["estimated_cost"]))
            self.assertEqual(json.loads(values["matrix"]), plan["matrix"])
            self.assertEqual(json.loads(values["clippy_matrix"]), plan["clippy_matrix"])
            self.assertEqual(values["test_shard_count"], "1")
            # An x86_64-only plan needs no standalone check: Clippy covers it.
            self.assertEqual(json.loads(values["check_matrix"]), {"include": []})
            self.assertEqual(values["check_required"], "false")
        with self.subTest(phase='clippy matrix lints each package set once on x86'):
            self.setUp()
            self.install_builder_recipe()
            plan = ci_select.make_plan(
                self.metadata, self.root, ["os/Containerfile.builder"], 2, force_full=True)
            rows = plan["matrix"]["include"]
            self.assertEqual({row["arch"] for row in rows}, set(ci_select.NATIVE_RUNNERS))
            for row in rows:
                self.assertEqual(row["runner"], ci_select.NATIVE_RUNNERS[row["arch"]])
                self.assertEqual(row["check_runner"], row["runner"])
            clippy = plan["clippy_matrix"]["include"]
            self.assertEqual({row["arch"] for row in clippy}, {"x86_64"})
            self.assertEqual(" ".join(row["packages"] for row in clippy).split()[1::2],
                             plan["selected"])
            # Architectures Clippy does not lint keep a standalone all-target check.
            check = plan["check_matrix"]["include"]
            self.assertEqual({row["arch"] for row in check}, set(ci_select.NATIVE_RUNNERS) - {"x86_64"})
            for row in clippy + check:
                self.assertEqual(row["runner"], ci_select.NATIVE_RUNNERS[row["arch"]])
                self.assertEqual(row["check_runner"], row["runner"])
            self.assertEqual(sorted(row["packages"] for row in check),
                             sorted(row["packages"] for row in clippy))
            output = self.root / "github-output"
            ci_select.emit_github(plan, output)
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(values["check_required"], "true")

    def test_documentation_changes_omit_rust_only_when_audited_and_unmixed(self):
        with self.subTest(phase='design only has explicit empty rust matrix'):
            self.setUp()
            plan = ci_select.make_plan(self.metadata, self.root, ["docs/designs/ci.md"], 2)
            self.assertFalse(plan["rust_required"])
            self.assertTrue(plan["security_required"])
            self.assertEqual(plan["selected_count"], 0)
            self.assertEqual(plan["shard_count"], 0)
            self.assertEqual(plan["matrix"], {"include": []})
            self.assertEqual(plan["clippy_matrix"], {"include": []})
            output = self.root / "outputs"
            ci_select.emit_github(plan, output)
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(values["rust_required"], "false")
            self.assertEqual(values["security_required"], "true")
            self.assertEqual(values["selected_count"], "0")
        with self.subTest(phase='design exception never overrides code or manual full'):
            self.setUp()
            for changes, full in [
                (["docs/designs/ci.md"], True),
                (["docs/designs/ci.md", "leaf/src/lib.rs"], False),
                (["docs/designs/ci.md", "Cargo.lock"], False),
            ]:
                with self.subTest(changes=changes, full=full):
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=full)
                    self.assertTrue(plan["rust_required"])
                    self.assertIn("leaf", plan["selected"])
        with self.subTest(phase='unknown docs and embedded code retain contract checks'):
            self.setUp()
            for path in ["docs/contracts.md", "docs/packaging.md", "docs/app-development.md",
                         "crates/sdk/ui-core/CHARTS.md", "docs/unknown.md", "docs/designs/fixture.rs",
                         "docs/designs/../contract.md", "unknown-input"]:
                with self.subTest(path=path):
                    plan = ci_select.make_plan(self.metadata, self.root, [path], 2)
                    self.assertTrue(plan["rust_required"])
                    self.assertTrue(ci_select.ALWAYS_PACKAGES.issubset(plan["selected"]))
        with self.subTest(phase='audited developer docs omit only rust'):
            self.setUp()
            paths = ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md", "docs/headless-testing.md"]
            for changes in [[path] for path in paths] + [paths + ["docs/designs/ci.md"]]:
                with self.subTest(changes=changes):
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                    self.assertFalse(plan["rust_required"])
                    self.assertEqual(plan["selected_count"], 0)
                    self.assertEqual(plan["matrix"], {"include": []})
        with self.subTest(phase='headless guide and agent instructions need no native jobs'):
            self.setUp()
            changes = ["AGENTS.md", "docs/headless-testing.md"]
            for name in changes:
                path = self.root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("Developer test commands.\n")
            plan = ci_select.make_plan(
                self.metadata, self.root, changes, 2, allow_security_skip=True)
            self.assertFalse(plan["rust_required"])
            self.assertFalse(plan["security_required"])
            self.assertEqual(plan["matrix"], {"include": []})
            (self.root / changes[1]).unlink()
            deleted = ci_select.make_plan(
                self.metadata, self.root, changes, 2, allow_security_skip=True)
            self.assertTrue(deleted["security_required"])
        with self.subTest(phase='developer docs do not mask mixed changes or full override'):
            self.setUp()
            for doc in ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md", "docs/headless-testing.md"]:
                for extra, force in [(None, True), ("leaf/src/lib.rs", False),
                                     ("Cargo.lock", False), ("docs/packaging.md", False)]:
                    with self.subTest(doc=doc, extra=extra, force=force):
                        changes = [doc] + ([extra] if extra else [])
                        plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=force)
                        self.assertTrue(plan["rust_required"])
                        if force or extra == "Cargo.lock":
                            self.assertTrue(plan["full"])
                            self.assertEqual(plan["selected_count"], plan["workspace_count"])
                        if extra == "leaf/src/lib.rs":
                            self.assertIn("leaf", plan["selected"])

    def test_security_selector_skips_only_audited_observed_pull_request_inputs(self):
        with self.subTest(phase='security skip is limited to audited pull request inputs'):
            self.setUp()
            self.install_quality_helpers()
            prose = [
                "AGENTS.md",
                "DEVELOPMENT.md",
                "docs/ci-performance.md",
                "docs/ci-emergency-relief.md",
                "docs/headless-testing.md",
                "docs/designs/ci.md",
            ]
            for path in prose:
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("audited prose\n")
            for path in prose:
                with self.subTest(path=path):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, [path], 2, allow_security_skip=True)
                    self.assertFalse(plan["security_required"])
            for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
                for changes in ([source], [test], [source, test, "docs/designs/ci.md"]):
                    with self.subTest(changes=changes):
                        plan = ci_select.make_plan(
                            self.metadata, self.root, changes, 2, allow_security_skip=True)
                        self.assertFalse(plan["security_required"])
        with self.subTest(phase='security selector defaults true for non pr and full runs'):
            self.setUp()
            for changes, options in [
                (["docs/designs/ci.md"], {}),
                (["docs/designs/ci.md"], {"force_full": True}),
                (["tools/ci/affected_packages.py"], {}),
            ]:
                with self.subTest(changes=changes, options=options):
                    self.install_quality_helpers()
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2, **options)
                    self.assertTrue(plan["security_required"])
        with self.subTest(phase='security selector rejects control sensitive and mixed inputs'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            excluded = [
                ".forgejo/workflows/check.yml",
                "os/tests/security-boundaries.sh",
                "tools/run-security-boundary-invariants.sh",
                "Cargo.toml",
                "os/external-assets/github-assets.tsv",
                "SECURITY.md",
                "tools/unknown.py",
            ]
            for path in excluded:
                with self.subTest(path=path):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, [path], 2, allow_security_skip=True)
                    self.assertTrue(plan["security_required"])
            mixed = ci_select.make_plan(
                self.metadata,
                self.root,
                ["tools/ci/affected_packages.py", "SECURITY.md"],
                2,
                allow_security_skip=True,
            )
            self.assertTrue(mixed["security_required"])
        with self.subTest(phase='security selector fails closed for deleted or unobserved helpers'):
            self.setUp()
            deleted_prose = self.root / "docs/designs/deleted.md"
            deleted_prose.parent.mkdir(parents=True, exist_ok=True)
            deleted_prose.write_text("audited prose\n")
            deleted_prose.unlink()
            self.assertTrue(ci_select.make_plan(
                self.metadata,
                self.root,
                ["docs/designs/deleted.md"],
                2,
                allow_security_skip=True,
            )["security_required"])
            for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
                for missing in (source, test):
                    self.install_quality_helpers()
                    (self.root / missing).unlink()
                    with self.subTest(source=source, missing=missing):
                        plan = ci_select.make_plan(
                            self.metadata,
                            self.root,
                            [source],
                            2,
                            allow_security_skip=True,
                        )
                        self.assertTrue(plan["security_required"])

    def install_builder_recipe(self) -> None:
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")

    def test_builder_recipe_skips_compile_only_for_an_unchanged_image_alone(self):
        with self.subTest(phase='builder recipe only omits compile in unchanged image'):
            self.setUp()
            self.install_builder_recipe()
            self.install_quality_helpers()
            for extras in ([], ["docs/ci-performance.md"], ["tools/ci/affected_packages.py"]):
                with self.subTest(extras=extras):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, ["os/Containerfile.builder", *extras], 2)
                    self.assertFalse(plan["rust_required"])
                    self.assertEqual(plan["selected"], [])
                    self.assertEqual(plan["tested"], [])
                    self.assertEqual(plan["matrix"], {"include": []})
                    self.assertIn("candidate image qualification is separate", plan["reason"])
        with self.subTest(phase='builder recipe mixed inputs and deletion retain full checks'):
            self.setUp()
            self.install_builder_recipe()
            for extra in ("leaf/src/lib.rs", "Cargo.lock", ".cargo/config.toml",
                          "rust-toolchain.toml", ".forgejo/workflows/check.yml"):
                with self.subTest(extra=extra):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, ["os/Containerfile.builder", extra], 2)
                    self.assertTrue(plan["rust_required"])
                    self.assertTrue(plan["full"])
                    self.assertEqual(plan["selected_count"], plan["workspace_count"])
            (self.root / "os/Containerfile.builder").unlink()
            deleted = ci_select.make_plan(self.metadata, self.root, ["os/Containerfile.builder"], 2)
            self.assertTrue(deleted["full"])
            self.assertTrue(deleted["rust_required"])
        with self.subTest(phase='builder recipe explicit acceptance checks both architectures'):
            self.setUp()
            self.install_builder_recipe()
            plan = ci_select.make_plan(
                self.metadata, self.root, ["os/Containerfile.builder"], 2, force_full=True)
            self.assertTrue(plan["full"])
            self.assertEqual(plan["selected_count"], plan["workspace_count"])
            self.assertEqual(plan["tested"], plan["selected"])
            self.assertEqual({row["arch"] for row in plan["matrix"]["include"]},
                             set(ci_select.NATIVE_RUNNERS))
        with self.subTest(phase='builder recipe deletion and mixed inputs stay conservative'):
            self.setUp()
            recipe = self.root / "os/Containerfile.builder"
            recipe.parent.mkdir(parents=True, exist_ok=True)
            recipe.write_text("FROM scratch\n")
            for extra in ("leaf/src/lib.rs", "Cargo.lock", ".cargo/config.toml",
                          ".forgejo/workflows/unknown.yml"):
                with self.subTest(extra=extra):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, ["os/Containerfile.builder", extra], 2)
                    self.assertTrue(plan["rust_required"])
                    self.assertTrue(plan["full"])
            recipe.unlink()
            deleted = ci_select.make_plan(
                self.metadata, self.root, ["os/Containerfile.builder"], 2)
            self.assertTrue(deleted["rust_required"])
            self.assertTrue(deleted["full"])
        with self.subTest(phase='builder observer runs in mandatory policy suite'):
            self.setUp()
            policy = (REPO_ROOT / "tools/policy/run.sh").read_text()
            self.assertIn(
                "run_tests 'Builder candidate workflow tests' python3 "
                "os/tests/test_builder_candidate_workflow.py",
                policy,
            )

    def test_ci_helpers_and_control_plane_are_quality_only_while_exactly_observed(self):
        with self.subTest(phase='ci scripts do not force unrelated rust rebuilds'):
            self.setUp()
            plan = ci_select.make_plan(
                self.metadata,
                self.root,
                [
                    ".forgejo/workflows/check.yml",
                    "os/tests/imager_ephemeral_runner.sh",
                    "scripts/check-install-contract.py",
                    "tools/ci/runners/imager-ephemeral-linux.sh",
                ],
                2,
            )
            self.assertFalse(plan["full"])
            self.assertEqual(set(plan["selected"]), {"tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types"})
            self.assertEqual(plan["shard_count"], 1)
        with self.subTest(phase='tested ci helper changes have no native shards'):
            self.setUp()
            self.install_quality_helpers()
            for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
                for changes in ([source], [test], [source, test, "docs/ci-emergency-relief.md"]):
                    with self.subTest(changes=changes):
                        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                        self.assertFalse(plan["rust_required"])
                        self.assertEqual(plan["selected"], [])
                        self.assertEqual(plan["tested"], [])
                        self.assertEqual(plan["selected_count"], 0)
                        self.assertEqual(plan["estimated_cost"], 0)
                        self.assertEqual(plan["shard_count"], 0)
                        self.assertEqual(plan["matrix"], {"include": []})
                        self.assertIn("mandatory repository-policy tests", plan["reason"])
        with self.subTest(phase='ci helper exception preserves unknown mixed and full checks'):
            self.setUp()
            self.install_quality_helpers()
            for source in ci_select.QUALITY_ONLY_HELPERS:
                for extra in ("tools/unknown.py", ".forgejo/workflows/check.yml",
                              "os/tests/security-boundaries.sh",
                              "leaf/src/lib.rs", "Cargo.lock", "tools/../tools/ci/affected_packages.py"):
                    with self.subTest(source=source, extra=extra):
                        plan = ci_select.make_plan(self.metadata, self.root, [source, extra], 2)
                        self.assertTrue(plan["rust_required"])
                        if extra == "leaf/src/lib.rs":
                            self.assertIn("leaf", plan["selected"])
                        if extra == "Cargo.lock":
                            self.assertTrue(plan["full"])
                            self.assertEqual(plan["selected_count"], plan["workspace_count"])
                with self.subTest(source=source, force_full=True):
                    full = ci_select.make_plan(self.metadata, self.root, [source], 2, force_full=True)
                    self.assertTrue(full["full"])
                    self.assertEqual(full["selected_count"], full["workspace_count"])
                    self.assertEqual({row["arch"] for row in full["matrix"]["include"]},
                                     set(ci_select.NATIVE_RUNNERS))
            self.assertTrue(ci_select.make_plan(self.metadata, self.root, [], 2)["rust_required"])
        with self.subTest(phase='missing helper or test retains native checks'):
            self.setUp()
            for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
                for missing in (source, test):
                    self.install_quality_helpers()
                    (self.root / missing).unlink()
                    with self.subTest(source=source, missing=missing):
                        plan = ci_select.make_plan(self.metadata, self.root, [source, test], 2)
                        self.assertTrue(plan["rust_required"])
        with self.subTest(phase='ci control plane change does not compile the workspace'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            recipe = self.root / "os/Containerfile.builder"
            recipe.parent.mkdir(parents=True, exist_ok=True)
            recipe.write_text("FROM scratch\n")
            # Exact file set from PR #86, whose first two checks selected all 140
            # Rust packages even though every change was CI orchestration or its test.
            changes = [
                ".forgejo/workflows/check.yml",
                ".forgejo/workflows/ci-legacy-apps.yml",
                ".forgejo/workflows/release-relay.yml",
                "os/Containerfile.builder",
                "os/tests/test_builder_candidate_workflow.py",
                "tools/ci/affected_packages.py",
                "tools/policy/run.sh",
                "tools/tests/test_ci_check_aggregate.py",
                "tools/tests/test_ci_queue_relief.py",
                "tools/tests/test_ci_receipt.sh",
                "tools/tests/test_ci_select.py",
                "tools/tests/test_workflow_package_paths.py",
            ]
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertFalse(plan["rust_required"])
            self.assertFalse(plan["full"])
            self.assertEqual(plan["selected"], [])
            self.assertEqual(plan["matrix"], {"include": []})
        with self.subTest(phase='check workflow and builder resolution contract need no rust'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            changes = [
                ".forgejo/workflows/check.yml",
                "tools/tests/test_ci_check_aggregate.py",
                "tools/tests/test_resolve_ci_builder.py",
                "tools/ci/affected_packages.py",
                "tools/tests/test_ci_select.py",
            ]
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertFalse(plan["rust_required"])
            self.assertEqual(plan["selected"], [])
            self.assertTrue(plan["security_required"])
            # A missing mandatory observer removes the exception.
            (self.root / "tools/tests/test_resolve_ci_builder.py").unlink()
            self.assertTrue(ci_select.make_plan(self.metadata, self.root, changes, 2)["rust_required"])
        with self.subTest(phase='mold builder policy does not compile unrelated rust'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            for path in (
                ".forgejo/workflows/build-ci-builder.yml",
                "os/tests/test_builder_candidate_workflow.py",
                "tools/ci/native-mold-linker.sh",
                "tools/ci/verify-native-mold.sh",
                "tools/tests/test_ci_native_mold.py",
            ):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("checked by mandatory policy\n")
            recipe = self.root / "os/Containerfile.builder"
            recipe.parent.mkdir(parents=True, exist_ok=True)
            recipe.write_text("FROM scratch\n")
            changes = [
                ".forgejo/workflows/build-ci-builder.yml",
                "os/Containerfile.builder",
                "os/tests/test_builder_candidate_workflow.py",
                "tools/ci/native-mold-linker.sh",
                "tools/ci/verify-native-mold.sh",
                "tools/tests/test_ci_native_mold.py",
            ]
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertFalse(plan["rust_required"])
            self.assertEqual(plan["selected"], [])
            self.assertEqual(plan["matrix"], {"include": []})
        with self.subTest(phase='builder cache policy does not compile unrelated rust'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            changes = [
                ".forgejo/workflows/build-ci-builder.yml",
                "tools/tests/test_cargo_download_cache_action.py",
            ]
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertFalse(plan["rust_required"])
        with self.subTest(phase='builder reuse policy does not compile unrelated rust'):
            self.setUp()
            self.install_quality_helpers()
            self.install_control_inputs()
            changes = [
                ".forgejo/workflows/build-ci-builder.yml",
                "os/tests/test_builder_candidate_workflow.py",
                "os/tests/release_pipeline_invariants.sh",
                "os/tests/workflow_dependency_invariants.sh",
                "tools/ci/builder_workflow_plan.py",
                "tools/ci/resolve_promoted_builder.py",
                "tools/tests/test_builder_workflow_plan.py",
                "tools/tests/test_cargo_download_cache_action.py",
                "tools/tests/test_cosign_install_action.py",
                "tools/tests/test_resolve_promoted_builder.py",
            ]
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertFalse(plan["rust_required"])
            self.assertEqual(plan["selected"], [])
            self.assertEqual(plan["matrix"], {"include": []})
        with self.subTest(phase='ci control plane mapping stays exact and conservative'):
            self.setUp()
            self.install_control_inputs()
            source = ".forgejo/workflows/check.yml"
            for extra in (".forgejo/workflows/unknown.yml", "leaf/src/lib.rs",
                          "Cargo.lock", ".cargo/config.toml"):
                with self.subTest(extra=extra):
                    plan = ci_select.make_plan(self.metadata, self.root, [source, extra], 2)
                    self.assertTrue(plan["rust_required"])
            for mapped_source, observers in ci_select.QUALITY_ONLY_CONTROL_INPUTS.items():
                for missing in (mapped_source, *observers):
                    with self.subTest(source=mapped_source, missing=missing):
                        self.install_control_inputs()
                        (self.root / missing).unlink()
                        plan = ci_select.make_plan(
                            self.metadata, self.root, [mapped_source], 2)
                        self.assertTrue(plan["rust_required"])
        with self.subTest(phase='rpm fingerprint helpers run mandatory policy and security checks'):
            self.setUp()
            self.install_control_inputs()
            source = "os/dev/rpm-snapshot-fingerprint.py"
            observer = "os/tests/test_rpm_snapshot_fingerprint.py"
            plan = ci_select.make_plan(self.metadata, self.root, [source], 2)
            self.assertFalse(plan["rust_required"])
            self.assertEqual(plan["selected"], [])
            self.assertTrue(plan["security_required"])
            (self.root / observer).unlink()
            self.assertTrue(ci_select.make_plan(self.metadata, self.root, [source], 2)["rust_required"])
            self.install_control_inputs()
            self.assertTrue(ci_select.make_plan(
                self.metadata, self.root, [source, "leaf/src/lib.rs"], 2)["rust_required"])

    def test_os_image_request_wrappers_keep_policy_and_real_build_coverage(self):
        self.install_quality_helpers()
        self.install_control_inputs()
        wrappers = [".forgejo/workflows/os-dev.yml",
                    ".forgejo/workflows/os-dev-x86.yml",
                    ".forgejo/workflows/os-dev-arm.yml"]
        for source in wrappers:
            with self.subTest(source=source):
                plan = ci_select.make_plan(self.metadata, self.root, [source], 2)
                self.assertTrue(plan["security_required"])
                self.assertFalse(plan["rust_required"])
                self.assertEqual(plan["selected"], [])
                self.assertEqual(plan["matrix"], {"include": []})
                for build_input in ("leaf/src/lib.rs", ".forgejo/workflows/build-image.yml",
                                    "Cargo.lock"):
                    mixed = ci_select.make_plan(
                        self.metadata, self.root, [source, build_input], 2)
                    self.assertTrue(mixed["rust_required"], build_input)
                full = ci_select.make_plan(
                    self.metadata, self.root, [source], 2, force_full=True)
                self.assertTrue(full["full"])
                self.assertEqual(full["selected_count"], full["workspace_count"])
                self.assertEqual({row["arch"] for row in full["matrix"]["include"]},
                                 set(ci_select.NATIVE_RUNNERS))
        observer = self.root / "tools/tests/test_os_dev_arches.py"
        observer.unlink()
        for source in wrappers:
            self.assertTrue(ci_select.make_plan(
                self.metadata, self.root, [source], 2)["rust_required"], source)

    def install_quality_helpers(self) -> None:
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for path in (source, test):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("# fixture for path existence\n")

    def install_control_inputs(self) -> None:
        for source, observers in ci_select.QUALITY_ONLY_CONTROL_INPUTS.items():
            for path in (source, *observers):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("# fixture for path existence\n")

    def test_external_asset_manifest_is_builder_only_only_for_trusted_mold_rows(self):
        with self.subTest(phase='builder only asset rows do not compile unrelated rust'):
            self.setUp()
            subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
            manifest = self.root / "os/external-assets/github-assets.tsv"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                "a" * 64 + "\tpdfium.tgz\thttps://example.invalid/pdfium.tgz\n"
            )
            subprocess.run(["git", "add", str(manifest.relative_to(self.root))], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "base manifest"], cwd=self.root, check=True)
            base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            manifest.write_text(
                manifest.read_text()
                + "# Builder-only linker archives.\n"
                + "b" * 64
                + "\tmold-2.40.4-x86_64-linux.tar.gz"
                + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
                + "mold-2.40.4-x86_64-linux.tar.gz\n"
                + "c" * 64
                + "\tmold-2.40.4-aarch64-linux.tar.gz"
                + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
                + "mold-2.40.4-aarch64-linux.tar.gz\n"
            )
            subprocess.run(["git", "commit", "-qam", "add mold assets"], cwd=self.root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            self.assertTrue(ci_select.builder_only_asset_manifest_change(self.root, base, head))
            self.install_quality_helpers()
            self.install_control_inputs()
            recipe = self.root / "os/Containerfile.builder"
            recipe.parent.mkdir(parents=True, exist_ok=True)
            recipe.write_text("FROM scratch\n")
            changes = [
                ".forgejo/workflows/build-ci-builder.yml",
                "os/Containerfile.builder",
                "os/external-assets/github-assets.tsv",
                "os/tests/test_builder_candidate_workflow.py",
                "tools/ci/verify-native-mold.sh",
                "tools/tests/test_ci_native_mold.py",
            ]
            plan = ci_select.make_plan(
                self.metadata, self.root, changes, 2, builder_only_external_assets=True
            )
            self.assertFalse(plan["rust_required"])
            self.assertFalse(plan["full"])
            self.assertEqual(plan["matrix"], {"include": []})
        with self.subTest(phase='shared or mixed asset changes force full workspace'):
            self.setUp()
            subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
            manifest = self.root / "os/external-assets/github-assets.tsv"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                "a" * 64 + "\tpdfium-old.tgz\thttps://example.invalid/pdfium-old.tgz\n"
            )
            subprocess.run(["git", "add", str(manifest.relative_to(self.root))], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "base manifest"], cwd=self.root, check=True)
            base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
            manifest.write_text(
                manifest.read_text()
                + "b" * 64
                + "\tmold-2.40.4-x86_64-linux.tar.gz"
                + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
                + "mold-2.40.4-x86_64-linux.tar.gz\n"
                + "c" * 64 + "\tpdfium-new.tgz\thttps://example.invalid/pdfium-new.tgz\n"
            )
            subprocess.run(["git", "commit", "-qam", "mixed assets"], cwd=self.root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            self.assertFalse(ci_select.builder_only_asset_manifest_change(self.root, base, head))
            plan = ci_select.make_plan(
                self.metadata, self.root, ["os/external-assets/github-assets.tsv"], 2
            )
            self.assertTrue(plan["rust_required"])
            self.assertTrue(plan["full"])
            self.assertEqual(plan["selected_count"], plan["workspace_count"])

            manifest.write_text(
                "a" * 64 + "\tpdfium-old.tgz\thttps://example.invalid/pdfium-old.tgz\n"
                + "d" * 64
                + "\tmold-2.40.4-aarch64-linux.tar.gz"
                + "\thttps://example.invalid/mold-2.40.4-aarch64-linux.tar.gz\n"
            )
            subprocess.run(["git", "commit", "-qam", "untrusted mold source"], cwd=self.root, check=True)
            untrusted_head = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
            ).strip()
            self.assertFalse(
                ci_select.builder_only_asset_manifest_change(self.root, base, untrusted_head)
            )

    def test_chunkah_release_rows_compile_no_workspace_rust(self):
        """Only os/Containerfile.rechunk reads chunkah's tarballs."""
        subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        manifest = self.root / "os/external-assets/github-assets.tsv"
        manifest.parent.mkdir(parents=True)
        manifest.write_text("a" * 64 + "\tpdfium.tgz\thttps://example.invalid/pdfium.tgz\n")
        subprocess.run(["git", "add", str(manifest.relative_to(self.root))], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "base manifest"], cwd=self.root, check=True)
        base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        def commit(rows: str, message: str) -> str:
            manifest.write_text("a" * 64 + "\tpdfium.tgz\thttps://example.invalid/pdfium.tgz\n" + rows)
            subprocess.run(["git", "commit", "-qam", message], cwd=self.root, check=True)
            return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        release = "https://github.com/coreos/chunkah/releases/download/v0.7.1/"
        trusted = commit(
            "b" * 64 + f"\tchunkah-0.7.1.tar.gz\t{release}chunkah-0.7.1.tar.gz\n"
            + "c" * 64 + f"\tchunkah-0.7.1-vendor.tar.gz\t{release}chunkah-0.7.1-vendor.tar.gz\n",
            "add chunkah")
        self.assertTrue(ci_select.builder_only_asset_manifest_change(self.root, base, trusted))
        elsewhere = commit(
            "b" * 64 + "\tchunkah-0.7.1.tar.gz\thttps://example.invalid/chunkah-0.7.1.tar.gz\n",
            "chunkah from elsewhere")
        self.assertFalse(ci_select.builder_only_asset_manifest_change(self.root, base, elsewhere))
        mixed = commit(
            "b" * 64 + f"\tchunkah-0.7.1.tar.gz\t{release}chunkah-0.7.1.tar.gz\n"
            + "d" * 64 + "\tpdfium-new.tgz\thttps://example.invalid/pdfium-new.tgz\n",
            "chunkah and a shared asset")
        self.assertFalse(ci_select.builder_only_asset_manifest_change(self.root, base, mixed))

    def test_changed_files_follow_the_true_merge_base_and_renames(self):
        with self.subTest(phase='changed files fails when merge base is unavailable'):
            self.setUp()
            subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
            (self.root / "base.rs").write_text("fn base() {}\n")
            subprocess.run(["git", "add", "base.rs"], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=self.root, check=True)
            base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            subprocess.run(["git", "checkout", "--orphan", "shallow-head", "-q"], cwd=self.root, check=True)
            subprocess.run(["git", "rm", "-qf", "base.rs"], cwd=self.root, check=True)
            (self.root / "head.rs").write_text("fn head() {}\n")
            subprocess.run(["git", "add", "head.rs"], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "head"], cwd=self.root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            with self.assertRaisesRegex(RuntimeError, "merge base"):
                ci_select.changed_files(self.root, base, head)
        with self.subTest(phase='changed files excludes changes only on advanced base'):
            self.setUp()
            subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
            (self.root / "shared.txt").write_text("common\n")
            subprocess.run(["git", "add", "shared.txt"], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "common"], cwd=self.root, check=True)

            subprocess.run(["git", "switch", "-qc", "pull-request"], cwd=self.root, check=True)
            (self.root / "head-only.py").write_text("print('head')\n")
            subprocess.run(["git", "add", "head-only.py"], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "pull request"], cwd=self.root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

            subprocess.run(["git", "switch", "-q", "base"], cwd=self.root, check=True)
            (self.root / "base-only.rs").write_text("fn landed_on_main() {}\n")
            subprocess.run(["git", "add", "base-only.rs"], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "advance base"], cwd=self.root, check=True)
            advanced_base = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
            ).strip()

            self.assertEqual(ci_select.changed_files(self.root, advanced_base, head), ["head-only.py"])
        with self.subTest(phase='move between packages selects both owners and dependants'):
            self.setUp()
            subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
            subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
            # Identical content encourages Git's normal rename detection. Unicode
            # also exercises its default quoted-path output rather than ASCII only.
            old = "base/src/café.rs"
            new = "leaf/src/café.rs"
            (self.root / old).parent.mkdir(parents=True)
            (self.root / new).parent.mkdir(parents=True)
            (self.root / old).write_text("pub fn moved() {}\n")
            subprocess.run(["git", "add", old], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "before move"], cwd=self.root, check=True)
            base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
            subprocess.run(["git", "mv", old, new], cwd=self.root, check=True)
            subprocess.run(["git", "commit", "-qm", "move between packages"], cwd=self.root, check=True)
            changes = ci_select.changed_files(self.root, base, "HEAD")
            self.assertEqual(set(changes), {old, new})
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertTrue({"base", "middle", "leaf"}.issubset(plan["selected"]))


REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
LOCAL_PACKAGES = ("base", "middle", "leaf", "tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types")


def lockfile(external: dict[tuple[str, str], list[str]], local: dict[str, list[str]],
             header: str = "version = 4\n") -> bytes:
    """Render a Cargo.lock with registry packages and source-less workspace nodes."""
    blocks = [header]
    for (name, version), deps in sorted(external.items()):
        blocks.append(
            f'[[package]]\nname = "{name}"\nversion = "{version}"\nsource = "{REGISTRY}"\n'
            f'checksum = "{name}-{version}"\ndependencies = {deps!r}\n'.replace("'", '"'))
    for name, deps in sorted(local.items()):
        blocks.append(
            f'[[package]]\nname = "{name}"\nversion = "0.1.0"\ndependencies = {deps!r}\n'
            .replace("'", '"'))
    return "\n".join(blocks).encode()


ROOT_MANIFEST = """\
[workspace]
resolver = "3"
members = ["base", "middle", "leaf", "tilo-doc", "tilo-doc-codecs", "tilo-app-package", "collab-types"]

[workspace.package]
edition = "2024"

[workspace.lints.clippy]
pedantic = "deny"

[workspace.dependencies]
itoa = "1.0"
serde = { version = "1", features = ["derive"] }
json = { package = "serde_json", version = "1" }

[profile.release]
lto = "thin"

[patch.crates-io]
vendored-sys = { path = "vendor/vendored-sys" }
"""


class DependencyKindTests(unittest.TestCase):
    """Normal and build edges propagate a change; a dev edge stops after one hop.

    A dev-dependency enters only the dependant's own test, example and bench
    targets. The library the dependant's own dependants link is unchanged, so
    linting them cannot find anything the change caused.
    """

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        # The contract packages every plan adds, kept apart from the graph.
        self.contracts = sorted(ci_select.ALWAYS_PACKAGES)

    def metadata(self, graph: dict[str, list[tuple[str, str | None]]],
                 wasm: frozenset[str] = frozenset()) -> dict:
        """`graph`: package -> [(dependency, kind)] with Cargo's kinds (None, "build", "dev")."""
        names = set(graph) | set(self.contracts)
        packages = []
        for name in sorted(names):
            dependencies = [{"name": dep, "kind": kind, "path": str(self.root / dep)}
                            for dep, kind in graph.get(name, [])]
            if name in wasm:
                dependencies.append({"name": "wasm-bindgen", "kind": None})
            packages.append({"name": name, "manifest_path": str(self.root / name / "Cargo.toml"),
                             "targets": [{"name": name}], "dependencies": dependencies})
        return {"packages": packages}

    def selected(self, metadata: dict, changed: str) -> set[str]:
        plan = ci_select.make_plan(metadata, self.root, [f"{changed}/src/lib.rs"], 2)
        selected = set(plan["selected"])
        self.assertLessEqual(set(self.contracts), selected)
        # What Clippy actually receives is the selection, nothing more.
        linted = {name for row in plan["clippy_matrix"]["include"]
                  for name in row["packages"].split()[1::2]}
        self.assertEqual(linted, selected)
        return selected - set(self.contracts)

    def test_a_normal_chain_is_followed_to_its_end(self):
        metadata = self.metadata({"a": [], "b": [("a", None)], "c": [("b", None)], "d": [("c", None)]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b", "c", "d"})
        self.assertEqual(self.selected(metadata, "c"), {"c", "d"})

    def test_a_dev_edge_selects_the_dev_dependant_and_stops(self):
        # b's tests use a; c's tests use b's library, which a does not reach.
        metadata = self.metadata({"a": [], "b": [("a", "dev")], "c": [("b", "dev")]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b"})
        plan = ci_select.make_plan(metadata, self.root, ["a/src/lib.rs"], 2)
        self.assertIn("b", plan["tested"], "a dev dependant's own tests changed and still run")

    def test_a_dev_edge_then_a_normal_edge_stops_at_the_dev_dependant(self):
        metadata = self.metadata({"a": [], "b": [("a", "dev")], "c": [("b", None)],
                                  "d": [("c", "build")]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b"})
        # The library b's dependants link is what a change to b itself reaches.
        self.assertEqual(self.selected(metadata, "b"), {"b", "c", "d"})

    def test_a_normal_edge_then_a_dev_edge_adds_one_dev_hop_from_anywhere_in_the_closure(self):
        metadata = self.metadata({"a": [], "b": [("a", None)], "c": [("b", "dev")],
                                  "d": [("c", None)], "e": [("c", "dev")]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b", "c"})

    def test_a_build_dependency_propagates_like_a_normal_one(self):
        # A build script can generate the library's code, so b's library changes.
        metadata = self.metadata({"a": [], "b": [("a", "build")], "c": [("b", None)],
                                  "d": [("c", "dev")], "e": [("d", None)]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b", "c", "d"})

    def test_a_dependency_declared_both_normal_and_dev_propagates(self):
        # Cargo lists a crate twice when it is, e.g., a target-specific normal
        # dependency and an unconditional dev-dependency.
        metadata = self.metadata({"a": [], "b": [("a", "dev"), ("a", None)], "c": [("b", None)]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b", "c"})

    def test_an_unknown_dependency_kind_propagates(self):
        metadata = self.metadata({"a": [], "b": [("a", "artifact")], "c": [("b", None)]})
        self.assertEqual(self.selected(metadata, "a"), {"a", "b", "c"})

    def test_a_test_support_cycle_selects_both_ends_without_the_dev_side_dependants(self):
        # The common test-helper shape: support links core; core's tests use support.
        metadata = self.metadata({"core": [], "support": [("core", None)],
                                  "core-user": [("core", None)], "support-user": [("support", None)],
                                  "core-dev-user": [("core", "dev")]})
        self.assertEqual(self.selected(metadata, "core"),
                         {"core", "support", "core-user", "support-user", "core-dev-user"})
        cyclic = self.metadata({"core": [("support", "dev")], "support": [("core", None)],
                                "core-user": [("core", None)], "support-user": [("support", None)]})
        self.assertEqual(self.selected(cyclic, "support"), {"support", "core", "support-user"})
        self.assertEqual(self.selected(cyclic, "core"), {"core", "support", "core-user", "support-user"})

    def test_the_selection_never_drops_a_direct_dependant_and_never_grows(self):
        graph = {"a": [], "b": [("a", "dev")], "c": [("a", None)], "d": [("b", None)],
                 "e": [("c", "dev")], "f": [("e", None), ("d", "build")], "g": [("f", "dev")]}
        metadata = self.metadata(graph)
        packages, _ = ci_select.package_maps(metadata, self.root)
        reverse = ci_select.reverse_dependencies(packages, self.root)
        edges = ci_select.dependency_edges(packages)
        self.assertEqual({name: edges.linked[name] | edges.dev[name] for name in packages}, reverse)
        for name in graph:
            selected = self.selected(metadata, name)
            self.assertLessEqual({name} | reverse[name], selected, name)
            self.assertLessEqual(selected, ci_select.closure({name}, reverse), name)

    def test_the_owner_commit_vocabulary_no_longer_lints_relay_or_the_wasm_crates(self):
        # The workspace shape around tilo-owner-commit when it moved out of
        # tilo-peer-state. Every edge kind is the one Cargo metadata reported.
        graph = {
            "tilo-owner-commit": [],
            "tilo-action-broker": [("tilo-owner-commit", None)],
            "tilo-joined-room": [("tilo-owner-commit", None)],
            "shell": [("tilo-owner-commit", None)],
            "tilo-runtime": [("tilo-owner-commit", None)],
            "agent": [("shell", "dev")],
            "tilo-browser-peer": [("tilo-joined-room", "dev")],
            "tilo-relay": [("tilo-browser-peer", None)],
            "tilo-agent-participant": [("tilo-browser-peer", None), ("agent", None)],
            "tilo-room-load": [("tilo-browser-peer", None)],
            "tilo-browser-runtime": [("tilo-browser-peer", None)],
            "tilo-web-desktop": [("tilo-browser-runtime", None)],
            "tilo-relay-shell": [("tilo-relay", "dev")],
            "discord-worker": [("tilo-relay", "dev")],
        }
        metadata = self.metadata(graph, wasm=frozenset({"tilo-browser-runtime", "tilo-web-desktop"}))
        packages, _ = ci_select.package_maps(metadata, self.root)
        every_edge = ci_select.closure({"tilo-owner-commit"}, ci_select.reverse_dependencies(packages, self.root))
        self.assertEqual(len(every_edge | set(self.contracts)), 18, "the selection before dev edges stopped")
        plan = ci_select.make_plan(metadata, self.root, ["tilo-owner-commit/src/lib.rs"], 2)
        self.assertEqual(plan["selected_count"], 11)
        self.assertEqual(set(plan["selected"]) - set(self.contracts), {
            "tilo-owner-commit", "tilo-action-broker", "tilo-joined-room", "shell",
            "tilo-runtime", "agent", "tilo-browser-peer"})
        self.assertEqual(plan["wasm"], [])
        self.assertFalse(plan["wasm_required"])
        # Every direct dependant, the dev one included, is still tested.
        self.assertLessEqual({"tilo-action-broker", "tilo-joined-room", "shell", "tilo-runtime"},
                             set(plan["tested"]))
        # A change to browser-peer itself still reaches relay and the wasm crates.
        plan = ci_select.make_plan(metadata, self.root, ["tilo-browser-peer/src/lib.rs"], 2)
        self.assertLessEqual({"tilo-relay", "tilo-relay-shell", "discord-worker", "tilo-web-desktop"},
                             set(plan["selected"]))
        self.assertEqual(plan["wasm"], ["tilo-browser-runtime", "tilo-web-desktop"])


class ManifestAttributionTests(unittest.TestCase):
    """Root Cargo.toml and Cargo.lock changes select only what they reach."""

    def setUp(self) -> None:
        # Merged tests call setUp() again at the start of every phase so each
        # phase gets a fresh temporary workspace and freshly built metadata.
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(ROOT_MANIFEST)
        self.metadata = {"packages": []}
        for name, deps in [("base", [("serde", None)]), ("middle", [("base", "path"), ("itoa", None)]),
                           ("leaf", [("middle", "path"), ("serde_json", "json")]),
                           ("tilo-doc", []), ("tilo-doc-codecs", [("tilo-doc", "path")]), ("tilo-app-package", []), ("collab-types", [])]:
            self.add_package(name, deps)
        self.external = {
            ("serde", "1.0.0"): [],
            ("itoa", "1.0.0"): [],
            ("serde_json", "1.0.0"): ["itoa", "serde"],
            ("vendored-sys", "0.9.6"): ["cc"],
            ("cc", "1.2.0"): [],
        }
        self.local = {
            "base": ["serde"], "middle": ["base", "itoa"], "leaf": ["middle", "serde_json"],
            "tilo-doc": [], "tilo-doc-codecs": ["tilo-doc"], "tilo-app-package": [], "collab-types": [],
        }

    def add_package(self, name: str, deps: list[tuple[str, str | None]]) -> None:
        dependencies = []
        for dep, kind in deps:
            entry = {"name": dep, "rename": None}
            if kind == "path":
                entry["path"] = str(self.root / dep)
            elif kind:
                entry["rename"] = kind
            dependencies.append(entry)
        self.metadata["packages"].append({
            "name": name,
            "manifest_path": str(self.root / name / "Cargo.toml"),
            "targets": [{"name": name}],
            "dependencies": dependencies,
        })

    def lock(self, external=None, local=None, header="version = 4\n") -> bytes:
        return lockfile(self.external if external is None else external,
                        self.local if local is None else local, header)

    def plan(self, changes: list[str], resolved: dict) -> dict:
        return ci_select.make_plan(self.metadata, self.root, changes, 2, resolved_inputs=resolved)

    def test_lockfile_changes_select_exactly_the_reached_workspace_crates(self):
        with self.subTest(phase='adding a workspace crate selects only the new crate'):
            self.setUp()
            self.add_package("durable", [("serde", None), ("thiserror", None)])
            before = self.lock()
            after = self.lock(external={**self.external, ("thiserror", "2.0.0"): []},
                              local={**self.local, "durable": ["serde", "thiserror"]})
            manifest = ROOT_MANIFEST.replace('"collab-types"]', '"collab-types", "durable"]')
            resolved = {
                "Cargo.lock": ci_select.resolve_lockfile(before, after, self.metadata),
                "Cargo.toml": ci_select.resolve_root_manifest(
                    ROOT_MANIFEST.encode(), manifest.encode(), self.metadata, self.root),
            }
            self.assertEqual(resolved["Cargo.lock"].seeds, {"durable"})
            self.assertEqual(resolved["Cargo.toml"].seeds, {"durable"})
            plan = self.plan(["Cargo.lock", "Cargo.toml", "durable/Cargo.toml", "durable/src/lib.rs"], resolved)
            self.assertFalse(plan["full"])
            self.assertEqual(set(plan["selected"]), {"durable"} | ci_select.ALWAYS_PACKAGES)
            self.assertEqual(set(plan["tested"]), {"durable"} | ci_select.ALWAYS_PACKAGES)
        with self.subTest(phase='leaf dependency bump selects its consumers and their dependants'):
            self.setUp()
            after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "itoa"},
                                        ("itoa", "1.0.1"): []})
            resolved = {"Cargo.lock": ci_select.resolve_lockfile(self.lock(), after, self.metadata)}
            # itoa is used directly by middle and through serde_json by leaf.
            self.assertEqual(resolved["Cargo.lock"].seeds, {"middle", "leaf"})
            plan = self.plan(["Cargo.lock"], resolved)
            self.assertFalse(plan["full"])
            self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
            self.assertEqual(set(plan["tested"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
            self.assertNotIn("base", plan["selected"])
        with self.subTest(phase='transitive dependency change reaches workspace consumers'):
            self.setUp()
            external = dict(self.external)
            external[("serde", "1.0.0")] = ["serde_derive"]
            external[("serde_derive", "1.0.0")] = []
            before = self.lock(external=external)
            external.pop(("serde_derive", "1.0.0"))
            external[("serde_derive", "1.0.1")] = []
            resolved = ci_select.resolve_lockfile(before, self.lock(external=external), self.metadata)
            self.assertEqual(resolved.seeds, {"base", "leaf"})
            self.assertEqual(set(self.plan(["Cargo.lock"], {"Cargo.lock": resolved})["selected"]),
                             {"base", "middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
        with self.subTest(phase='second version of a crate does not touch existing consumers'):
            self.setUp()
            self.add_package("legacy", [("itoa", None)])
            local = {**self.local, "middle": ["base", "itoa 1.0.0"], "leaf": ["middle", "serde_json"]}
            external = {**self.external, ("itoa", "0.4.0"): [],
                        ("serde_json", "1.0.0"): ["itoa 1.0.0", "serde"]}
            after = self.lock(external=external, local={**local, "legacy": ["itoa 0.4.0"]})
            resolved = ci_select.resolve_lockfile(
                self.lock(local={**self.local, "legacy": []}), after, self.metadata)
            self.assertEqual(resolved.seeds, {"legacy"})
        with self.subTest(phase='removing a dependency selects its former consumers'):
            self.setUp()
            external = {k: v for k, v in self.external.items() if k[0] != "itoa"}
            external[("serde_json", "1.0.0")] = ["serde"]
            after = self.lock(external=external, local={**self.local, "middle": ["base"]})
            resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
            self.assertEqual(resolved.seeds, {"middle", "leaf"})
        with self.subTest(phase='lockfile change reaching no workspace crate selects only contract suites'):
            self.setUp()
            after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "cc"},
                                        ("cc", "1.2.1"): []})
            resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
            self.assertEqual(resolved.seeds, set())
            plan = self.plan(["Cargo.lock"], {"Cargo.lock": resolved})
            self.assertFalse(plan["full"])

    def test_root_manifest_changes_attribute_to_members_or_stay_full(self):
        with self.subTest(phase='removing a workspace member selects its dependants'):
            self.setUp()
            manifest = ROOT_MANIFEST.replace('"base", "middle"', '"middle"')
            self.metadata["packages"] = [p for p in self.metadata["packages"] if p["name"] != "base"]
            resolved = ci_select.resolve_root_manifest(
                ROOT_MANIFEST.encode(), manifest.encode(), self.metadata, self.root)
            self.assertEqual(resolved.seeds, {"middle"})
            plan = self.plan(["Cargo.toml"], {"Cargo.toml": resolved})
            self.assertFalse(plan["full"])
            self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
        with self.subTest(phase='workspace dependency change selects members that use it'):
            self.setUp()
            for old, new, expected in [
                ('itoa = "1.0"', 'itoa = "1.0.9"', {"middle"}),
                ('features = ["derive"]', 'features = ["derive", "rc"]', {"base"}),
                # A renamed entry is matched through both its key and package.
                ('json = { package = "serde_json", version = "1" }',
                 'json = { package = "serde_json", version = "1.0.100" }', {"leaf"}),
                ('itoa = "1.0"\n', 'itoa = "1.0"\nryu = "1"\n', set()),
            ]:
                with self.subTest(new=new):
                    resolved = ci_select.resolve_root_manifest(
                        ROOT_MANIFEST.encode(), ROOT_MANIFEST.replace(old, new).encode(),
                        self.metadata, self.root)
                    self.assertIsNotNone(resolved)
                    self.assertEqual(resolved.seeds, expected)
        with self.subTest(phase='unattributable root manifest changes keep full selection'):
            self.setUp()
            for old, new in [
                ('pedantic = "deny"', 'pedantic = "warn"'),
                ('lto = "thin"', 'lto = "fat"'),
                ('edition = "2024"', 'edition = "2024"\nrust-version = "1.90"'),
                ('vendored-sys = { path = "vendor/vendored-sys" }', ''),
                ('resolver = "3"', 'resolver = "2"'),
                ('members = [', 'exclude = ["fuzz"]\nmembers = ['),
                ('"collab-types"]', '"collab-types", "crates/*"]'),
                ('"collab-types"]', '"collab-types", "unknown"]'),
                ('[workspace]', '[workspace]\n[workspace.metadata.tool]\nx = 1\n[workspace.x]'),
                ('[workspace]', '[package]\nname = "root"\n[workspace]'),
                ('[workspace]', '[workspace'),
            ]:
                with self.subTest(new=new):
                    resolved = ci_select.resolve_root_manifest(
                        ROOT_MANIFEST.encode(), ROOT_MANIFEST.replace(old, new).encode(),
                        self.metadata, self.root)
                    self.assertIsNone(resolved)
                    plan = self.plan(["Cargo.toml"], {})
                    self.assertTrue(plan["full"])
                    self.assertEqual(plan["selected_count"], plan["workspace_count"])
            self.assertIsNone(ci_select.resolve_root_manifest(
                None, ROOT_MANIFEST.encode(), self.metadata, self.root))

    def test_attribution_fails_safe_and_is_fed_from_git_revisions(self):
        with self.subTest(phase='malformed or unprovable lockfiles keep full selection'):
            self.setUp()
            good = self.lock()
            ambiguous = self.lock(external={**self.external, ("itoa", "0.4.0"): []})
            for before, after in [
                (good, b"[[package]\nname = "),
                (b"\xff\xfe", good),
                (None, good),
                (good, None),
                (good, self.lock(header="version = 3\n")),
                # Two itoa records make the unqualified "itoa" references ambiguous.
                (good, ambiguous),
                (good, self.lock(local={**self.local, "middle": ["base", "missing"]})),
                (good, good.replace(b'name = "cc"', b'name_typo = "cc"')),
                # Attribution requires every workspace member to be a lockfile node.
                (good, self.lock(local={k: v for k, v in self.local.items() if k != "tilo-doc"})),
            ]:
                with self.subTest(after=after and after[:40]):
                    self.assertIsNone(ci_select.resolve_lockfile(before, after, self.metadata))
            plan = self.plan(["Cargo.lock"], {})
            self.assertTrue(plan["full"])
            self.assertIn("repository-wide", plan["reason"])
        with self.subTest(phase='attribution never hides other global inputs or explicit full'):
            self.setUp()
            after = self.lock(external={**self.external, ("unused", "1.0.0"): []})
            resolved = {"Cargo.lock": ci_select.resolve_lockfile(self.lock(), after, self.metadata)}
            for changes in (["Cargo.lock", "rust-toolchain.toml"], ["Cargo.lock", ".cargo/config.toml"],
                            ["Cargo.lock", "Cargo.toml"]):
                with self.subTest(changes=changes):
                    plan = self.plan(changes, resolved)
                    self.assertTrue(plan["full"])
            full = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2,
                                       force_full=True, resolved_inputs=resolved)
            self.assertTrue(full["full"])
            self.assertEqual(full["qualification"], "full-native")
            # A resolution for a path that did not change is ignored.
            self.assertEqual(self.plan(["leaf/src/lib.rs"], resolved)["tested"],
                             sorted({"leaf"} | ci_select.ALWAYS_PACKAGES))
        with self.subTest(phase='git revisions feed manifest attribution'):
            self.setUp()
            def git(*args: str) -> str:
                return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()
            git("init", "-q", "-b", "main")
            git("config", "user.email", "ci@example.invalid")
            git("config", "user.name", "CI")
            (self.root / "Cargo.lock").write_bytes(self.lock())
            git("add", "Cargo.toml", "Cargo.lock")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "itoa"},
                                        ("itoa", "1.0.1"): []})
            (self.root / "Cargo.lock").write_bytes(after)
            (self.root / "Cargo.toml").write_text(ROOT_MANIFEST.replace('itoa = "1.0"', 'itoa = "1.0.1"'))
            git("commit", "-qam", "bump itoa")
            changes = ci_select.changed_files(self.root, base, "HEAD")
            resolved = ci_select.resolve_manifest_inputs(self.root, base, "HEAD", self.metadata, changes)
            self.assertEqual(set(resolved), {"Cargo.lock", "Cargo.toml"})
            plan = self.plan(changes, resolved)
            self.assertFalse(plan["full"])
            self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
            self.assertIn("Cargo.lock", plan["reason"])
            # An unreadable head lockfile is unattributable, so it stays global.
            (self.root / "Cargo.lock").write_bytes(b"not toml [")
            git("commit", "-qam", "corrupt")
            changes = ci_select.changed_files(self.root, base, "HEAD")
            resolved = ci_select.resolve_manifest_inputs(self.root, base, "HEAD", self.metadata, changes)
            self.assertNotIn("Cargo.lock", resolved)
            self.assertTrue(self.plan(changes, resolved)["full"])


GIT = "git+https://example.invalid/fork.git?rev=main"


class LockfileDependencyKindTests(unittest.TestCase):
    """A lock-only change follows each member's dependency kinds, as path edges do.

    The lockfile records no kinds, but Cargo locks only the members'
    dev-dependencies, so a third-party edge is always normal or build and a
    member's edge takes its kind from the member's manifest entries.

    Workspace: `core` uses itoa (normal), cc (build) and proptest (dev); `app`
    links core by path and uses rustls and the patched member `native-certs`
    (no path entry, like a `[patch.crates-io]` crate); `plugin` links app;
    `tool` dev-depends on core and uses clap; `native-certs` uses
    rustls-pki-types. Every member uses the hakari package, which names all
    the shared third-party crates.
    """

    MEMBERS = {
        "core": [("itoa", None), ("cc", "build"), ("proptest", "dev")],
        "app": [("core", "path"), ("rustls", None), ("native-certs", None), ("tempfile", "dev")],
        "plugin": [("app", "path")],
        "tool": [("core", "dev-path"), ("clap", None)],
        "native-certs": [("rustls-pki-types", None)],
        ci_select.HAKARI_PACKAGE: [("itoa", None), ("rustls", None), ("clap", None),
                                   ("rustls-pki-types", None), ("cc", "build")],
    }

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        self.metadata = self.describe(self.MEMBERS)
        self.external = {
            ("itoa", "1.0.0", REGISTRY): [],
            ("cc", "1.2.0", REGISTRY): [],
            ("proptest", "1.5.0", REGISTRY): ["tempfile"],
            ("tempfile", "3.10.0", REGISTRY): [],
            ("rustls", "0.23.0", REGISTRY): ["rustls-pki-types"],
            ("rustls-pki-types", "1.0.0", REGISTRY): [],
            ("clap", "4.5.0", REGISTRY): [],
            ("hyper-rustls", "0.27.0", REGISTRY): ["native-certs", "rustls"],
        }
        hack = ci_select.HAKARI_PACKAGE
        self.local = {
            "core": ["itoa", "cc", "proptest", hack],
            "app": ["core", "rustls", "native-certs", "tempfile", hack],
            "plugin": ["app", hack],
            "tool": ["core", "clap", hack],
            "native-certs": ["rustls-pki-types", hack],
            hack: ["itoa", "rustls", "clap", "rustls-pki-types", "cc"],
            **{name: [hack] for name in ci_select.ALWAYS_PACKAGES},
        }

    def describe(self, members: dict[str, list[tuple[str, str | None]]]) -> dict:
        """`cargo metadata --no-deps` for `members` plus the contract packages."""
        packages = []
        for name in sorted(set(members) | ci_select.ALWAYS_PACKAGES):
            dependencies = []
            for dep, kind in members.get(name, []):
                entry = {"name": dep, "rename": None,
                         "kind": {"path": None, "dev-path": "dev"}.get(kind, kind)}
                if kind in ("path", "dev-path"):
                    entry["path"] = str(self.root / dep)
                else:
                    entry["source"] = REGISTRY
                dependencies.append(entry)
            if name != ci_select.HAKARI_PACKAGE:
                dependencies.append({"name": ci_select.HAKARI_PACKAGE, "kind": None, "source": REGISTRY})
            packages.append({"name": name, "manifest_path": str(self.root / name / "Cargo.toml"),
                             "targets": [{"name": name}], "dependencies": dependencies})
        return {"packages": packages}

    def lock(self, external=None, local=None, extra: str = "") -> bytes:
        blocks = ["version = 4\n"]
        for (name, version, source), deps in sorted((self.external if external is None else external).items()):
            source_line = f'source = "{source}"\n' if source else ""
            blocks.append(f'[[package]]\nname = "{name}"\nversion = "{version}"\n{source_line}'
                          f'dependencies = {json.dumps(deps)}\n')
        for name, deps in sorted((self.local if local is None else local).items()):
            blocks.append(f'[[package]]\nname = "{name}"\nversion = "0.1.0"\ndependencies = {json.dumps(deps)}\n')
        return ("\n".join(blocks) + extra).encode()

    def bump(self, name: str, version: str, deps: list[str] | None = None, source: str = REGISTRY) -> dict:
        external = {key: value for key, value in self.external.items() if key[0] != name}
        old = next(value for key, value in self.external.items() if key[0] == name)
        external[(name, version, source)] = old if deps is None else deps
        return external

    def kind_blind(self, before: bytes, after: bytes) -> tuple[frozenset[str], frozenset[str]]:
        packages = {package["name"] for package in self.metadata["packages"]}
        change = ci_select.lockfile_change(before, after, packages)
        return change.seeds, frozenset(change.reached & packages)

    def plan(self, after: bytes) -> tuple[ci_select.ResolvedInput, dict]:
        resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
        self.assertIsNotNone(resolved)
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2,
                                   resolved_inputs={"Cargo.lock": resolved})
        self.assertFalse(plan["full"])
        linted = {name for row in plan["clippy_matrix"]["include"] for name in row["packages"].split()[1::2]}
        self.assertEqual(linted, set(plan["selected"]))
        return resolved, plan

    def assert_kind_blind(self, after: bytes) -> None:
        """Unprovable by kind: exactly the kind-blind lockfile attribution, never less."""
        self.assertIsNone(ci_select.lockfile_change_by_kind(self.lock(), after, self.metadata))
        resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
        self.assertEqual((resolved.seeds, resolved.checked), self.kind_blind(self.lock(), after))
        self.assertEqual(resolved.dev_only, frozenset())

    def test_a_normal_third_party_dependency_reaches_its_users_and_their_dependants(self):
        resolved, plan = self.plan(self.lock(external=self.bump("itoa", "1.0.1")))
        self.assertEqual(resolved.seeds, {"core", ci_select.HAKARI_PACKAGE})
        self.assertEqual(resolved.dev_only, frozenset())
        # app and plugin link core's library; tool's tests use it.
        self.assertEqual(set(plan["selected"]) - ci_select.ALWAYS_PACKAGES,
                         {"core", "app", "plugin", "tool", ci_select.HAKARI_PACKAGE})
        self.assertNotIn("native-certs", plan["selected"])
        self.assertLessEqual({"core", "app", "tool"}, set(plan["tested"]))

    def test_a_dev_only_third_party_dependency_selects_its_dev_dependants_and_stops(self):
        after = self.lock(external=self.bump("proptest", "1.5.1"))
        resolved, plan = self.plan(after)
        self.assertEqual(resolved.seeds, frozenset())
        self.assertEqual(resolved.dev_only, {"core"})
        self.assertEqual(set(plan["selected"]), {"core"} | ci_select.ALWAYS_PACKAGES)
        self.assertIn("core", plan["tested"], "core's own tests use the changed crate")
        # The kind-blind graph seeds core and so expands core's library closure.
        seeds, _ = self.kind_blind(self.lock(), after)
        self.assertIn("core", seeds)

    def test_a_third_party_change_reached_through_a_dev_dependency_is_dev_only(self):
        # tempfile is app's dev-dependency and proptest's (core's dev) dependency.
        resolved, plan = self.plan(self.lock(external=self.bump("tempfile", "3.10.1")))
        self.assertEqual(resolved.seeds, frozenset())
        self.assertEqual(resolved.dev_only, {"core", "app"})
        self.assertEqual(set(plan["selected"]), {"core", "app"} | ci_select.ALWAYS_PACKAGES)

    def test_a_build_dependency_propagates_like_a_normal_one(self):
        resolved, plan = self.plan(self.lock(external=self.bump("cc", "1.2.1")))
        self.assertEqual(resolved.seeds, {"core", ci_select.HAKARI_PACKAGE})
        self.assertEqual(set(plan["selected"]) - ci_select.ALWAYS_PACKAGES,
                         {"core", "app", "plugin", "tool", ci_select.HAKARI_PACKAGE})

    def test_the_hakari_package_does_not_carry_a_change_to_every_member(self):
        after = self.lock(external=self.bump("clap", "4.5.1"))
        resolved, plan = self.plan(after)
        self.assertEqual(resolved.seeds, {"tool", ci_select.HAKARI_PACKAGE})
        self.assertEqual(set(plan["selected"]) - ci_select.ALWAYS_PACKAGES,
                         {"tool", ci_select.HAKARI_PACKAGE})
        # Through the hakari package's lock edges the kind-blind graph reaches every member.
        _, reached = self.kind_blind(self.lock(), after)
        self.assertEqual(reached, {package["name"] for package in self.metadata["packages"]})

    def test_a_patched_member_without_a_path_entry_carries_the_change(self):
        resolved, plan = self.plan(self.lock(external=self.bump("rustls-pki-types", "1.0.1")))
        # rustls and native-certs use it; app uses both; plugin links app.
        self.assertEqual(resolved.seeds, {"app", "native-certs", ci_select.HAKARI_PACKAGE})
        self.assertEqual(set(plan["selected"]) - ci_select.ALWAYS_PACKAGES,
                         {"app", "plugin", "native-certs", ci_select.HAKARI_PACKAGE})

    def test_a_transitive_third_party_change_reaches_the_member_through_its_consumer(self):
        external = self.bump("rustls-pki-types", "1.0.1")
        self.local["native-certs"] = [ci_select.HAKARI_PACKAGE]
        self.metadata = self.describe({**self.MEMBERS, "native-certs": []})
        resolved, _ = self.plan(self.lock(external=external))
        # Only rustls (app's) and the hakari package name it now.
        self.assertEqual(resolved.seeds, {"app", ci_select.HAKARI_PACKAGE})

    def test_a_feature_unification_change_keeps_the_kind_blind_attribution(self):
        # clap's new release depends on itoa, which other members also build: it
        # may enable itoa features for them, which no lockfile records.
        self.assert_kind_blind(self.lock(external=self.bump("clap", "4.6.0", ["itoa"])))
        # A brand-new crate depending on an existing one is the same case.
        external = {**self.bump("clap", "4.6.0", ["clap_lex"]), ("clap_lex", "0.7.0", REGISTRY): ["itoa"]}
        self.assert_kind_blind(self.lock(external=external))
        # Dropping an edge may disable features the other members relied on.
        self.assert_kind_blind(self.lock(external=self.bump("rustls", "0.23.1", [])))
        # So may a major move, which starts a new semver-compatible line.
        self.assert_kind_blind(self.lock(external=self.bump("rustls", "1.0.0")))
        # Moving to a dependency that is new as a whole is provable.
        external = {**self.bump("clap", "4.6.0", ["clap_lex"]), ("clap_lex", "0.7.0", REGISTRY): []}
        self.assertIsNotNone(ci_select.lockfile_change_by_kind(self.lock(), self.lock(external=external),
                                                               self.metadata))

    def test_git_and_patched_sources_keep_the_kind_blind_attribution(self):
        with self.subTest("a git revision moves"):
            self.external[("accesskit", "0.17.0", f"{GIT}#aaaa")] = []
            self.local["app"] = [*self.local["app"], "accesskit"]
            self.assert_kind_blind(self.lock(external=self.bump("accesskit", "0.17.0", source=f"{GIT}#bbbb")))
        with self.subTest("a registry crate moves to a git fork"):
            self.assert_kind_blind(self.lock(external=self.bump("clap", "4.5.0", source=f"{GIT}#cccc")))
        with self.subTest("a registry crate is patched to a local path"):
            self.assert_kind_blind(self.lock(external=self.bump("clap", "4.5.0", source="")))
        with self.subTest("an unused patch is recorded"):
            after = self.lock(extra='\n[[patch.unused]]\nname = "clap"\nversion = "4.5.9"\n')
            self.assertIsNone(ci_select.resolve_lockfile(self.lock(), after, self.metadata))
        with self.subTest("a git record that only follows a registry bump is attributed"):
            self.setUp()
            self.external[("accesskit", "0.17.0", f"{GIT}#aaaa")] = ["itoa"]
            self.local["app"] = [*self.local["app"], "accesskit"]
            self.metadata = self.describe({**self.MEMBERS, "app": [*self.MEMBERS["app"], ("accesskit", None)]})
            resolved, _ = self.plan(self.lock(external=self.bump("itoa", "1.0.1")))
            self.assertEqual(resolved.seeds, {"core", "app", ci_select.HAKARI_PACKAGE})

    def test_a_member_record_the_registry_changes_do_not_explain_is_seeded(self):
        # tool's manifest gained itoa (already locked, unchanged): a manifest edit.
        local = {**self.local, "tool": [*self.local["tool"], "itoa"]}
        after = self.lock(external=self.bump("proptest", "1.5.1"), local=local)
        resolved, plan = self.plan(after)
        self.assertEqual(resolved.seeds, {"tool"})
        self.assertEqual(resolved.dev_only, {"core"})
        self.assertEqual(set(plan["selected"]) - ci_select.ALWAYS_PACKAGES, {"core", "tool"})

    def test_a_member_edge_without_a_manifest_entry_counts_as_linked(self):
        # core's lock record names clap but its manifest entries do not.
        self.local["core"] = [*self.local["core"], "clap"]
        resolved, plan = self.plan(self.lock(external=self.bump("clap", "4.5.1")))
        self.assertIn("core", resolved.seeds)
        self.assertLessEqual({"app", "plugin", "tool"}, set(plan["selected"]))


class HakariPackageTests(unittest.TestCase):
    def test_the_hakari_package_constant_names_the_configured_package(self):
        config = tomllib.loads((REPO_ROOT / ".config/hakari.toml").read_text())
        self.assertEqual(config["hakari-package"], ci_select.HAKARI_PACKAGE)


if __name__ == "__main__":
    unittest.main()
