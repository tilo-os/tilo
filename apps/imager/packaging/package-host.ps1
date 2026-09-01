$ErrorActionPreference = "Stop"
$Out = if ($args.Count -gt 0) { $args[0] } else { "dist/qos-imager" }
cargo build --locked --release -p imager
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Bundle = Join-Path $Out "qos-imager-windows"
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item "target/release/imager.exe" (Join-Path $Bundle "qos-imager.exe")
Copy-Item "docs/installer-walkthrough.md" (Join-Path $Bundle "README.md")
Copy-Item "apps/imager/packaging/windows/Start-qOS-Imager.ps1" $Bundle
$Zip = Join-Path $Out "qos-imager-windows.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path "$Bundle/*" -DestinationPath $Zip
$Hash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
$Digest = "$Hash  $([System.IO.Path]::GetFileName($Zip))`n"
[System.IO.File]::WriteAllText("$Zip.sha256", $Digest, [System.Text.UTF8Encoding]::new($false))
