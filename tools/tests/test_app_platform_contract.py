import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy" / "check_app_platform_contract.py"
SPEC = importlib.util.spec_from_file_location("check_app_platform_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class AppPlatformContractTests(unittest.TestCase):
    def fixture(
        self,
        source: str = "pub fn portable() {}\n",
        *,
        manifest_tail: str = "",
        app_name: str = "sample",
    ) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        app = root / "apps" / app_name
        (app / "src").mkdir(parents=True)
        (app / "Cargo.toml").write_text(
            f'[package]\nname = "{app_name}"\nversion = "0.1.0"\n' + manifest_tail,
            encoding="utf-8",
        )
        (app / "src" / "lib.rs").write_text(source, encoding="utf-8")
        self.write_register(root)
        return root

    def write_register(self, root: Path) -> None:
        register = root / "tools/policy/app_platform_findings.json"
        register.parent.mkdir(parents=True, exist_ok=True)
        register.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "tasks": [{"id": "G03"}, {"id": "L01"}],
                    "findings": [{"id": "A01-01", "primary_task": "L01"}],
                }
            ),
            encoding="utf-8",
        )

    def write_baseline(self, root: Path, entries: list[dict]) -> Path:
        path = root / "tools/policy/app_platform_baseline.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"schema_version": 1, "entries": entries}, indent=2) + "\n",
            encoding="utf-8",
        )
        return path

    @staticmethod
    def reasons(violations) -> str:
        return "\n".join(violation.reason for violation in violations)

    def one_inventory_entry(self, root: Path) -> dict:
        sites, _ = contract.inventory(root)
        self.assertEqual(len(sites), 1)
        entry = sites[0].as_baseline_entry()
        entry.update(
            finding_id="A01-01",
            removal_task="L01",
            rationale="Move this exact native call behind the host service.",
        )
        return entry

    def test_framework_backend_is_allowed_but_app_native_import_fails(self):
        root = self.fixture("use std::os::unix::fs::PermissionsExt;\n")
        backend = root / "crates/system/example/src/lib.rs"
        backend.parent.mkdir(parents=True)
        backend.write_text("use std::os::unix::fs::PermissionsExt;\n", encoding="utf-8")
        violations, count = contract.audit(root)
        self.assertEqual(count, 1)
        self.assertTrue(any(v.rule == "native-api" for v in violations))
        self.assertTrue(all("apps/" in v.path.as_posix() for v in violations))

    def test_comments_strings_raw_strings_byte_strings_and_nested_comments_are_ignored(self):
        root = self.fixture(
            "// use nix::unistd;\n"
            "/* outer libc::open /* nested rustix::fs */ windows::Win32 */\n"
            'const A: &str = "std::os::unix";\n'
            'const B: &str = r###"ashpd::desktop and #[cfg(windows)]"###;\n'
            'const C: &[u8] = b"zbus::Connection";\n'
            'const D: &[u8] = br#"xattr::get"#;\n'
            "pub fn portable() { let _ = std::fs::read(\"/tmp/fixture\"); }\n"
        )
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

    def test_grouped_and_straightforward_aliased_native_imports_fail(self):
        root = self.fixture(
            "use nix::{sys::stat, unistd};\n"
            "use rustix as host;\n"
            "fn leak() { host::fs::open(\"x\", 0, 0); libc::close(1); }\n"
        )
        violations, _ = contract.audit(root)
        native = [v for v in violations if v.rule == "native-api"]
        self.assertEqual(len(native), 3)
        self.assertTrue(
            any(v.line == 3 and "renamed" in v.reason and "2 new occurrence" in v.reason for v in native)
        )

    def test_grouped_self_alias_native_import_is_resolved(self):
        root = self.fixture(
            "use rustix::{self as host, fs};\n"
            "fn leak() { host::fs::open(\"x\", 0, 0); }\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(
            any(v.line == 2 and v.rule == "native-api" and "renamed" in v.reason for v in violations)
        )

    def test_grouped_std_os_import_fails(self):
        root = self.fixture("use std::{os::unix::fs::PermissionsExt};\n")
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "native-api" for v in violations))

    def test_renamed_std_os_import_and_use_fail(self):
        root = self.fixture(
            "use std::os as platform;\n"
            "fn leak() { let _ = platform::unix::fs::PermissionsExt::mode; }\n"
        )
        violations, _ = contract.audit(root)
        native = [v for v in violations if v.rule == "native-api"]
        self.assertEqual(len(native), 2)
        self.assertTrue(any(v.line == 2 and "renamed" in v.reason for v in native))

    def test_grouped_std_os_self_alias_uses_remain_ratcheted(self):
        imports = (
            "use std::os::{self as platform};\n",
            "use std::{os::{self as platform}};\n",
            "use std::os::{unix, self as platform};\n",
            "use std::{io, os::{unix, self as platform}};\n",
        )
        for index, import_line in enumerate(imports):
            with self.subTest(import_line=import_line):
                root = self.fixture(import_line, app_name=f"sample{index}")
                self.write_baseline(root, [self.one_inventory_entry(root)])
                source = next((root / "apps").glob("*/src/lib.rs"))
                source.write_text(
                    import_line
                    + "fn leak() { let _ = platform::unix::fs::PermissionsExt::mode; }\n",
                    encoding="utf-8",
                )
                violations, _ = contract.audit(root)
                self.assertTrue(
                    any(
                        violation.line == 2
                        and violation.rule == "native-api"
                        and "renamed" in violation.reason
                        for violation in violations
                    )
                )

    def test_native_ffi_and_os_cfg_variants_fail_but_cfg_test_passes(self):
        root = self.fixture(
            '#[cfg(test)] fn test_only() {}\n'
            '#[cfg(unix)] fn unix_only() {}\n'
            '#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]\n'
            'fn selected() { if cfg!(target_family = "unix") {} }\n'
            'unsafe extern "C" { fn getpid() -> i32; }\n'
        )
        violations, _ = contract.audit(root)
        rules = [v.rule for v in violations]
        self.assertEqual(rules.count("platform-cfg"), 3)
        self.assertEqual(rules.count("native-ffi"), 1)

    def test_multiline_cfg_predicate_fails(self):
        root = self.fixture("#[cfg(\n    unix\n)]\nfn leak() {}\n")
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "platform-cfg" and v.line == 1 for v in violations))

    def test_nested_cfg_predicate_fails(self):
        root = self.fixture(
            '#[cfg(all(feature = "x", any(feature = "y"), unix))]\nfn leak() {}\n'
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "platform-cfg" for v in violations))

    def test_quote_character_does_not_hide_following_native_call(self):
        root = self.fixture(
            "pub fn leak() { let quote = '\"'; unsafe { libc::close(1); } }\n"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "native-api" and v.line == 1 for v in violations))

    def test_implicit_c_abi_extern_block_fails(self):
        root = self.fixture("unsafe extern { fn getpid() -> i32; }\n")
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "native-ffi" for v in violations))

    def test_direct_native_dependencies_in_all_dependency_table_kinds_fail(self):
        root = self.fixture(
            manifest_tail=(
                '[dependencies]\nnix = "0.1"\n'
                '[dev-dependencies]\nattrs = { package = "xattr", version = "1" }\n'
                '[build-dependencies]\nportal-runtime = { package = "runtime-portal-xdg", path = "../../crates/runtime/runtime-portal-xdg" }\n'
                "[target.'cfg(unix)'.dependencies]\nlow = { package = \"rustix\", version = \"1\" }\n"
            )
        )
        violations, _ = contract.audit(root)
        self.assertEqual(
            sum(v.rule == "native-dependency" for v in violations),
            4,
        )

    def test_workspace_inherited_and_renamed_native_dependency_fails(self):
        root = self.fixture(manifest_tail="[dependencies]\nposix.workspace = true\n")
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["apps/sample"]\n'
            '[workspace.dependencies]\nposix = { package = "nix", version = "0.1" }\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(
            any(
                v.rule == "native-dependency" and "nix" in v.reason and v.line == 5
                for v in violations
            )
        )

    def test_windows_sys_cargo_package_direct_renamed_and_inherited_fail(self):
        root = self.fixture(
            manifest_tail=(
                '[dependencies]\nwindows-sys = "0.59"\n'
                'win-api = { package = "windows-sys", version = "0.59" }\n'
                "inherited.workspace = true\n"
            )
        )
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["apps/sample"]\n'
            '[workspace.dependencies]\n'
            'inherited = { package = "windows-sys", version = "0.59" }\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        native = [v for v in violations if v.rule == "native-dependency"]
        self.assertEqual(len(native), 3)
        self.assertTrue(all("windows-sys" in v.reason for v in native))

    def test_dependency_subtable_reports_the_declaration_line(self):
        root = self.fixture(
            manifest_tail=(
                "[dependencies.posix]\n"
                'package = "nix"\n'
                'version = "0.1"\n'
            )
        )
        violations, _ = contract.audit(root)
        self.assertTrue(
            any(v.rule == "native-dependency" and v.line == 4 for v in violations)
        )

    def test_sdk_backend_features_fail_in_package_and_dependency_feature_arrays(self):
        root = self.fixture(
            manifest_tail=(
                '[dependencies]\nui = { path = "../../crates/sdk/ui-core", features = ["dmabuf-import"] }\n'
                '[features]\nweb = ["ui?/webview-wpe"]\nmedia = ["ui/mpris"]\n'
            )
        )
        violations, _ = contract.audit(root)
        self.assertEqual(sum(v.rule == "native-feature" for v in violations), 3)

    def test_renamed_tilo_app_dependency_feature_selection_fails(self):
        root = self.fixture(
            manifest_tail=(
                '[dependencies]\napp-sdk = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }\n'
                '[features]\nbrowser = ["app-sdk/webview-wpe"]\n'
            )
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "native-feature" and "webview-wpe" in v.reason for v in violations))

    def test_workspace_inherited_sdk_feature_array_fails(self):
        root = self.fixture(manifest_tail="[dependencies]\nui.workspace = true\n")
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["apps/sample"]\n'
            '[workspace.dependencies]\nui = { path = "crates/sdk/ui-core", features = ["mpris"] }\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.rule == "native-feature" and "mpris" in v.reason for v in violations))

    def test_workspace_and_local_sdk_feature_arrays_are_unioned(self):
        root = self.fixture(
            manifest_tail=(
                '[dependencies]\nui = { workspace = true, features = ["dmabuf-import"] }\n'
            )
        )
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["apps/sample"]\n'
            '[workspace.dependencies]\n'
            'ui = { package = "tilo-app", path = "crates/sdk/tilo-app", features = ["mpris"] }\n',
            encoding="utf-8",
        )
        violations, _ = contract.audit(root)
        features = [v.reason for v in violations if v.rule == "native-feature"]
        self.assertEqual(len(features), 2)
        self.assertTrue(any("mpris" in reason for reason in features))
        self.assertTrue(any("dmabuf-import" in reason for reason in features))

    def test_semantic_portal_dependency_is_allowed(self):
        root = self.fixture(manifest_tail='[dependencies]\nportal = "1"\n')
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

    def test_nested_profile_tool_manifest_and_sources_are_checked(self):
        root = self.fixture()
        tool = root / "apps/sample/tools/profile"
        (tool / "src").mkdir(parents=True)
        (tool / "Cargo.toml").write_text(
            '[package]\nname = "profile-tool"\nversion = "0.1.0"\n[build-dependencies]\nzbus = "4"\n',
            encoding="utf-8",
        )
        (tool / "src/main.rs").write_text("fn main() { windows_sys::Win32::foo(); }\n", encoding="utf-8")
        violations, count = contract.audit(root)
        self.assertEqual(count, 2)
        self.assertTrue(any(v.path.name == "main.rs" for v in violations))
        self.assertTrue(any(v.path.name == "Cargo.toml" for v in violations))

    def test_exact_baseline_entry_accepts_the_attributed_site(self):
        root = self.fixture("fn leak() { nix::unistd::getpid(); }\n")
        self.write_baseline(root, [self.one_inventory_entry(root)])
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

    def test_baseline_does_not_cover_extra_occurrence_at_same_site(self):
        root = self.fixture("fn leak() { nix::a(); }\n")
        entry = self.one_inventory_entry(root)
        self.write_baseline(root, [entry])
        (root / "apps/sample/src/lib.rs").write_text(
            "fn leak() { nix::a(); }\nfn leak() { nix::a(); }\n", encoding="utf-8"
        )
        violations, _ = contract.audit(root)
        self.assertIn("new occurrence", self.reasons(violations))

    def test_baseline_does_not_cover_same_rule_at_different_site(self):
        root = self.fixture("fn first() { nix::a(); }\n")
        self.write_baseline(root, [self.one_inventory_entry(root)])
        (root / "apps/sample/src/lib.rs").write_text(
            "fn first() { nix::a(); }\nfn second() { nix::a(); }\n", encoding="utf-8"
        )
        violations, _ = contract.audit(root)
        self.assertTrue(any(v.line == 2 and v.rule == "native-api" for v in violations))

    def test_moved_line_preserves_the_normalized_site_identity(self):
        root = self.fixture("fn leak() { nix::a(); }\n")
        self.write_baseline(root, [self.one_inventory_entry(root)])
        (root / "apps/sample/src/lib.rs").write_text(
            "\nfn leak() { nix::a(); }\n", encoding="utf-8"
        )
        violations, _ = contract.audit(root)
        self.assertEqual(violations, [])

    def test_deleted_site_or_file_reports_stale_exception(self):
        for delete_file in (False, True):
            with self.subTest(delete_file=delete_file):
                root = self.fixture("fn leak() { nix::a(); }\n", app_name=f"sample{int(delete_file)}")
                self.write_baseline(root, [self.one_inventory_entry(root)])
                source = next((root / "apps").glob("*/src/lib.rs"))
                if delete_file:
                    source.unlink()
                else:
                    source.write_text("pub fn portable() {}\n", encoding="utf-8")
                violations, _ = contract.audit(root)
                self.assertIn("stale exception", self.reasons(violations))

    def test_decreased_occurrence_count_reports_stale_exception(self):
        root = self.fixture("fn leak() { nix::a(); }\nfn leak() { nix::a(); }\n")
        entry = self.one_inventory_entry(root)
        self.assertEqual(entry["count"], 2)
        self.write_baseline(root, [entry])
        (root / "apps/sample/src/lib.rs").write_text("fn leak() { nix::a(); }\n", encoding="utf-8")
        violations, _ = contract.audit(root)
        self.assertIn("decreased from 2 to 1", self.reasons(violations))

    def test_invalid_finding_or_task_ids_are_rejected(self):
        root = self.fixture("fn leak() { nix::a(); }\n")
        entry = self.one_inventory_entry(root)
        for field, invalid in (("finding_id", "A99-99"), ("removal_task", "NOPE")):
            with self.subTest(field=field):
                bad = dict(entry)
                bad[field] = invalid
                self.write_baseline(root, [bad])
                violations, _ = contract.audit(root)
                self.assertIn(
                    "does not exist in app_platform_findings.json",
                    self.reasons(violations),
                )

    def test_malformed_and_duplicate_baseline_entries_are_rejected(self):
        root = self.fixture("fn leak() { nix::a(); }\n")
        entry = self.one_inventory_entry(root)
        malformed = dict(entry)
        malformed.pop("rationale")
        self.write_baseline(root, [malformed])
        violations, _ = contract.audit(root)
        self.assertIn("malformed baseline entry", self.reasons(violations))

        self.write_baseline(root, [entry, entry])
        violations, _ = contract.audit(root)
        self.assertIn("duplicate baseline entry", self.reasons(violations))

    def test_cli_fails_on_violation_and_inventory_does_not_modify_files(self):
        root = self.fixture("fn leak() { nix::a(); }\n")
        before = {path: path.read_bytes() for path in root.rglob("*") if path.is_file()}
        failed = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root)],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("native-api", failed.stdout)

        inventoried = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root), "--inventory"],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(inventoried.returncode, 0, inventoried.stderr)
        record = json.loads(inventoried.stdout)
        self.assertEqual(record["checked_apps"], 1)
        self.assertEqual(record["entries"][0]["rule"], "native-api")
        after = {path: path.read_bytes() for path in root.rglob("*") if path.is_file()}
        self.assertEqual(after, before)

    def test_missing_apps_tree_is_a_configuration_failure(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.write_register(root)
        violations, count = contract.audit(root)
        self.assertEqual(count, 0)
        self.assertIn("no application packages", self.reasons(violations))


if __name__ == "__main__":
    unittest.main()
