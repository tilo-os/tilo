# Exercise real archive and digest creation with synthetic PE headers. These
# fixtures are never executed and no compiler, signer or installer is invoked.
$ErrorActionPreference = "Stop"
$Packager = Join-Path $PSScriptRoot "package-host.ps1"
$Fixture = Join-Path ([IO.Path]::GetTempPath()) "tilo-package-arch-$([guid]::NewGuid())"
$Saved = @{}
foreach ($Name in @("TILO_REQUIRE_SIGNING", "TILO_WINDOWS_CERT_PFX", "TILO_WINDOWS_SIGNER", "TILO_REQUIRE_NATIVE_PACKAGES", "TILO_RELEASE_BASE_URL")) {
    $Saved[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
    [Environment]::SetEnvironmentVariable($Name, $null, "Process")
}
function cargo { $global:LASTEXITCODE = 0 }
function Get-Command {
    param([string] $Name, $ErrorAction)
    if ($Name -eq "ISCC.exe") { return $null }
    Microsoft.PowerShell.Core\Get-Command $Name -ErrorAction $ErrorAction
}
function Write-Fixture([string] $Architecture, [uint16] $Machine) {
    $Directory = "target/$Architecture-pc-windows-msvc/release"
    New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    $Bytes = [byte[]]::new(128)
    $Bytes[0] = 0x4D; $Bytes[1] = 0x5A; $Bytes[0x3C] = 0x40
    $Bytes[64] = 0x50; $Bytes[65] = 0x45
    [BitConverter]::GetBytes($Machine).CopyTo($Bytes, 68)
    [IO.File]::WriteAllBytes((Join-Path (Get-Location) "$Directory/imager.exe"), $Bytes)
}
New-Item -ItemType Directory -Path $Fixture | Out-Null
Push-Location $Fixture
try {
    New-Item -ItemType Directory -Force -Path "docs", "apps/imager/packaging/windows" | Out-Null
    Set-Content docs/installer-walkthrough.md "package fixture"
    Set-Content apps/imager/packaging/windows/Start-Tilo-Imager.ps1 "# fixture"
    foreach ($Architecture in @("x86_64", "aarch64")) {
        $Machine = if ($Architecture -eq "aarch64") { 0xAA64 } else { 0x8664 }
        Write-Fixture $Architecture $Machine
        & $Packager "out/$Architecture" -Architecture $Architecture
        $Zip = "out/$Architecture/tilo-imager-windows-$Architecture.zip"
        if (-not (Test-Path -LiteralPath $Zip)) { throw "Missing $Architecture archive" }
        $Digest = (Get-Content -LiteralPath "$Zip.sha256").Split(' ')[0]
        if ($Digest -ne (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLowerInvariant()) {
            throw "Incorrect $Architecture archive digest"
        }
        $Standalone = "out/$Architecture/tilo-imager-windows-$Architecture.exe"
        if ((Get-FileHash -LiteralPath $Standalone).Hash -ne
            (Get-FileHash -LiteralPath "target/$Architecture-pc-windows-msvc/release/imager.exe").Hash) {
            throw "Packaged executable differs from input"
        }
    }
    Write-Fixture "aarch64" 0x8664
    $Failure = $null
    try { & $Packager "out/mismatched" -Architecture aarch64 } catch { $Failure = $_ }
    if (-not $Failure -or $Failure.Exception.Message -ne "Executable architecture does not match aarch64 package") {
        throw "Mislabeled x86 executable was not rejected"
    }
    if (Test-Path out/mismatched) { throw "Mismatched binary created package output" }
    Write-Output "PASS: both native package names and digests; mismatched PE architecture rejected."
} finally {
    Pop-Location
    foreach ($Name in $Saved.Keys) { [Environment]::SetEnvironmentVariable($Name, $Saved[$Name], "Process") }
    $ResolvedFixture = [IO.Path]::GetFullPath($Fixture)
    $TempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $ResolvedFixture.StartsWith($TempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove fixture outside the temporary directory"
    }
    Remove-Item -LiteralPath $ResolvedFixture -Recurse -Force
}
