# Run from the repository root. Exercises the real packager without compiling,
# signing, publishing, or touching any disk image.
$ErrorActionPreference = "Stop"
$OutputPath = Join-Path ([System.IO.Path]::GetTempPath()) "qos-imager-package-test-$([guid]::NewGuid())"
$SavedSigning = @{}
$SigningVariables = @("QOS_REQUIRE_SIGNING", "QOS_WINDOWS_CERT_PFX", "QOS_WINDOWS_SIGNER")
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
    $Failure = $null
    try {
        & "$PSScriptRoot/package-host.ps1" $OutputPath
    } catch {
        $Failure = $_
    }
    if (-not $Failure -or $Failure.Exception.Message -ne "qOS Imager build failed for x86_64-pc-windows-msvc") {
        throw "The packager must report the failed build before copying or signing any binary."
    }
    $TargetIndex = $BuildArguments.IndexOf("--target")
    if ($TargetIndex -lt 0 -or $BuildArguments[$TargetIndex + 1] -ne "x86_64-pc-windows-msvc") {
        throw "The Windows package must explicitly compile the architecture in its filename."
    }
    if (Test-Path -LiteralPath $OutputPath) {
        throw "A failed build must not create a package directory or reuse a stale binary."
    }
    Write-Output "PASS: explicit x86_64 build target; failed build stops before packaging."
} finally {
    foreach ($Name in $SigningVariables) {
        [Environment]::SetEnvironmentVariable($Name, $SavedSigning[$Name], "Process")
    }
}
