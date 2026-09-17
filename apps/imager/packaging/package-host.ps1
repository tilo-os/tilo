$ErrorActionPreference = "Stop"
$Out = if ($args.Count -gt 0) { $args[0] } else { "dist/tilo-imager" }
$Version = if ($env:TILO_VERSION) { $env:TILO_VERSION -replace '^imager-v', '' -replace '^v', '' } else { "0.1.0" }
$ReleaseRepository = if ($env:TILO_RELEASE_REPOSITORY) { $env:TILO_RELEASE_REPOSITORY } else { "tilo/releases" }
$ReleaseServer = if ($env:GITHUB_SERVER_URL) { $env:GITHUB_SERVER_URL.TrimEnd('/') } else { "https://code.sandbox.example" }
if ($ReleaseRepository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    throw "release repository must be OWNER/REPOSITORY"
}
if ($ReleaseServer -notmatch '^https://[^/]+$') { throw "release server must be an HTTPS origin" }
$ReleasePage = "$ReleaseServer/$ReleaseRepository"
$RequireNative = $env:TILO_REQUIRE_NATIVE_PACKAGES -eq "1"
if ($env:TILO_REQUIRE_SIGNING -eq "1" -and -not $env:TILO_WINDOWS_CERT_PFX -and -not $env:TILO_WINDOWS_SIGNER) {
    throw "TILO_WINDOWS_SIGNER or TILO_WINDOWS_CERT_PFX is required for a release build"
}
if ($env:TILO_WINDOWS_CERT_PFX -and -not $env:TILO_WINDOWS_CERT_PASSWORD) {
    throw "TILO_WINDOWS_CERT_PASSWORD is required when signing"
}
if ($env:TILO_REQUIRE_SIGNING -eq "1" -and -not $env:TILO_WINDOWS_EXPECTED_PUBLISHER) {
    throw "TILO_WINDOWS_EXPECTED_PUBLISHER is required for a release build"
}

function Invoke-TiloCodeSign([string] $Path) {
    if (-not $env:TILO_WINDOWS_SIGNER -and -not $env:TILO_WINDOWS_CERT_PFX) {
        return
    }
    $SignTool = (Get-Command signtool.exe -ErrorAction Stop).Source
    if ($env:TILO_WINDOWS_SIGNER) {
        & $env:TILO_WINDOWS_SIGNER $Path
        if ($LASTEXITCODE -ne 0) { throw "external Authenticode signer failed for $Path" }
    } elseif ($env:TILO_WINDOWS_CERT_PFX) {
        & $SignTool sign /fd SHA256 /td SHA256 /tr "http://timestamp.digicert.com" `
            /f $env:TILO_WINDOWS_CERT_PFX /p $env:TILO_WINDOWS_CERT_PASSWORD $Path
        if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed for $Path" }
    }
    & $SignTool verify /pa /v $Path
    if ($LASTEXITCODE -ne 0) { throw "Authenticode verification failed for $Path" }
    $Signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($Signature.Status -ne 'Valid' -or -not $Signature.SignerCertificate) {
        throw "Authenticode did not produce a valid signer certificate for $Path"
    }
    if ($env:TILO_WINDOWS_EXPECTED_PUBLISHER -and
        $Signature.SignerCertificate.Subject -cne $env:TILO_WINDOWS_EXPECTED_PUBLISHER) {
        throw "Authenticode publisher identity does not match protected release policy"
    }
}

cargo build --locked --release -p imager --features host-media-writer
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Bundle = Join-Path $Out "tilo-imager-windows-x86_64"
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item "target/release/imager.exe" (Join-Path $Bundle "tilo-imager.exe")
Copy-Item "docs/installer-walkthrough.md" (Join-Path $Bundle "README.md")
Copy-Item "apps/imager/packaging/windows/Start-Tilo-Imager.ps1" $Bundle
$Exe = Join-Path $Bundle "tilo-imager.exe"
Invoke-TiloCodeSign $Exe
$Standalone = Join-Path $Out "tilo-imager-windows-x86_64.exe"
Copy-Item $Exe $Standalone
$Zip = Join-Path $Out "tilo-imager-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Bundle/*" -DestinationPath $Zip
$Hash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
$Digest = "$Hash  $([System.IO.Path]::GetFileName($Zip))`n"
[System.IO.File]::WriteAllText("$Zip.sha256", $Digest, [System.Text.UTF8Encoding]::new($false))

$Iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($Iscc) {
    & $Iscc.Source "/DMyOut=$Out" "/DMyVersion=$Version" `
        "apps/imager/packaging/windows/tilo-imager.iss"
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup packaging failed" }
    $Installer = Join-Path $Out "tilo-imager-setup-windows-x86_64.exe"
    Invoke-TiloCodeSign $Installer
    $InstallerHash = (Get-FileHash -Algorithm SHA256 $Installer).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText(
        "$Installer.sha256",
        "$InstallerHash  $([System.IO.Path]::GetFileName($Installer))`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    if ($env:TILO_RELEASE_BASE_URL) {
        $ManifestDir = Join-Path $Out "winget"
        New-Item -ItemType Directory -Force -Path $ManifestDir | Out-Null
        $Url = "$($env:TILO_RELEASE_BASE_URL.TrimEnd('/'))/$([System.IO.Path]::GetFileName($Installer))"
        $Manifest = @"
PackageIdentifier: Tilo.Imager
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
            (Join-Path $ManifestDir "Tilo.Imager.installer.yaml"),
            $Manifest,
            [System.Text.UTF8Encoding]::new($false)
        )
        $VersionManifest = @"
PackageIdentifier: Tilo.Imager
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
"@
        $LocaleManifest = @"
PackageIdentifier: Tilo.Imager
PackageVersion: $Version
PackageLocale: en-US
Publisher: Tilo Project
PackageName: Tilo Imager
License: AGPL-3.0-only
ShortDescription: Create and verify customised Tilo installation media.
PackageUrl: $ReleasePage
ManifestType: defaultLocale
ManifestVersion: 1.9.0
"@
        [System.IO.File]::WriteAllText(
            (Join-Path $ManifestDir "Tilo.Imager.yaml"),
            $VersionManifest,
            [System.Text.UTF8Encoding]::new($false)
        )
        [System.IO.File]::WriteAllText(
            (Join-Path $ManifestDir "Tilo.Imager.locale.en-US.yaml"),
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
