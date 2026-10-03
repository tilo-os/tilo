# Run on Windows: pwsh -NoProfile -File os/tests/vm-windows-tool-resolver-test.ps1
# Load the production function through its AST without starting or stopping a VM.
param([string]$Source = (Join-Path $PSScriptRoot '../dev/vm-windows.ps1'))
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This test requires native Windows command resolution'
}
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    [IO.Path]::GetFullPath($Source), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw "PowerShell parse errors: $parseErrors" }
$functions = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Resolve-Tool'
}, $true))
if ($functions.Count -ne 1) { throw 'Expected exactly one production Resolve-Tool function' }
. ([scriptblock]::Create($functions[0].Extent.Text))

$saved = @{}
foreach ($name in @('PATH', 'USERPROFILE', 'TILO_QEMU_IMG', 'TILO_QEMU_SYSTEM')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$sandbox = Join-Path $tempRoot ('tilo-tool-resolver-' + [guid]::NewGuid().ToString('N'))
$pathDir = Join-Path $sandbox 'path'
$overrideDir = Join-Path $sandbox 'overrides'
$fallbackDir = Join-Path $sandbox 'profile/msys64/clangarm64/bin'
$tools = @('qemu-img', 'qemu-system-aarch64', 'magick')
function Assert-Resolved([string]$name, [string]$expected) {
    $actual = Resolve-Tool $name
    if ($actual -cne $expected) { throw "$name resolved to $actual; expected $expected" }
}
try {
    foreach ($dir in @($pathDir, $overrideDir, $fallbackDir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        foreach ($tool in $tools) { [IO.File]::WriteAllText((Join-Path $dir "$tool.exe"), 'resolver fixture; never executed') }
    }
    $env:USERPROFILE = Join-Path $sandbox 'profile'
    $cases = 0
    foreach ($useImg in @($false, $true)) {
        foreach ($useSystem in @($false, $true)) {
            $env:TILO_QEMU_IMG = if ($useImg) { Join-Path $overrideDir 'qemu-img.exe' } else { $null }
            $env:TILO_QEMU_SYSTEM = if ($useSystem) { Join-Path $overrideDir 'qemu-system-aarch64.exe' } else { $null }
            foreach ($usePath in @($true, $false)) {
                $env:PATH = if ($usePath) { $pathDir } else { Join-Path $sandbox 'empty-path' }
                $defaultDir = if ($usePath) { $pathDir } else { $fallbackDir }
                Assert-Resolved 'qemu-img' (Join-Path $(if ($useImg) { $overrideDir } else { $defaultDir }) 'qemu-img.exe')
                Assert-Resolved 'qemu-system-aarch64' (Join-Path $(if ($useSystem) { $overrideDir } else { $defaultDir }) 'qemu-system-aarch64.exe')
                Assert-Resolved 'magick' (Join-Path $defaultDir 'magick.exe')
                $cases++
            }
        }
    }
    # An invalid QEMU override must fail for QEMU but never affect ImageMagick.
    $env:PATH = $pathDir
    $env:TILO_QEMU_SYSTEM = Join-Path $sandbox 'missing-qemu.exe'
    Assert-Resolved 'magick' (Join-Path $pathDir 'magick.exe')
    $rejected = $false
    try { Resolve-Tool 'qemu-system-aarch64' | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw 'Missing explicit QEMU override was silently ignored' }
    Write-Output "PASS: $cases override/PATH/fallback combinations and invalid override isolation"
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    # Delete only this invocation's generated directory beneath Windows Temp.
    $resolved = [IO.Path]::GetFullPath($sandbox)
    if (!$resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notmatch '^tilo-tool-resolver-[0-9a-f]{32}$') {
        throw 'Refusing cleanup outside the generated test sandbox'
    }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
