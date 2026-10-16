"""Selection, execution and policy for the hermetic suites outside Cargo."""
from __future__ import annotations

import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest
import unittest.mock
from contextlib import redirect_stderr, redirect_stdout

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
                '[[covered]]\nsuite = "os/a.sh"\n',
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


class RunTests(unittest.TestCase):
    def run_suites(self, files: dict[str, str], names: list[str]) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            listed = [name for name in files if name.endswith((".py", ".sh", ".test.mjs"))]
            (root / "tools/ci").mkdir(parents=True)
            (root / suites.MANIFEST).write_text(
                '[[group]]\nname = "g"\ninputs = ["x/"]\nsuites = [%s]\n'
                % ", ".join(f'"{name}"' for name in listed))
            for name, text in files.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(textwrap.dedent(text))
            output = io.StringIO()
            with redirect_stdout(output):
                code = suites.run(root, names, jobs=2)
            return code, output.getvalue()

    def test_selected_suites_all_run_from_root_and_any_failure_fails(self):
        with self.subTest(phase='all suites run and one failure fails the run'):
            code, output = self.run_suites(
                {"x/ok.sh": "echo fine\n", "x/bad.py": "raise SystemExit(3)\n"},
                ["x/ok.sh", "x/bad.py"])
            self.assertEqual(code, 1)
            self.assertIn("x/ok.sh: ok", output)
            self.assertIn("x/bad.py: FAILED (exit 3)", output)
            self.assertIn("1/2 suites passed", output)
        with self.subTest(phase='suites run from the repository root'):
            code, _ = self.run_suites({"x/where.sh": "test -f tools/ci/suites.toml\n"}, ["x/where.sh"])
            self.assertEqual(code, 0)
        with self.subTest(phase='suites run without the caller\'s desktop session'):
            code, output = self.run_suites(
                {"x/session.sh": 'test -z "${WAYLAND_DISPLAY:-}" && test -n "$TILO_TEST_ENVIRONMENT" '
                                 '&& test "$XDG_RUNTIME_DIR" = "$TILO_TEST_ENVIRONMENT/runtime"\n'},
                ["x/session.sh"])
            self.assertEqual(code, 0, output)
        with self.subTest(phase='JavaScript suites run under node --test'):
            code, output = self.run_suites(
                {"x/ok.test.mjs": "import test from 'node:test';\ntest('passes', () => {});\n",
                 "x/bad.test.mjs": "import test from 'node:test';\n"
                                   "test('fails', () => { throw new Error('no'); });\n"},
                ["x/ok.test.mjs", "x/bad.test.mjs"])
            self.assertEqual(code, 1, output)
            self.assertIn("x/ok.test.mjs: ok", output)
            self.assertIn("x/bad.test.mjs: FAILED", output)
        with self.subTest(phase='undeclared or empty selection fails'):
            self.assertEqual(self.run_suites({"x/ok.sh": "true\n"}, ["x/other.sh"])[0], 2)
            self.assertEqual(self.run_suites({"x/ok.sh": "true\n"}, [])[0], 2)


class AuditTests(unittest.TestCase):
    def audit(self, files: dict[str, str], manifest: str = MANIFEST) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = {
                "tools/security-boundary-invariants.txt": "os/tests/secure.sh\n",
                "tools/policy/run.sh": "bash tools/tests/cargo_shim.sh\n",
                ".forgejo/workflows/check.yml": "python3 os/tests/native.py\n",
                ".forgejo/workflows/lint.yml": "\n",
                "os/tests/a.sh": "true\n",
                "os/tests/b.py": "import helper\n",
                "os/tests/helper.py": "\n",
                "os/tests/vm.sh": "true\n",
                "scripts/tests/test_c.py": "print('ok')\n",
                "os/tests/secure.sh": "true\n",
                "os/tests/native.py": "\n",
                "tools/tests/cargo_shim.sh": "true\n",
                "tools/tests/test_discovered.py": "\n",
            }
            base.update(files)
            for name, text in base.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(text)
            return suites.audit(root, sorted(name for name, text in base.items() if text is not None),
                                suites.parse(manifest))

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
        with self.subTest(phase='covered entries need a gated referencing suite'):
            problems = self.audit({"os/tests/b.py": "print('no import')\n"})
            self.assertTrue(any("does not reference it" in problem for problem in problems), problems)
            manifest = MANIFEST.replace('by = "os/tests/b.py"', 'by = "os/tests/vm.sh"')
            problems = self.audit({"os/tests/vm.sh": "python3 os/tests/helper.py\n"}, manifest)
            self.assertTrue(any("is not itself gated" in problem for problem in problems), problems)
        with self.subTest(phase='python unittest suites must run when executed'):
            problems = self.audit({"os/tests/b.py": "import helper, unittest\nclass T(unittest.TestCase): pass\n"})
            self.assertTrue(any("unittest.main" in problem for problem in problems), problems)

    def test_production_tree_is_fully_classified(self):
        result = subprocess.run([sys.executable, str(ROOT / "tools/ci/suites.py"), "check"],
                                cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


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
        with self.subTest(phase='production ui scope keeps compiled inputs whole'):
            manifest = suites.load()
            scoped = suites.select_package_scopes(manifest, ["apps/scratchpad/src/main.rs"])
            self.assertEqual(scoped["tilo-ui"].test, "it")
            self.assertIn("design_token_audit", scoped["tilo-ui"].filter)
            self.assertIn("spacing_token_audit", scoped["tilo-ui"].filter)
            # The vendored quit request is compiled into tilo-ui's library.
            self.assertIsNone(suites.select_package_scopes(
                manifest, ["vendor/winit/src/platform_impl/quit_request.rs"])["tilo-ui"])
        with self.subTest(phase='declared inputs select the reading package'):
            manifest = suites.parse(READER_MANIFEST)
            self.assertEqual(suites.select_packages(manifest, ["apps/calc/src/main.rs"]), ["ui"])
            self.assertEqual(suites.select_packages(manifest, ["os/share/deep/x.txt"]), ["ui"])
            self.assertEqual(suites.select_packages(manifest, ["apps/calc/Cargo.toml"]), [])

    def test_cross_package_readers_must_be_declared_exactly(self):
        with self.subTest(phase='a declared reader passes'):
            self.assertEqual(self.audit({}), [])
        with self.subTest(phase='an undeclared reader is rejected'):
            for text in (
                'const A: &str = include_str!("../../../os/share/data.json");\n',
                '#[path = "../../../crates/ui/src/shared.rs"]\nmod shared;\n',
                'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/share/data.json");\n',
                'let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../os/share/data.json");\n',
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
