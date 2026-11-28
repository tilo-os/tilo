"""Selection, execution and policy for the hermetic suites outside Cargo."""
from __future__ import annotations

import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_suites", ROOT / "tools/ci/suites.py")
assert SPEC and SPEC.loader
suites = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = suites  # dataclasses resolve annotations through the module
SPEC.loader.exec_module(suites)

MANIFEST = """
[[group]]
name = "os"
inputs = ["os/"]
suites = ["os/tests/a.sh", "os/tests/b.py"]

[[group]]
name = "workspace"
inputs = ["*Cargo.toml", "docs/one.md"]
suites = ["scripts/tests/test_c.py"]

[[covered]]
suite = "os/tests/helper.py"
by = "os/tests/b.py"

[[unrequired]]
suite = "os/tests/vm.sh"
reason = "boots a VM"
"""


class ParseTests(unittest.TestCase):
    def test_production_manifest_parses_and_invalid_manifests_are_rejected(self):
        with self.subTest(phase='production manifest parses'):
            manifest = suites.load(ROOT)
            self.assertIn("os/tests/containerfile_copy_invariants.sh", manifest.suites())
            self.assertIn("os/tests/internal_bundle_test.py", manifest.suites())
            self.assertEqual(
                manifest.unrequired_checks["os/containers/tilo-lounge/test-e2e.sh"], "e2e")
        with self.subTest(phase='rejects duplicates unknown keys and unsafe paths'):
            for text in (
                MANIFEST + '[[unrequired]]\nsuite = "os/tests/a.sh"\nreason = "again"\n',
                MANIFEST + '[[group]]\nname = "os"\ninputs = ["x/"]\nsuites = ["x/y.sh"]\n',
                MANIFEST + '[extra]\nkey = 1\n',
                '[[group]]\nname = "x"\ninputs = ["os/"]\nsuites = ["../escape.sh"]\n',
                '[[group]]\nname = "x"\ninputs = ["/abs/"]\nsuites = ["os/a.sh"]\n',
                '[[group]]\nname = "x"\ninputs = ["os/"]\nsuites = ["os/has space.sh"]\n',
                '[[group]]\nname = "x"\ninputs = ["os/"]\nsuites = ["os/tests/a.rs"]\n',
                '[[group]]\nname = "x"\ninputs = []\nsuites = ["os/a.sh"]\n',
                '[[group]]\nname = "x"\ninputs = ["os/"]\nsuites = ["os/a.sh"]\nextra = 1\n',
                '[[unrequired]]\nsuite = "os/a.sh"\nreason = " "\n',
                '[[unrequired]]\nsuite = "os/a.sh"\nreason = "x"\ncheck = "Not A Check"\n',
                '[[unrequired]]\nsuite = "os/a.sh"\nreason = "x"\ncheck = ""\n',
                '[[unrequired]]\nsuite = "os/a.sh"\nreason = "x"\nextra = 1\n',
                '[[covered]]\nsuite = "os/a.sh"\n',
                '[[covered]]\nsuite = "os/a.sh"\ncheck = "Not A Check"\n',
                '[[covered]]\nsuite = "os/a.sh"\nby = "os/tests/b.py"\ncheck = "sdk-python"\n',
            ):
                with self.subTest(text=text), self.assertRaises(ValueError):
                    suites.parse(text)


class SelectTests(unittest.TestCase):
    def setUp(self):
        self.manifest = suites.parse(MANIFEST)

    def test_changes_select_their_group_suite_or_everything(self):
        with self.subTest(phase='changed inputs select their group only'):
            self.assertEqual(suites.select(self.manifest, ["os/dev/tool.py"]),
                             ["os/tests/a.sh", "os/tests/b.py"])
            self.assertEqual(suites.select(self.manifest, ["crates/x/Cargo.toml"]),
                             ["scripts/tests/test_c.py"])
            self.assertEqual(suites.select(self.manifest, ["docs/one.md"]), ["scripts/tests/test_c.py"])
            self.assertEqual(suites.select(self.manifest, ["docs/one.md.orig", "docs/other.md"]), [])
            self.assertEqual(suites.select(self.manifest, ["apps/calculator/src/main.rs"]), [])
        with self.subTest(phase='a changed suite selects itself'):
            self.assertEqual(suites.select(self.manifest, ["scripts/tests/test_c.py"]),
                             ["scripts/tests/test_c.py"])
        with self.subTest(phase='control inputs and run all select everything'):
            everything = self.manifest.suites()
            self.assertEqual(suites.select(self.manifest, [], run_all=True), everything)
            for path in sorted(suites.CONTROL_INPUTS):
                with self.subTest(path=path):
                    self.assertEqual(suites.select(self.manifest, [path]), everything)
        with self.subTest(phase='internal bundle export logic selects its test'):
            # The regression that motivated path selection: an exporter change
            # must run the suite that proves USB-free install export.
            selected = suites.select(suites.load(ROOT), ["os/dev/internal-bundle.py"])
            self.assertIn("os/tests/internal_bundle_test.py", selected)
            self.assertIn("os/tests/containerfile_copy_invariants.sh", selected)


class AuditTests(unittest.TestCase):
    def audit(self, files: dict[str, str], manifest: str = MANIFEST) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = {
                "tools/security-boundary-invariants.txt": "os/tests/secure.sh\n",
                "tools/policy/run.sh": "bash tools/tests/cargo_shim.sh\n",
                "tools/policy/steps.toml": "[[step]]\nrun = \"python3 os/tests/native.py\"\n",
                "os/tests/a.sh": "true\n",
                "os/tests/b.py": "import helper\n",
                "os/tests/helper.py": "\n",
                "os/tests/vm.sh": "true\n",
                "scripts/tests/test_c.py": "print('ok')\n",
                "os/tests/secure.sh": "true\n",
                "os/tests/native.py": "\n",
                "os/tests/serial_runner.sh": "true\n",
                "tools/tests/cargo_shim.sh": "true\n",
                "tools/tests/test_discovered.py": "\n",
            }
            base.update(files)
            for name, text in base.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(text)
            return suites.audit(root, sorted(name for name, text in base.items() if text is not None),
                                suites.parse(manifest))

    def test_a_policy_step_checks_the_suite_it_runs_not_the_files_it_lists(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tools/policy").mkdir(parents=True)
            (root / "tools/security-boundary-invariants.txt").write_text("\n")
            (root / "tools/policy/run.sh").write_text("\n")
            (root / "tools/policy/steps.toml").write_text(
                '[[step]]\nname = "a"\nlabel = "A"\nrun = "python3 os/tests/ran.py"\n'
                'inputs = ["os/tests/listed.py"]\n')
            guarded = suites.always_run(root)
        self.assertIn("os/tests/ran.py", guarded)
        self.assertNotIn("os/tests/listed.py", guarded)

    def test_classification_audit_accepts_complete_and_rejects_gaps(self):
        with self.subTest(phase='complete classification passes'):
            self.assertEqual(self.audit({}), [])
        with self.subTest(phase='unclassified test shaped files are rejected'):
            for path in ("os/tests/new.sh", "os/tests/new.py", "tools/tests/new.sh",
                         "dist/x/test-contract.sh", "apps/x/test_y.py", "scripts/z_test.py",
                         "os/other_invariants.sh", "crates/x/web/y.test.mjs"):
                with self.subTest(path=path):
                    problems = self.audit({path: "true\n"})
                    self.assertEqual(len(problems), 1, problems)
                    self.assertIn(path, problems[0])
            for path in ("vendor/x/test_y.py", "tools/tests/test_new.py", "os/tests/lib/x.sh",
                         "os/dev/helper.py"):
                with self.subTest(path=path):
                    self.assertEqual(self.audit({path: "true\n"}), [])
        with self.subTest(phase='stale or redundant entries are rejected'):
            problems = self.audit({}, MANIFEST + '[[unrequired]]\nsuite = "os/tests/gone.sh"\nreason = "x"\n')
            self.assertTrue(any("gone.sh" in problem and "not a tracked file" in problem for problem in problems))
            problems = self.audit({}, MANIFEST + '[[unrequired]]\nsuite = "os/tests/secure.sh"\nreason = "x"\n')
            self.assertTrue(any("already runs on every change" in problem for problem in problems))
        with self.subTest(phase='covered entries need a guarded referencing suite'):
            problems = self.audit({"os/tests/b.py": "print('no import')\n"})
            self.assertTrue(any("does not reference it" in problem for problem in problems), problems)
            manifest = MANIFEST.replace('by = "os/tests/b.py"', 'by = "os/tests/vm.sh"')
            problems = self.audit({"os/tests/vm.sh": "python3 os/tests/helper.py\n"}, manifest)
            self.assertTrue(any("is not itself guarded" in problem for problem in problems), problems)
        with self.subTest(phase='python unittest suites must run when executed'):
            problems = self.audit({"os/tests/b.py": "import helper, unittest\nclass T(unittest.TestCase): pass\n"})
            self.assertTrue(any("unittest.main" in problem for problem in problems), problems)

    def test_a_t_check_covers_the_suites_it_runs_in_its_own_image(self):
        manifest = MANIFEST + '''
[[covered]]
suite = "sdks/test_a.py"
check = "sdk-python"
'''
        files = {
            "sdks/test_a.py": "import pytest\n",
            "t.toml": '[[check]]\nname = "sdk-python"\nrun = "bash tools/ci/sdk-python.sh"\n',
        }
        self.assertEqual(self.audit(files, manifest), [])
        # Every head of stable runs a check whose `on` names it, idle on main or not: a release
        # is stabilized there, so the suite runs before it ships.
        for declaration in ('on = ["stable"]\n', 'on = ["idle", "stable"]\n', 'on = ["stable"]\ndefault = false\n'):
            with self.subTest(declaration=declaration):
                self.assertEqual(self.audit(
                    dict(files, **{"t.toml": '[[check]]\nname = "sdk-python"\nrun = "true"\n' + declaration}),
                    manifest), [])
        # A check that names only other places in its `on`, or is default = false, runs there and
        # when asked: it checks no change, so it cannot prove a suite runs.
        for declaration in ('on = ["idle"]\n', 'on = ["dispatch"]\n', 'default = false\n'):
            with self.subTest(declaration=declaration):
                problems = self.audit(
                    dict(files, **{"t.toml": '[[check]]\nname = "sdk-python"\nrun = "true"\n' + declaration}),
                    manifest)
                self.assertTrue(any("is not a check t.toml runs for a change" in problem
                                    for problem in problems), problems)
        with self.subTest(phase="a check t.toml does not declare"):
            problems = self.audit(
                dict(files, **{"t.toml": '[[check]]\nname = "other"\nrun = "true"\n'}), manifest)
            self.assertTrue(any("sdk-python" in problem for problem in problems), problems)

    def test_a_templated_checks_instance_covers_the_suite_it_runs(self):
        # A flow's check is an instance of t.toml's family (`flows/{flow}`), and the suite
        # names the instance that runs it.
        manifest = MANIFEST + '''
[[covered]]
suite = "os/tests/flow.sh"
check = "flows/four-devices"
'''
        files = {
            "os/tests/flow.sh": "true\n",
            "t.toml": '[[check]]\nname = "flows/{flow}"\nrun = "bash os/tests/flow.sh {flow}"\n'
                      'matrix = { flow = ["four-devices", "laptop-phone-server"] }\n',
        }
        self.assertEqual(self.audit(files, manifest), [])
        with self.subTest(phase="an instance the matrix does not make"):
            problems = self.audit(
                dict(files, **{"os/tests/flow.sh": "true\n"}),
                manifest.replace("flows/four-devices", "flows/none"))
            self.assertTrue(any("is not a check t.toml runs for a change" in problem
                                for problem in problems), problems)

    def test_an_unrequired_suite_names_the_t_check_that_executes_it(self):
        manifest = MANIFEST.replace('reason = "boots a VM"',
                                    'reason = "boots a VM"\ncheck = "vm"')
        self.assertEqual(self.audit(
            {"t.toml": '[[check]]\nname = "vm"\nrun = "bash os/tests/vm.sh"\n'}, manifest), [])
        # Any trigger will do: the suite is not required, so its check need not execute for a change.
        self.assertEqual(self.audit(
            {"t.toml": '[[check]]\nname = "vm"\nrun = "bash os/tests/vm.sh"\non = ["dispatch"]\n'},
            manifest), [])
        with self.subTest(phase="a check t.toml does not declare"):
            problems = self.audit(
                {"t.toml": '[[check]]\nname = "other"\nrun = "bash os/tests/vm.sh"\n'}, manifest)
            self.assertTrue(any("check vm is not a check t.toml declares" in problem
                                for problem in problems), problems)
        with self.subTest(phase="a check that executes something else"):
            problems = self.audit(
                {"t.toml": '[[check]]\nname = "vm"\nrun = "bash os/tests/b.py"\n'}, manifest)
            self.assertTrue(any("check vm does not execute it" in problem
                                for problem in problems), problems)
        with self.subTest(phase="the one script a check executes names it"):
            self.assertEqual(self.audit({
                "t.toml": '[[check]]\nname = "vm"\nrun = "bash tools/ci/vm.sh"\n',
                "tools/ci/vm.sh": "bash os/tests/vm.sh\n"}, manifest), [])
        with self.subTest(phase="a matrix instance names its family"):
            self.assertEqual(self.audit({
                "t.toml": '[[check]]\nname = "vm-{arch}"\nrun = "bash os/tests/vm.sh"\n'
                          '[check.matrix]\narch = ["x86_64", "aarch64"]\n',
            }, manifest.replace('check = "vm"', 'check = "vm-x86_64"')), [])

    def test_a_dispatcher_may_execute_a_neighbouring_suite_by_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "os/containers/flows").mkdir(parents=True)
            (root / "os/containers/flows/steps.sh").write_text(
                'exec bash "$here/test-wait-list.sh" "$bundle"\n')
            self.assertTrue(suites.executes_suite(
                root, "bash os/containers/flows/steps.sh /target/out",
                "os/containers/flows/test-wait-list.sh"))
            # A script of another directory does not stand for the suite.
            self.assertFalse(suites.executes_suite(
                root, "bash os/containers/flows/steps.sh /target/out",
                "os/containers/elsewhere/test-wait-list.sh"))

    def test_cargo_integration_driver_is_a_covering_check(self):
        manifest = MANIFEST.replace('by = "os/tests/b.py"', 'by = "crates/native/tests/oracle.rs"')
        manifest += '''
[[cargo]]
package = "native"
inputs = ["os/tests/helper.py"]
readers = ["crates/native/tests/oracle.rs"]
'''
        files = {
            "crates/native/Cargo.toml": '[package]\nname = "native"\nversion = "0.1.0"\n',
            "crates/native/tests/oracle.rs": (
                '#[test] fn full_suite() { '
                'include_str!("../../../os/tests/helper.py"); }'),
        }
        self.assertEqual(self.audit(files, manifest), [])
        for source in ('fn main() { driver("helper.py"); }',
                       '#[test] fn unrelated() {}'):
            with self.subTest(source=source):
                changed = dict(files, **{"crates/native/tests/oracle.rs": source})
                self.assertTrue(self.audit(changed, manifest))
        files["crates/native/Cargo.toml"] += 'autotests = false\n'
        self.assertTrue(self.audit(files, manifest))
    def test_missing_covering_suite_cannot_prove_coverage(self):
        # Workflow text can still mention a deleted script. That mention alone
        # is not evidence that the helper's tests are executed.
        manifest = MANIFEST.replace('by = "os/tests/b.py"', 'by = "os/tests/deleted.py"')
        with patch.object(suites, "always_run", return_value={"os/tests/deleted.py"}):
            problems = self.audit({}, manifest)
        self.assertTrue(any("covering suite os/tests/deleted.py is not a tracked file on disk"
                            in problem for problem in problems), problems)

    def test_production_tree_is_fully_classified(self):
        result = subprocess.run([sys.executable, str(ROOT / "tools/ci/suites.py"), "check"],
                                cwd=ROOT, capture_output=True, text=True)
        # `check` prints its findings on stdout.
        self.assertEqual(result.returncode, 0,
                         f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}")


READER_MANIFEST = """
[[cargo]]
package = "ui"
inputs = ["apps/*/src/main.rs", "os/share/"]
readers = ["crates/ui/tests/audit.rs"]
"""


class CargoReaderTests(unittest.TestCase):
    PACKAGES = {
        "crates/ui/Cargo.toml": '[package]\nname = "ui"\n',
        "apps/calc/Cargo.toml": '[package]\nname = "calc"\n',
        "apps/calc/src/main.rs": "fn main() {}\n",
        "os/share/data.json": "{}\n",
        "crates/ui/tests/audit.rs": (
            'fn root() -> PathBuf {\n'
            '    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()\n'
            '}\n'),
    }

    def audit(self, files: dict[str, str], manifest: str = READER_MANIFEST) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree = {**self.PACKAGES, **files}
            for name, text in tree.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(text)
            return suites.audit_cargo(root, sorted(tree), suites.parse(manifest))

    def test_unreadable_package_manifest_cannot_hide_an_undeclared_reader(self):
        reader = 'const DATA: &str = include_str!("../../../os/share/data.json");\n'
        # calc has no declared reader: losing its package mapping used to make
        # cross_package_reads skip the source entirely and the audit pass.
        with self.assertRaisesRegex(ValueError, "cannot audit Cargo package apps/calc/Cargo.toml"):
            self.audit({"apps/calc/Cargo.toml": "[package", "apps/calc/src/main.rs": reader})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_bytes(b"[package]\nname = '\xff'\n")
            with self.assertRaisesRegex(ValueError, "cannot audit Cargo package Cargo.toml"):
                suites.audit_cargo(root, ["Cargo.toml"], suites.parse(""))
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "cannot audit Cargo package Cargo.toml"):
                suites.audit_cargo(Path(directory), ["Cargo.toml"], suites.parse(""))

    def test_reader_manifest_parses_strictly_and_selects_reading_package(self):
        with self.subTest(phase='parse rejects malformed entries'):
            for text in (
                READER_MANIFEST + READER_MANIFEST.replace("audit.rs", "other.rs"),
                READER_MANIFEST.replace('readers = ["crates/ui/tests/audit.rs"]', 'readers = ["x.py"]'),
                READER_MANIFEST.replace('package = "ui"', 'package = "bad name"'),
                READER_MANIFEST + "extra = 1\n",
                '[[cargo-exempt]]\nfile = "a.rs"\n',
                '[[cargo-exempt]]\nfile = "a.rs"\nreason = " "\n',
                READER_MANIFEST + '[[cargo-exempt]]\nfile = "crates/ui/tests/audit.rs"\nreason = "x"\n',
            ):
                with self.subTest(text=text), self.assertRaises(ValueError):
                    suites.parse(text)
        with self.subTest(phase='scoped inputs select only the reading tests'):
            manifest = suites.parse(READER_MANIFEST.replace(
                'readers = ["crates/ui/tests/audit.rs"]\n',
                'readers = ["crates/ui/tests/audit.rs"]\n'
                'scope = { inputs = ["apps/*/src/main.rs"], test = "it", filter = "test(/^audit::/)" }\n'))
            scope = suites.select_package_scopes(manifest, ["apps/calc/src/main.rs"])["ui"]
            self.assertEqual((scope.test, scope.filter), ("it", "test(/^audit::/)"))
            # An unscoped input among the changes needs the whole suite.
            self.assertEqual(suites.select_package_scopes(
                manifest, ["apps/calc/src/main.rs", "os/share/x.json"]), {"ui": None})
            self.assertEqual(suites.select_package_scopes(manifest, ["os/share/x.json"]), {"ui": None})
            self.assertEqual(suites.select_package_scopes(manifest, ["apps/calc/Cargo.toml"]), {})
            # Without a scope every selection is whole.
            self.assertEqual(suites.select_package_scopes(
                suites.parse(READER_MANIFEST), ["apps/calc/src/main.rs"]), {"ui": None})
        with self.subTest(phase='parse rejects malformed scopes'):
            base = 'readers = ["crates/ui/tests/audit.rs"]\n'
            for scope in (
                'scope = { inputs = ["os/other/"], test = "it", filter = "test(a)" }',
                'scope = { inputs = ["os/share/"], test = "two words", filter = "test(a)" }',
                'scope = { inputs = ["os/share/"], test = "it", filter = " " }',
                'scope = { inputs = ["os/share/"], test = "it" }',
                'scope = { inputs = ["os/share/"], test = "it", filter = "test(a)", extra = 1 }',
                'scope = "it"',
            ):
                with self.subTest(scope=scope), self.assertRaises(ValueError):
                    suites.parse(READER_MANIFEST.replace(base, base + scope + "\n"))
        with self.subTest(phase='production ui keeps compiled inputs whole'):
            manifest = suites.load()
            # No app source is read by the toolkit's tests any more.
            self.assertNotIn(
                "tilo-ui",
                suites.select_package_scopes(manifest, ["apps/discord/src/lib.rs"]),
            )
            # The vendored quit request is compiled into the app runner's library.
            self.assertIsNone(suites.select_package_scopes(
                manifest, ["vendor/winit/src/platform_impl/quit_request.rs"])["tilo-ui-app"])
        with self.subTest(phase='declared inputs select the reading package'):
            manifest = suites.parse(READER_MANIFEST)
            self.assertEqual(suites.select_packages(manifest, ["apps/calc/src/main.rs"]), ["ui"])
            self.assertEqual(suites.select_packages(manifest, ["os/share/deep/x.txt"]), ["ui"])
            self.assertEqual(suites.select_packages(manifest, ["apps/calc/Cargo.toml"]), [])

    def test_office_fixture_changes_select_their_mount_consumers(self):
        manifest = suites.load()
        prefix = "crates/integrations/cloud-source-gdrive/tests/fixtures/"
        for fixture in ("docx.rs", "xlsx.rs", "pptx.rs"):
            with self.subTest(fixture=fixture):
                self.assertEqual(
                    suites.select_packages(manifest, [prefix + fixture]), ["tilo-fs"])
        # A fixture the fs tests do not read selects nothing.
        self.assertEqual(
            suites.select_packages(manifest, [prefix + "unrelated.rs"]), [])

    def test_cross_package_readers_must_be_declared_exactly(self):
        with self.subTest(phase='a declared reader passes'):
            self.assertEqual(self.audit({}), [])
        with self.subTest(phase='an undeclared reader is rejected'):
            for text in (
                'const A: &str = include_str!("../../../os/share/data.json");\n',
                '#[path = "../../../crates/ui/src/shared.rs"]\nmod shared;\n',
                'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/share/data.json");\n',
                'let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../os/share/data.json");\n',
                # A macro's include whose path a concat! builds from its arguments.
                'const A: &str = include_str!(concat!("../../../os/share/", $name, ".json"));\n',
                'let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))\n    .parent()\n    .unwrap()\n    .join("x");\n',
            ):
                with self.subTest(text=text):
                    problems = self.audit({"apps/calc/src/lib.rs": text})
                    self.assertEqual(len(problems), 1, problems)
                    self.assertIn("apps/calc/src/lib.rs", problems[0])
                    self.assertIn('package = "calc"', problems[0])
        with self.subTest(phase='reads inside the package and build output are not cross package'):
            for text in (
                'const A: &str = include_str!("../tests/fixtures/a.json");\n',
                'const A: &str = include_str!("strings.ftl");\n',
                'const A: &str = include_str!(concat!("../tests/fixtures/", $name));\n',
                '#[path = "platform/linux.rs"]\nmod platform;\n',
                'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");\n',
                'let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/review");\n',
                'let out = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().join("target");\n',
            ):
                with self.subTest(text=text):
                    self.assertEqual(self.audit({"apps/calc/src/lib.rs": text}), [])
        with self.subTest(phase='an include outside the declared inputs is rejected'):
            problems = self.audit({"crates/ui/tests/audit.rs": (
                'const A: &str = include_str!("../../../apps/calc/Cargo.toml");\n')})
            self.assertEqual(len(problems), 1, problems)
            self.assertIn("includes apps/calc/Cargo.toml", problems[0])
        with self.subTest(phase='stale and misattributed entries are rejected'):
            problems = self.audit({"crates/ui/tests/audit.rs": "fn nothing() {}\n"})
            self.assertTrue(any("no longer reads outside ui" in problem for problem in problems), problems)
            problems = self.audit({}, READER_MANIFEST.replace('package = "ui"', 'package = "calc"'))
            self.assertTrue(any("does not own it" in problem for problem in problems), problems)
            problems = self.audit({}, READER_MANIFEST.replace('"os/share/"', '"os/gone/"'))
            self.assertTrue(any("os/gone/ matches no tracked file" in problem for problem in problems), problems)
            problems = self.audit({}, READER_MANIFEST.replace('package = "ui"', 'package = "nowhere"'))
            self.assertTrue(any("no tracked Cargo.toml" in problem for problem in problems), problems)
            problems = self.audit({}, READER_MANIFEST + (
                '[[cargo-exempt]]\nfile = "apps/calc/src/main.rs"\nreason = "writes only"\n'))
            self.assertTrue(any("reads nothing outside" in problem for problem in problems), problems)
        with self.subTest(phase='an exempt reader needs no inputs'):
            text = 'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/x.py");\n'
            manifest = READER_MANIFEST + '[[cargo-exempt]]\nfile = "apps/calc/src/lib.rs"\nreason = "ignored test"\n'
            self.assertEqual(self.audit({"apps/calc/src/lib.rs": text}, manifest), [])

    APPS = {
        "apps/calc/tilo-app.toml": "",
        "apps/settings/Cargo.toml": '[package]\nname = "notes"\n',
        "apps/settings/src/main.rs": "fn main() {}\n",
        "apps/settings/tilo-app.toml": "",
    }

    def test_inputs_cover_what_paths_built_from_the_manifest_dir_read(self):
        lists_apps = (
            'fn every_app() {\n'
            '    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();\n'
            '    for app in std::fs::read_dir(root.join("apps")).unwrap() {\n'
            '        let manifest = app.unwrap().path().join("tilo-app.toml");\n'
            '        let _ = std::fs::read_to_string(manifest);\n'
            '    }\n'
            '}\n')
        enumerated = READER_MANIFEST.replace('"apps/*/src/main.rs"', '"apps/calc/tilo-app.toml"')
        with self.subTest(phase='an input for some entries of a listed directory is not enough'):
            problems = self.audit({**self.APPS, "crates/ui/tests/audit.rs": lists_apps}, enumerated)
            self.assertEqual(len(problems), 1, problems)
            self.assertIn("crates/ui/tests/audit.rs:3: lists apps and reads apps/settings/tilo-app.toml", problems[0])
        with self.subTest(phase='a glob over the entries covers every one, and those to come'):
            manifest = READER_MANIFEST.replace('"apps/*/src/main.rs"', '"apps/*/tilo-app.toml"')
            self.assertEqual(self.audit({**self.APPS, "crates/ui/tests/audit.rs": lists_apps}, manifest), [])
        with self.subTest(phase='an entry without the name read is not required'):
            self.assertEqual(self.audit({"apps/calc/tilo-app.toml": "", "crates/ui/tests/audit.rs": lists_apps},
                                        enumerated), [])
        with self.subTest(phase='bindings, aliases and root functions are followed'):
            for text in (
                'fn root() -> PathBuf {\n    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")\n}\n'
                'fn every_app() {\n    let apps = root().join("apps");\n'
                '    for app in std::fs::read_dir(&apps).unwrap() {\n'
                '        read(app.unwrap().path().join("tilo-app.toml"));\n    }\n}\n',
                'fn every_app() {\n    let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps");\n'
                '    for app in std::fs::read_dir(&apps).unwrap() {\n'
                '        read(app.unwrap().path().join("tilo-app.toml"));\n    }\n}\n',
            ):
                with self.subTest(text=text):
                    problems = self.audit({**self.APPS, "crates/ui/tests/audit.rs": text}, enumerated)
                    self.assertEqual(len(problems), 1, problems)
                    self.assertIn("lists apps and reads apps/settings/tilo-app.toml", problems[0])
        with self.subTest(phase='a joined path must match an input'):
            text = ('fn notes() {\n    let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap();\n'
                    '    read(root.join("apps/settings"));\n    read(root.join("os/share/data.json"));\n}\n')
            problems = self.audit({**self.APPS, "crates/ui/tests/audit.rs": text}, enumerated)
            self.assertEqual(problems, ["crates/ui/tests/audit.rs:3: reads apps/settings, "
                                        "which no input of cargo ui matches"])
            self.assertEqual(self.audit({**self.APPS, "crates/ui/tests/audit.rs": text},
                                        READER_MANIFEST.replace('"apps/*/src/main.rs"', '"apps/"')), [])
        with self.subTest(phase='paths the tree does not hold and the package itself are not required'):
            text = ('fn scratch() {\n    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");\n'
                    '    write(root.join("apps/new/tilo-app.toml"));\n    read(root.join("crates/ui/src"));\n}\n')
            self.assertEqual(self.audit({**self.APPS, "crates/ui/tests/audit.rs": text}, enumerated), [])

    def test_a_file_the_checked_change_deletes_is_not_tracked(self):
        # t checks a commit its server has not seen as uncommitted changes on
        # the commit it branched from: what the change deletes stays indexed.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            git = ["git", "-c", "user.name=t", "-c", "user.email=t@example.com"]
            subprocess.run([*git, "init", "-q"], cwd=root, check=True)
            (root / "kept.rs").write_text("fn kept() {}\n")
            (root / "gone.rs").write_text("fn gone() {}\n")
            subprocess.run([*git, "add", "."], cwd=root, check=True)
            subprocess.run([*git, "commit", "-q", "-m", "base"], cwd=root, check=True)
            (root / "gone.rs").unlink()
            self.assertEqual(suites.tracked_files(root), ["kept.rs"])


class ReportTests(unittest.TestCase):
    def report(self, manifest: str) -> tuple[str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(suites, "load", return_value=suites.parse(manifest)), \
                redirect_stdout(stdout), redirect_stderr(stderr):
            self.assertEqual(suites.main(["report"]), 0)
        return stdout.getvalue(), stderr.getvalue()

    def test_report_counts_unrequired_suites_without_a_check(self):
        self.assertEqual(self.report(MANIFEST), ("os/tests/vm.sh\n", "1 of 1 unrequired suites "
                                                 "name no t check yet\n"))
        with_check = MANIFEST.replace('reason = "boots a VM"', 'reason = "boots a VM"\ncheck = "vm"')
        self.assertEqual(self.report(with_check)[0], "")
        self.assertIn("0 of 1 unrequired suites name no t check yet", self.report(with_check)[1])
        # A bare invocation reports too.
        with patch.object(suites, "load", return_value=suites.parse(MANIFEST)), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.assertEqual(suites.main([]), 0)


class GroupSelectionTests(unittest.TestCase):
    def test_run_group_selects_that_groups_suites_and_rejects_unknown_names(self):
        group = suites.load(ROOT).groups[0]
        calls = []
        with unittest.mock.patch.object(suites, "run", lambda root, selected, jobs: calls.append(selected) or 0):
            self.assertEqual(suites.main(["run", "--group", group.name]), 0)
        self.assertEqual(calls, [list(group.suites)])
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            suites.main(["run", "--group", "no-such-group"])
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            suites.main(["run", "--group", group.name, "--all"])


if __name__ == "__main__":
    unittest.main()
