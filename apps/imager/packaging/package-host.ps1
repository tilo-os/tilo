$ErrorActionPreference = "Stop"
$Out = if ($args.Count -gt 0) { $args[0] } else { "dist/qos-imager" }
if ($env:QOS_REQUIRE_SIGNING -eq "1" -and -not $env:QOS_WINDOWS_CERT_PFX) {
    throw "QOS_WINDOWS_CERT_PFX is required for a release build"
}
if ($env:QOS_WINDOWS_CERT_PFX -and -not $env:QOS_WINDOWS_CERT_PASSWORD) {
    throw "QOS_WINDOWS_CERT_PASSWORD is required when signing"
}
cargo build --locked --release -p imager
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Bundle = Join-Path $Out "qos-imager-windows"
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item "target/release/imager.exe" (Join-Path $Bundle "qos-imager.exe")
Copy-Item "docs/installer-walkthrough.md" (Join-Path $Bundle "README.md")
Copy-Item "apps/imager/packaging/windows/Start-qOS-Imager.ps1" $Bundle
$Exe = Join-Path $Bundle "qos-imager.exe"
if ($env:QOS_WINDOWS_CERT_PFX) {
    $SignTool = (Get-Command signtool.exe -ErrorAction Stop).Source
    & $SignTool sign /fd SHA256 /td SHA256 /tr "http://timestamp.digicert.com" `
        /f $env:QOS_WINDOWS_CERT_PFX /p $env:QOS_WINDOWS_CERT_PASSWORD $Exe
    if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed" }
    & $SignTool verify /pa /v $Exe
    if ($LASTEXITCODE -ne 0) { throw "Authenticode verification failed" }
} elseif ($env:QOS_REQUIRE_SIGNING -eq "1") {
    throw "QOS_WINDOWS_CERT_PFX is required for a release build"
}
$Zip = Join-Path $Out "qos-imager-windows.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Bundle/*" -DestinationPath $Zip
$Hash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
$Digest = "$Hash  $([System.IO.Path]::GetFileName($Zip))`n"
[System.IO.File]::WriteAllText("$Zip.sha256", $Digest, [System.Text.UTF8Encoding]::new($false))
