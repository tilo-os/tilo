# Install the shared DXC shader compiler. Without it wgpu silently falls back
# to FXC, recompiling the same UI shaders for seconds on every app launch.
[CmdletBinding()]
param(
    [string]$ArchivePath,
    [ValidateSet('arm64', 'x64', 'x86')]
    [string]$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Run this installer from PowerShell on Windows.' }
$version = '1.8.2505.1'
$archiveName = 'dxc_2025_07_14.zip'
# Digest published by Microsoft's release API, not a hash of an SDK-local DLL.
$digest = '9ad895a6b039e3a8f8c22a1009f866800b840a74b50db9218d13319e215ea8a4'
$tiloDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo'
$runtimeDirectory = Join-Path $tiloDirectory "Runtime/DXC/$version/$Architecture"
$applicationDirectory = Join-Path $tiloDirectory 'Applications'
New-Item -ItemType Directory -Force -Path $runtimeDirectory, $applicationDirectory | Out-Null
if (-not $ArchivePath) {
    $ArchivePath = Join-Path $runtimeDirectory $archiveName
    if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
        Invoke-WebRequest -Uri "https://github.com/microsoft/DirectXShaderCompiler/releases/download/v$version/$archiveName" -OutFile $ArchivePath
    }
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash -ne $digest) {
    throw "DXC $version archive checksum does not match Microsoft's published release."
}
# dxcompiler is MIT/LLVM licensed. wgpu needs neither the command-line tools
# nor the separately licensed dxil validator; ship only the compiler.
& tar -xf $ArchivePath -C $runtimeDirectory "bin/$Architecture/dxcompiler.dll" LICENSE-MIT.txt LICENSE-LLVM.txt
if ($LASTEXITCODE -ne 0) { throw 'Could not extract the DXC compiler and license notices.' }
$library = Join-Path $runtimeDirectory "bin/$Architecture/dxcompiler.dll"
$destination = Join-Path $applicationDirectory 'dxcompiler.dll'
if (-not (Test-Path -LiteralPath $destination -PathType Leaf) -or
    (Get-FileHash -LiteralPath $library).Hash -ne (Get-FileHash -LiteralPath $destination).Hash) {
    Copy-Item -LiteralPath $library -Destination $destination -Force
}
$licenseDirectory = Join-Path $applicationDirectory "licenses/DXC-$version"
New-Item -ItemType Directory -Force -Path $licenseDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $runtimeDirectory 'LICENSE-MIT.txt'), (Join-Path $runtimeDirectory 'LICENSE-LLVM.txt') -Destination $licenseDirectory -Force
[pscustomobject]@{ Version = $version; Architecture = $Architecture; Library = $destination; Licenses = $licenseDirectory }
