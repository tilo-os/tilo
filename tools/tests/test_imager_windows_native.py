"""Portable contracts for the deliberately disabled future Windows fixture lane.

These inspect source, not Windows behavior, and are not qualification evidence.
"""
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".forgejo/workflows/dev-imager-windows-native.yml"
SCRIPT = "tools/ci/run-imager-windows-native.ps1"

# Exact module paths matter: checking only a function basename can hide zero tests.
SOURCES = {
    "folders::tests": "crates/platform/imager-backend/src/folders.rs",
    "nofollow::tests": "crates/platform/imager-backend/src/nofollow.rs",
    "account_transfer::windows_source::tests": "crates/platform/imager-backend/src/account_transfer/windows_source.rs",
    "windows_inventory::tests": "crates/platform/imager-backend/src/windows_inventory/tests.rs",
    "profile::windows_tests": "apps/imager/src/profile.rs",
    "model::tests": "apps/imager/src/model.rs",
    "model::prepared_setup_binding_tests": "apps/imager/src/model.rs",
}


class DeferredWindowsNative(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / WORKFLOW).read_text()
        self.script = (ROOT / SCRIPT).read_text()

    def test_disabled_manual_single_job_and_pinned_actions(self):
        self.assertTrue(self.workflow.startswith("# This workflow has never been run:"))
        self.assertTrue(self.script.startswith("# This script has never been run:"))
        for text in (self.workflow, self.script):
            self.assertIn("no Windows laptop or native runner is currently available", text)
            self.assertIn("not qualification evidence", text)
            self.assertIn("further dedicated fixtures", text)
        # Exact top-level event/job shape fails closed on new triggers or jobs.
        self.assertEqual(re.findall(r"(?m)^[a-z].*", self.workflow), [
            "name: Deferred native Windows imager fixtures (disabled)",
            "on:", "permissions:", "jobs:",
        ])
        self.assertIn("on:\n  workflow_dispatch:\npermissions:", self.workflow)
        self.assertEqual(re.findall(r"(?m)^  ([\w-]+):", self.workflow),
                         ["workflow_dispatch", "contents", "native-fixtures"])
        self.assertEqual(self.workflow.count("    if: ${{ false }}"), 1)
        self.assertIn("runs-on: imager-windows-x86_64", self.workflow)
        launcher = (ROOT / "tools/ci/runners/imager-ephemeral-windows.ps1").read_text()
        self.assertIn('9 { return "imager-windows-x86_64" }', launcher)
        self.assertEqual(re.findall(r"uses: (\S+)", self.workflow), [
            "actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
            "actions/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e",
        ])
        self.assertIn("persist-credentials: false", self.workflow)
        self.assertIn("ref: ${{ github.sha }}", self.workflow)
        self.assertNotIn("secrets.", self.workflow)
        self.assertNotIn("enable-openid-connect", self.workflow)

    def test_native_private_ntfs_preflight_and_bounded_evidence(self):
        for guard in (
            "-not $IsWindows", "::OSArchitecture -ne 'X64'", "::ProcessArchitecture -ne 'X64'",
            "$env:TILO_OWNED_DISPOSABLE_WINDOWS -cne '1'", "::Administrator",
            "[IO.Path]::IsPathFullyQualified($env:RUNNER_TEMP)",
            "[IO.FileAttributes]::ReparsePoint", "$Volume.FileSystemType -ne 'NTFS'",
            "$Volume.DriveType -ne 'Fixed'", "$Acl.SetOwner($Identity.User)",
            "$Acl.SetAccessRuleProtection($true, $false)", "$ObservedAcl.AreAccessRulesProtected",
            "$Entry.IdentityReference.Value -cne $Identity.User.Value",
            "Get-Command $Tool -CommandType Application -ErrorAction Stop",
            "$Target = 'x86_64-pc-windows-msvc'", "'rustc', '-vV'", "^host: $Target",
            "Get-Content -LiteralPath rust-toolchain.toml", "if ($LASTEXITCODE -ne 0)",
            "if ($Bytes -gt 2MB)",
        ):
            self.assertIn(guard, self.script)
        for variable in ("TEMP", "TMP", "TMPDIR"):
            self.assertIn(f"$env:{variable} = $FixtureRoot", self.script)
        self.assertIn("Join-Path $RunnerTemp ('imager-private-'", self.script)
        self.assertIn("Join-Path $RunnerTemp 'imager-windows-evidence'", self.script)
        self.assertIn("path: ${{ runner.temp }}/imager-windows-evidence/*", self.workflow)
        self.assertIn("retention-days: 3", self.workflow)
        self.assertIn("if-no-files-found: error", self.workflow)
        executable = "\n".join(line for line in self.script.splitlines() if not line.startswith("#"))
        for unsafe in ("PhysicalDrive", "Set-Disk", "Clear-Disk", "Format-Volume", "bcdedit",
                       "native_windows_profiles", "USERPROFILE", "--ignored", "continue-on-error"):
            self.assertNotIn(unsafe, executable)

    def test_exact_filters_exist_and_cannot_pass_with_zero_or_ignored_tests(self):
        block = self.script.split("$Tests = @(\n", 1)[1].split("\n)", 1)[0]
        tests = re.findall(r"'([^']+)'", block)
        self.assertEqual(len(tests), 9)
        self.assertEqual(len(set(tests)), len(tests))
        for test in tests:
            module, function = test.rsplit("::", 1)
            source = (ROOT / SOURCES[module]).read_text()
            pattern = (rf"#\[test\]\s*(?:#\[cfg\(windows\)\]\s*)?"
                       rf"(?:#\[allow\([^\]]*\)\]\s*)?fn {function}\(")
            self.assertIsNotNone(re.search(pattern, source), test)
            if not SOURCES[module].endswith("/tests.rs"):
                module_name = module.rsplit("::", 1)[1]
                self.assertIn(f"mod {module_name} {{", source)
                self.assertIn(f"fn {function}(", source.split(f"mod {module_name} {{", 1)[1])
        backend = (ROOT / "crates/platform/imager-backend/src/lib.rs").read_text()
        for module in ("folders", "nofollow", "windows_inventory", "account_transfer"):
            self.assertIn(f"pub mod {module};", backend)
        inventory = (ROOT / "crates/platform/imager-backend/src/windows_inventory.rs").read_text()
        self.assertIn("#[cfg(test)]\nmod tests;", inventory)
        accounts = (ROOT / "crates/platform/imager-backend/src/account_transfer/mod.rs").read_text()
        self.assertIn("mod windows_source;", accounts)
        folders = (ROOT / SOURCES["folders::tests"]).read_text()
        self.assertIn("#[cfg(windows)]\n    fn windows_offline_entries", folders)
        self.assertIn("FILE_ATTRIBUTE_OFFLINE", folders)
        self.assertIn("UnsupportedReparsePoint", (ROOT / SOURCES["nofollow::tests"]).read_text())
        self.assertIn("'--locked', '-p', $Package, '--lib', '--target', $Target", self.script)
        for module in ("folders", "nofollow", "windows_inventory", "account_transfer"):
            self.assertIn(f"$Test.StartsWith('{module}::')", self.script)
        self.assertIn("{ 'imager-backend' } else { 'imager' }", self.script)
        self.assertIn("@($Test, '--', '--exact', '--list')", self.script)
        self.assertIn('$_ -ceq "${Test}: test"', self.script)
        self.assertIn("if ($Selected.Count -ne 1)", self.script)
        self.assertLess(self.script.index("if ($Selected.Count -ne 1)"),
                        self.script.index('$RunLog = Invoke-Bounded'))
        self.assertIn("1 passed; 0 failed; 0 ignored;", self.script)
        self.assertNotIn("'--test', 'profile_transport'", self.script)

    def test_contract_runs_through_required_unittest_discovery(self):
        policy = (ROOT / "tools/policy/run.sh").read_text()
        self.assertIn("python3 -m unittest discover -s tools/tests -p 'test_*.py'", policy)
        groups = tomllib.loads((ROOT / "tools/ci/suites.toml").read_text())["group"]
        self.assertFalse(any(
            "tools/tests/test_imager_windows_native.py" in group["suites"]
            for group in groups
        ), "required unittest discovery already runs this contract on every change")
        for path in (WORKFLOW, SCRIPT, *SOURCES.values()):
            self.assertTrue((ROOT / path).is_file(), path)


if __name__ == "__main__":
    unittest.main()
