# This script has never been run: no Windows laptop or native runner is currently available.
# Disabled future work only, not qualification evidence. Its workflow must stay disabled.
# Future prerequisite: an owned disposable NTFS VM with no user data, provisioned
# through runners/imager-ephemeral-windows.ps1 and destroyed after its one job.
# No physical drives, firmware, real profiles, credentials or provider accounts.
# Not full destructive Windows replacement or real-provider OneDrive/Steam qualification;
# those require further dedicated fixtures. No Wine or cross-compiled evidence.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64' -or
    [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -ne 'X64') {
    throw 'Native x86_64 Windows and native PowerShell are required'
}
# Set only in the trusted disposable image, never supplied by workflow inputs.
if ($env:TILO_OWNED_DISPOSABLE_WINDOWS -cne '1') { throw 'Owned disposable VM required' }
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if ($Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Non-administrator fixture identity required'
}
foreach ($Tool in @('git', 'rustup', 'cargo', 'rustc', 'cl.exe', 'link.exe', 'capnp', 'cmake', 'ninja')) {
    Get-Command $Tool -CommandType Application -ErrorAction Stop | Out-Null
}
if (-not $env:RUNNER_TEMP -or -not [IO.Path]::IsPathFullyQualified($env:RUNNER_TEMP)) {
    throw 'Absolute RUNNER_TEMP required'
}
$RunnerTemp = (Get-Item -LiteralPath $env:RUNNER_TEMP -Force).FullName
$Ancestor = Get-Item -LiteralPath $RunnerTemp -Force
while ($null -ne $Ancestor) {
    if ($Ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse ancestor refused' }
    $Ancestor = $Ancestor.Parent
}
$Volume = Get-Volume -FilePath $RunnerTemp -ErrorAction Stop
if ($Volume.FileSystemType -ne 'NTFS' -or $Volume.DriveType -ne 'Fixed') {
    throw 'Disposable local NTFS fixture storage required'
}
$FixtureRoot = Join-Path $RunnerTemp ('imager-private-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $FixtureRoot -ErrorAction Stop | Out-Null
$Acl = [Security.AccessControl.DirectorySecurity]::new()
$Acl.SetOwner($Identity.User)
$Acl.SetAccessRuleProtection($true, $false)
$Rule = [Security.AccessControl.FileSystemAccessRule]::new(
    $Identity.User, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
$Acl.AddAccessRule($Rule)
Set-Acl -LiteralPath $FixtureRoot -AclObject $Acl
$ObservedAcl = Get-Acl -LiteralPath $FixtureRoot
if (-not $ObservedAcl.AreAccessRulesProtected -or
    $ObservedAcl.GetOwner([Security.Principal.SecurityIdentifier]).Value -cne $Identity.User.Value) {
    throw 'Private runner-owned fixture ACL required'
}
foreach ($Entry in $ObservedAcl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
    if ($Entry.AccessControlType -eq 'Allow' -and $Entry.IdentityReference.Value -cne $Identity.User.Value) {
        throw 'Fixture ACL grants another principal access'
    }
}
$env:TEMP = $FixtureRoot
$env:TMP = $FixtureRoot
$env:TMPDIR = $FixtureRoot
$Evidence = Join-Path $RunnerTemp 'imager-windows-evidence'
# Never mix evidence with an earlier run or upload fixture content.
New-Item -ItemType Directory -Path $Evidence -ErrorAction Stop | Out-Null
$Utf8 = [Text.UTF8Encoding]::new($false)

function Invoke-Bounded([string]$Name, [string]$Program, [string[]]$Arguments) {
    $Log = Join-Path $Evidence ($Name + '.log')
    $Writer = [IO.StreamWriter]::new($Log, $false, $Utf8)
    $Bytes = 0
    try {
        & $Program @Arguments 2>&1 | ForEach-Object {
            $Line = $_.ToString()
            $Bytes += $Utf8.GetByteCount($Line) + 2
            if ($Bytes -gt 2MB) { throw 'Command log exceeded 2 MiB; refusing unbounded artifacts' }
            $Writer.WriteLine($Line)
        }
        if ($LASTEXITCODE -ne 0) { throw "$Name failed; see bounded log" }
    } finally {
        $Writer.Dispose()
    }
    return $Log
}

# Read the checked-in pin; never update the lockfile or silently use another compiler.
$PinText = Get-Content -LiteralPath rust-toolchain.toml -Raw
if ($PinText -notmatch '(?m)^channel = "([0-9]+\.[0-9]+\.[0-9]+)"$') { throw 'Exact Rust pin required' }
$Pin = $Matches[1]
$Target = 'x86_64-pc-windows-msvc'
$Toolchain = "$Pin-$Target"
Invoke-Bounded 'toolchain-install' 'rustup' @('toolchain', 'install', $Toolchain, '--profile', 'minimal') | Out-Null
$RustLog = Invoke-Bounded 'rustc' 'rustup' @('run', $Toolchain, 'rustc', '-vV')
if ((Get-Content -LiteralPath $RustLog -Raw) -notmatch "(?m)^host: $Target\r?$") {
    throw 'Native MSVC Rust host required; cross compilation is not evidence'
}
Invoke-Bounded 'source' 'git' @('rev-parse', 'HEAD') | Out-Null
@{
    scope = 'Deferred synthetic private fixtures only; not Windows replacement qualification'
    native_os = 'Windows'; architecture = 'X64'; filesystem = 'NTFS'; storage = 'Fixed'
    private_acl_verified = $true; temp_bound_to_fixture = $true; rust = $Toolchain
    os_version = [Environment]::OSVersion.Version.ToString()
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Evidence 'native.json') -Encoding utf8
Invoke-Bounded 'capnp' 'capnp' @('--version') | Out-Null
Invoke-Bounded 'cmake' 'cmake' @('--version') | Out-Null
Invoke-Bounded 'ninja' 'ninja' @('--version') | Out-Null

# Exact lib tests inspected in the production source. Never select the Linux-only
# tests/profile_transport.rs: it would select zero tests on Windows. The model
# codec tests below cover public profile transport, not Linux live extraction.
# Reparse coverage here is the conservative attribute policy, not a real cloud
# provider's reparse object lifecycle; that still needs a dedicated fixture.
$Tests = @(
    'folders::tests::windows_offline_entries_are_excluded_and_copy_never_reports_complete',
    'nofollow::tests::windows_attribute_policy_is_conservative_and_does_not_conflate_cloud_with_links',
    'windows_inventory::tests::review_preserves_unknown_mail_drafts_and_cloud_data_until_explicit_copy',
    'windows_inventory::tests::steam_external_library_is_only_a_suggestion_and_requires_its_own_grant',
    'windows_inventory::tests::bounded_native_metadata_and_steam_parser_preserve_redirected_paths',
    'account_transfer::windows_source::tests::registry_profiles_are_private_suggestions_only_for_exact_grants',
    'profile::windows_tests::production_writer_and_reader_bind_exact_private_hints_and_recovery',
    'model::tests::media_profile_serializes_migrates_and_validates_the_installer_contract',
    'model::prepared_setup_binding_tests::media_profile_carries_only_valid_ciphertext_binding_and_public_device_name'
)
for ($Index = 0; $Index -lt $Tests.Count; $Index++) {
    $Test = $Tests[$Index]
    # Inventory, account suggestions and attribute checks use their production backend modules.
    $Package = if (($Test.StartsWith('folders::') -or $Test.StartsWith('nofollow::') -or $Test.StartsWith('windows_inventory::') -or $Test.StartsWith('account_transfer::'))) { 'imager-backend' } else { 'imager' }
    $CargoArgs = @('run', $Toolchain, 'cargo', 'test', '--locked', '-p', $Package, '--lib', '--target', $Target)
    $ListLog = Invoke-Bounded "list-$Index" 'rustup' ($CargoArgs + @($Test, '--', '--exact', '--list'))
    $Selected = @(Get-Content -LiteralPath $ListLog | Where-Object { $_ -ceq "${Test}: test" })
    if ($Selected.Count -ne 1) { throw "Expected exactly one test for $Test, got $($Selected.Count)" }
    $RunLog = Invoke-Bounded "test-$Index" 'rustup' ($CargoArgs + @($Test, '--', '--exact', '--test-threads=1'))
    if ((Get-Content -LiteralPath $RunLog -Raw) -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored;') {
        throw 'Test did not execute exactly one passing, non-ignored test'
    }
}
# Fixtures are intentionally not artifacts. VM destruction, not a shared-host cleanup
# command, owns their final disposal. Upload only the fixed evidence directory.
