$ErrorActionPreference = "Stop"
$Out = if ($args.Count -gt 0) { $args[0] } else { "dist/qos-imager" }
$Version = if ($env:QOS_VERSION) { $env:QOS_VERSION -replace '^imager-v', '' -replace '^v', '' } else { "0.1.0" }
$RequireNative = $env:QOS_REQUIRE_NATIVE_PACKAGES -eq "1"
if ($env:QOS_REQUIRE_SIGNING -eq "1" -and -not $env:QOS_WINDOWS_CERT_PFX -and -not $env:QOS_WINDOWS_SIGNER) {
    throw "QOS_WINDOWS_SIGNER or QOS_WINDOWS_CERT_PFX is required for a release build"
}
if ($env:QOS_WINDOWS_CERT_PFX -and -not $env:QOS_WINDOWS_CERT_PASSWORD) {
    throw "QOS_WINDOWS_CERT_PASSWORD is required when signing"
}

function Invoke-qOSCodeSign([string] $Path) {
    if (-not $env:QOS_WINDOWS_SIGNER -and -not $env:QOS_WINDOWS_CERT_PFX) {
        return
    }
    $SignTool = (Get-Command signtool.exe -ErrorAction Stop).Source
    if ($env:QOS_WINDOWS_SIGNER) {
        & $env:QOS_WINDOWS_SIGNER $Path
        if ($LASTEXITCODE -ne 0) { throw "external Authenticode signer failed for $Path" }
    } elseif ($env:QOS_WINDOWS_CERT_PFX) {
        & $SignTool sign /fd SHA256 /td SHA256 /tr "http://timestamp.digicert.com" `
            /f $env:QOS_WINDOWS_CERT_PFX /p $env:QOS_WINDOWS_CERT_PASSWORD $Path
        if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed for $Path" }
    }
    & $SignTool verify /pa /v $Path
    if ($LASTEXITCODE -ne 0) { throw "Authenticode verification failed for $Path" }
}

cargo build --locked --release -p imager
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Bundle = Join-Path $Out "qos-imager-windows-x86_64"
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item "target/release/imager.exe" (Join-Path $Bundle "qos-imager.exe")
Copy-Item "docs/installer-walkthrough.md" (Join-Path $Bundle "README.md")
Copy-Item "apps/imager/packaging/windows/Start-qOS-Imager.ps1" $Bundle
$Exe = Join-Path $Bundle "qos-imager.exe"
Invoke-qOSCodeSign $Exe
$Standalone = Join-Path $Out "qos-imager-windows-x86_64.exe"
Copy-Item $Exe $Standalone
$Zip = Join-Path $Out "qos-imager-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Bundle/*" -DestinationPath $Zip
$Hash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
$Digest = "$Hash  $([System.IO.Path]::GetFileName($Zip))`n"
[System.IO.File]::WriteAllText("$Zip.sha256", $Digest, [System.Text.UTF8Encoding]::new($false))

$Iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($Iscc) {
    & $Iscc.Source "/DMyOut=$Out" "/DMyVersion=$Version" `
        "apps/imager/packaging/windows/qos-imager.iss"
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup packaging failed" }
    $Installer = Join-Path $Out "qos-imager-setup-windows-x86_64.exe"
    Invoke-qOSCodeSign $Installer
    $InstallerHash = (Get-FileHash -Algorithm SHA256 $Installer).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText(
        "$Installer.sha256",
        "$InstallerHash  $([System.IO.Path]::GetFileName($Installer))`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    if ($env:QOS_RELEASE_BASE_URL) {
        $ManifestDir = Join-Path $Out "winget"
        New-Item -ItemType Directory -Force -Path $ManifestDir | Out-Null
        $Url = "$($env:QOS_RELEASE_BASE_URL.TrimEnd('/'))/$([System.IO.Path]::GetFileName($Installer))"
        $Manifest = @"
PackageIdentifier: qOS.Imager
PackageVersion: $Version
InstallerType: inno
Installers:
  - Architecture: x64
    InstallerUrl: $Url
    InstallerSha256: $($InstallerHash.ToUpperInvariant())
ManifestType: installer
ManifestVersion: 1.9.0
"@
        [System.IO.File]::WriteAllText(
            (Join-Path $ManifestDir "qOS.Imager.installer.yaml"),
            $Manifest,
            [System.Text.UTF8Encoding]::new($false)
        )
        $VersionManifest = @"
PackageIdentifier: qOS.Imager
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
"@
        $LocaleManifest = @"
PackageIdentifier: qOS.Imager
PackageVersion: $Version
PackageLocale: en-US
Publisher: qOS Project
PackageName: qOS Imager
License: AGPL-3.0-only
ShortDescription: Create and verify customised qOS installation media.
PackageUrl: https://code.podesta.ai/qos/qos
ManifestType: defaultLocale
ManifestVersion: 1.9.0
"@
        [System.IO.File]::WriteAllText(
            (Join-Path $ManifestDir "qOS.Imager.yaml"),
            $VersionManifest,
            [System.Text.UTF8Encoding]::new($false)
        )
        [System.IO.File]::WriteAllText(
            (Join-Path $ManifestDir "qOS.Imager.locale.en-US.yaml"),
            $LocaleManifest,
            [System.Text.UTF8Encoding]::new($false)
        )
    }
} elseif ($RequireNative) {
    throw "ISCC.exe (Inno Setup 6) is required for the release installer"
}

$StandaloneHash = (Get-FileHash -Algorithm SHA256 $Standalone).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText(
    "$Standalone.sha256",
    "$StandaloneHash  $([System.IO.Path]::GetFileName($Standalone))`n",
    [System.Text.UTF8Encoding]::new($false)
)
