# Read-only qualification plan. Does not build, download, install or publish.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string[]]$Apps,
    [Parameter(Mandatory)][ValidateSet('x86_64', 'aarch64')][string]$Architecture
)
$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$tokens = $null
$errors = $null
$installer = Join-Path $PSScriptRoot 'windows/install-apps.ps1'
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Native Windows installer has syntax errors.' }
$parameter = @($ast.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -eq 'Apps' })
if ($parameter.Count -ne 1) { throw 'Cannot observe the installer application contract.' }
$validation = @($parameter[0].Attributes | Where-Object { $_.TypeName.Name -eq 'ValidateSet' })
if ($validation.Count -ne 1) { throw 'Missing explicit installer application allowlist.' }
$supported = @($validation[0].PositionalArguments | ForEach-Object {
    if ($_ -isnot [Management.Automation.Language.StringConstantExpressionAst]) {
        throw 'Installer allowlist must contain literal application names.'
    }
    $_.Value
})
$selected = @($Apps | Sort-Object -Unique)
if (-not $selected.Count) { throw 'Select at least one application.' }
foreach ($app in $selected) {
    if ($app -cnotin $supported) { throw "Application is outside the native Windows installer contract: $app" }
    if (-not (Test-Path -LiteralPath (Join-Path $repository "apps/$app/Cargo.toml") -PathType Leaf)) {
        throw "Missing selected application manifest: $app"
    }
}
. (Join-Path $PSScriptRoot 'windows/app-build-plan.ps1')
$buildPlan = Get-TiloWindowsAppBuildPlan -Apps $selected
$builds = @($buildPlan.applicationBuild)
if ($null -ne $buildPlan.notificationBuild) { $builds += $buildPlan.notificationBuild }
$inputs = @('Cargo.toml', 'Cargo.lock', '.cargo/config.toml', 'rust-toolchain.toml', 'tools/windows/install-apps.ps1',
    'tools/windows/app-build-plan.ps1', 'tools/windows/install-dxc.ps1', 'tools/windows/install-pdfium.ps1',
    'tools/windows/install-gstreamer.ps1', 'tools/windows/gstreamer-crypto.ps1')
$inputs += @($selected | ForEach-Object { "apps/$_/Cargo.toml" })
[ordered]@{
    schema = 1
    product = 'windows-native-apps'
    architecture = $Architecture
    nativeRustHost = "$Architecture-pc-windows-msvc"
    runnerLabel = "imager-windows-$Architecture"
    runnerAvailability = 'must-be-verified-before-dispatch'
    applications = $selected
    installerAllowlistCount = $supported.Count
    cargo = $buildPlan.applicationBuild
    builds = $builds
    runtime = $buildPlan.runtime
    qualification = @(
        'Verify physical processor architecture and native rustc host; reject emulation or mixed hosts.',
        'Require PowerShell 7, pinned Rust/MSVC, CapnProto, LLVM readobj and native build dependencies.',
        'Build selected packages together to preserve their shared Cargo feature graph.',
        'Run the existing installer only in a disposable Windows user/VM, preserving PE import and runtime checksum checks.',
        'Verify Start entries, native launch/actions, notifications where applicable and media/runtime relocation.',
        'Retain binary/runtime checksums and exact source/toolchain/native dependency receipts before designing publication.'
    )
    cache = [ordered]@{
        reusable = $false
        scope = 'windows-native-architecture-toolchain-and-native-dependency-identity'
        inputs = @($inputs | ForEach-Object {
            [ordered]@{ path = $_; sha256 = (Get-FileHash -LiteralPath (Join-Path $repository $_) -Algorithm SHA256).Hash.ToLowerInvariant() }
        })
        limitation = 'Input inventory only; not a reusable artifact key. Complete source/dependency closure, resolved Cargo features, compiler/SDK identity and actual native runtime hashes still need qualification.'
    }
    publicationEnabled = $false
} | ConvertTo-Json -Depth 8
