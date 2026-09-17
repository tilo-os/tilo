# Install the shared native PDF renderer, independently of the application binaries.
[CmdletBinding()]
param(
    [string]$ArchivePath,
    [ValidateSet('arm64', 'x64', 'x86')]
    [string]$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Run this installer from PowerShell on Windows.' }
$version = '8057'
# Published asset digests from the upstream chromium/8057 GitHub release.
$digests = @{
    arm64 = '30070ce592b86da476601d07ca2742b26c4761095d4068121465088fe8979996'
    x64 = 'e307d519e42f2e69b1b531f0c2a32dffcdf3891ec0eba60328ba51a57cec01ed'
    x86 = '242aa2959be60fb5224011130e98fab71e83330f1bad4f728f5d78495624251f'
}
$tiloDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo'
$runtimeDirectory = Join-Path $tiloDirectory "Runtime/PDFium/$version/$Architecture"
$applicationDirectory = Join-Path $tiloDirectory 'Applications'
New-Item -ItemType Directory -Force -Path $runtimeDirectory, $applicationDirectory | Out-Null
if (-not $ArchivePath) {
    $ArchivePath = Join-Path $runtimeDirectory "pdfium-win-$Architecture.tgz"
    if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
        $url = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$version/pdfium-win-$Architecture.tgz"
        Invoke-WebRequest -Uri $url -OutFile $ArchivePath
    }
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash -ne $digests[$Architecture]) {
    throw "PDFium $version $Architecture archive checksum does not match the published release."
}
# Only extract the known runtime and license entries from the verified archive.
& tar -xf $ArchivePath -C $runtimeDirectory bin/pdfium.dll LICENSE VERSION licenses
if ($LASTEXITCODE -ne 0) { throw 'Could not extract the PDFium runtime.' }
$library = Join-Path $runtimeDirectory 'bin/pdfium.dll'
$destination = Join-Path $applicationDirectory 'pdfium.dll'
Copy-Item -LiteralPath $library -Destination $destination -Force
$licenseDirectory = Join-Path $applicationDirectory "licenses/PDFium-$version"
New-Item -ItemType Directory -Force -Path $licenseDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $runtimeDirectory 'LICENSE'), (Join-Path $runtimeDirectory 'VERSION') -Destination $licenseDirectory -Force
Copy-Item -LiteralPath (Join-Path $runtimeDirectory 'licenses') -Destination $licenseDirectory -Recurse -Force
[pscustomobject]@{ Version = $version; Architecture = $Architecture; Library = $destination; Licenses = $licenseDirectory }
