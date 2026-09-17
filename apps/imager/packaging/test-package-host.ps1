# Run from the repository root. Exercises the real packager without compiling,
# signing, publishing, or touching any disk image.
$ErrorActionPreference = "Stop"
$OutputPath = Join-Path ([System.IO.Path]::GetTempPath()) "tilo-imager-package-test-$([guid]::NewGuid())"
$SavedSigning = @{}
$SigningVariables = @("TILO_REQUIRE_SIGNING", "TILO_WINDOWS_CERT_PFX", "TILO_WINDOWS_SIGNER")
foreach ($Name in $SigningVariables) {
    $SavedSigning[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
    [Environment]::SetEnvironmentVariable($Name, $null, "Process")
}
$BuildArguments = [System.Collections.Generic.List[string]]::new()
function cargo {
    foreach ($Argument in $args) { $BuildArguments.Add([string]$Argument) }
    $global:LASTEXITCODE = 99
}

try {
  foreach ($Architecture in @("x86_64", "aarch64")) {
    $BuildArguments.Clear()
    $Failure = $null
    try {
        & "$PSScriptRoot/package-host.ps1" $OutputPath -Architecture $Architecture
    } catch {
        $Failure = $_
    }
    if (-not $Failure -or $Failure.Exception.Message -ne "Tilo Imager build failed for $Architecture-pc-windows-msvc") {
        throw "The packager must report the failed build before copying or signing any binary."
    }
    $TargetIndex = $BuildArguments.IndexOf("--target")
    if ($TargetIndex -lt 0 -or $BuildArguments[$TargetIndex + 1] -ne "$Architecture-pc-windows-msvc") {
        throw "The Windows package must explicitly compile the architecture in its filename."
    }
    if (Test-Path -LiteralPath $OutputPath) {
        throw "A failed build must not create a package directory or reuse a stale binary."
    }
    Write-Output "PASS: explicit $Architecture build target; failed build stops before packaging."
  }
} finally {
    foreach ($Name in $SigningVariables) {
        [Environment]::SetEnvironmentVariable($Name, $SavedSigning[$Name], "Process")
    }
}
