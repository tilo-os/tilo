# Bundle one shared, relocatable GStreamer runtime beside native app executables.
# Source packages must already be installed through trusted MSYS2 pacman repositories.
[CmdletBinding()]
param(
    [string]$SourcePrefix,
    [string]$Destination = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'),
    [string]$ReadObj = 'llvm-readobj.exe',
    [string[]]$ApplicationExecutables,
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-gstreamer-crypto.ps1')
if (-not $IsWindows) { throw 'Run this installer from PowerShell on Windows.' }
$readObjPath = (Get-Command $ReadObj -ErrorAction Stop).Source
$applicationLibraries = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$applicationArchitecture = $null
if ($ApplicationExecutables) {
    foreach ($executable in $ApplicationExecutables) {
        $imports = & $readObjPath --coff-imports $executable
        if ($LASTEXITCODE -ne 0) { throw "Cannot inspect executable imports: $executable" }
        $usesMedia = $false
        foreach ($line in $imports) {
            if ($line -match '^\s+Name: ((?:lib)?gst[^ ]+\.dll)$') {
                $usesMedia = $true
                [void]$applicationLibraries.Add($Matches[1])
            }
        }
        if ($usesMedia) {
            $currentArchitecture = @($imports | Where-Object { $_ -match '^Arch: ' }) -join ''
            if (-not $currentArchitecture) { throw "No PE architecture reported for $executable" }
            if ($applicationArchitecture -and $applicationArchitecture -ne $currentArchitecture) {
                throw 'Applications of different architectures cannot share one media runtime directory.'
            }
            $applicationArchitecture = $currentArchitecture
        }
    }
    if ($applicationLibraries.Count -eq 0) { return }
}
if (-not $SourcePrefix) {
    $environmentName = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
        'Arm64' { 'clangarm64' }
        'X64' { 'ucrt64' }
        default { throw 'This GStreamer bundle supports native Windows ARM64 and x64.' }
    }
    $SourcePrefix = Join-Path ([Environment]::GetFolderPath('UserProfile')) "msys64/$environmentName"
}
$SourcePrefix = (Resolve-Path -LiteralPath $SourcePrefix).Path
$Destination = [IO.Path]::GetFullPath($Destination)
if (-not $PlanOnly) { Assert-TiloMediaNoOptionalOpenSsl $Destination }
$msysRoot = Split-Path -Parent $SourcePrefix
$prefixName = Split-Path -Leaf $SourcePrefix
$packageDatabase = Join-Path $msysRoot 'var/lib/pacman/local'
if (-not (Test-Path -LiteralPath $packageDatabase -PathType Container)) {
    throw 'The source runtime must have its MSYS2 package provenance database.'
}

function Copy-RuntimeFile([string]$Source, [string]$Relative) {
    if ($PlanOnly) { return }
    $target = Join-Path $Destination $Relative
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
    if (-not (Test-Path -LiteralPath $target -PathType Leaf) -or
        (Get-FileHash -LiteralPath $Source).Hash -ne (Get-FileHash -LiteralPath $target).Hash) {
        Copy-Item -LiteralPath $Source -Destination $target -Force
    }
}

# Explicit playback surface shared by Music, Video and all other media consumers.
# Do not ship every development plugin or its unrelated dependency graph.
$plugins = @(
    'coreelements', 'typefindfunctions', 'playback', 'app', 'videoconvertscale',
    'audioconvert', 'audioresample', 'volume', 'autodetect', 'wasapi2', 'directsound',
    'audioparsers', 'videoparsersbad', 'isomp4', 'matroska', 'avi', 'ogg', 'vorbis',
    'opus', 'flac', 'mpg123', 'wavparse', 'wavpack', 'vpx', 'theora', 'libav',
    'subparse', 'soup', 'adaptivedemux2', 'audiotestsrc', 'videotestsrc'
)
$queue = [Collections.Generic.Queue[object]]::new()
foreach ($library in $applicationLibraries) {
    $queue.Enqueue(@{ Source = Join-Path $SourcePrefix "bin/$library"; Relative = $library })
}
foreach ($plugin in $plugins) {
    $relative = "lib/gstreamer-1.0/libgst$plugin.dll"
    $queue.Enqueue(@{ Source = Join-Path $SourcePrefix $relative; Relative = $relative })
}
foreach ($tool in @('gst-inspect-1.0.exe', 'gst-launch-1.0.exe')) {
    $queue.Enqueue(@{ Source = Join-Path $SourcePrefix "bin/$tool"; Relative = $tool })
}
$scanner = 'libexec/gstreamer-1.0/gst-plugin-scanner.exe'
$queue.Enqueue(@{ Source = Join-Path $SourcePrefix $scanner; Relative = $scanner })
# GIO loads its TLS backend dynamically; retain GnuTLS only, not the redundant
# OpenSSL backend excluded by the repository's native crypto boundary.
foreach ($module in @('libgiognutls.dll')) {
    $relative = "lib/gio/modules/$module"
    $queue.Enqueue(@{ Source = Join-Path $SourcePrefix $relative; Relative = $relative })
}
$sources = @{}
$architecture = $null
$systemImports = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$cryptoImports = [Collections.Generic.List[object]]::new()
while ($queue.Count -gt 0) {
    $entry = $queue.Dequeue()
    if ($sources.ContainsKey($entry.Relative)) { continue }
    if (-not (Test-Path -LiteralPath $entry.Source -PathType Leaf)) {
        throw "Missing runtime component $($entry.Source). Install MSYS2 GStreamer core/base/good/bad/libav first."
    }
    $imports = & $readObjPath --coff-imports $entry.Source
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect PE imports: $($entry.Source)" }
    $currentArchitecture = @($imports | Where-Object { $_ -match '^Arch: ' }) -join ''
    if (-not $currentArchitecture) { throw "No PE architecture reported for $($entry.Source)" }
    if (-not $architecture) { $architecture = $currentArchitecture }
    if ($currentArchitecture -ne $architecture) { throw "Mixed runtime architectures: $($entry.Source)" }
    if ($applicationArchitecture -and $currentArchitecture -ne $applicationArchitecture) {
        throw "The source runtime architecture does not match the selected application: $($entry.Source)"
    }
    $sources[$entry.Relative] = $entry.Source
    foreach ($line in $imports) {
        if ($line -notmatch '^\s+Name: (.+\.dll)$') { continue }
        $name = $Matches[1]
        if (Test-TiloMediaOpenSslImport $name) {
            $cryptoImports.Add(@{ Consumer = $entry.Relative; Dependency = $name })
        }
        $dependency = Join-Path $SourcePrefix "bin/$name"
        if (Test-Path -LiteralPath $dependency -PathType Leaf) {
            $queue.Enqueue(@{ Source = $dependency; Relative = $name })
        } elseif ($name -match '^(api-ms-|ext-ms-)' -or
            (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32/$name") -PathType Leaf)) {
            [void]$systemImports.Add($name)
        } else {
            throw "Unresolved dependency $name imported by $($entry.Source)."
        }
    }
}

# Record the exact package version and license declaration for every bundled DLL.
$owners = @{}
$packages = @{}
foreach ($directory in Get-ChildItem -LiteralPath $packageDatabase -Directory) {
    $descriptionPath = Join-Path $directory.FullName 'desc'
    $filesPath = Join-Path $directory.FullName 'files'
    if (-not (Test-Path -LiteralPath $filesPath -PathType Leaf)) { continue }
    $description = Get-Content -LiteralPath $descriptionPath -Raw
    $fields = @{}
    foreach ($match in [regex]::Matches($description, '(?ms)^%([^%]+)%\r?\n(.*?)(?=\r?\n\r?\n|\z)')) {
        $fields[$match.Groups[1].Value] = $match.Groups[2].Value.Trim()
    }
    $files = @(Get-Content -LiteralPath $filesPath | Where-Object { $_.StartsWith("$prefixName/") })
    if ($files.Count -eq 0) { continue }
    $package = @{ Name = $fields.NAME; Version = $fields.VERSION; License = $fields.LICENSE; URL = $fields.URL; Files = $files }
    $packages[$fields.NAME] = $package
    foreach ($file in $files) { $owners[$file] = $fields.NAME }
}
$usedPackages = [Collections.Generic.HashSet[string]]::new()
$cryptoResiduals = foreach ($edge in $cryptoImports) {
    $ownedPath = [IO.Path]::GetRelativePath($msysRoot, $sources[$edge.Consumer]).Replace('\', '/')
    if (-not $owners.ContainsKey($ownedPath)) { throw "No package provenance for $($edge.Consumer)" }
    $package = $packages[$owners[$ownedPath]]
    Assert-TiloMediaCryptoConsumer -Consumer $edge.Consumer -Dependency $edge.Dependency -Package $package.Name -Version $package.Version
    [ordered]@{ Consumer = $edge.Consumer; Dependency = $edge.Dependency; Package = $package.Name; Version = $package.Version; Reason = 'FFmpeg avformat protocol dependency; not Tilo-owned TLS or signing' }
}
$manifestFiles = foreach ($relative in ($sources.Keys | Sort-Object)) {
    $source = $sources[$relative]
    $ownedPath = [IO.Path]::GetRelativePath($msysRoot, $source).Replace('\', '/')
    if (-not $owners.ContainsKey($ownedPath)) { throw "No package provenance for $source" }
    [void]$usedPackages.Add($owners[$ownedPath])
    Copy-RuntimeFile $source $relative
    [ordered]@{ Path = $relative; SHA256 = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash; Package = $owners[$ownedPath] }
}
$supplementDirectory = Join-Path $PSScriptRoot 'gstreamer-licenses'
$supplements = Get-Content -LiteralPath (Join-Path $supplementDirectory 'sources.json') -Raw | ConvertFrom-Json
foreach ($supplement in $supplements) {
    $source = Join-Path $supplementDirectory $supplement.File
    if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne $supplement.SHA256) {
        throw "Supplementary license checksum mismatch: $($supplement.File)"
    }
    Copy-RuntimeFile $source "licenses/GStreamer/supplemental/$($supplement.File)"
}
Copy-RuntimeFile (Join-Path $supplementDirectory 'sources.json') 'licenses/GStreamer/supplemental/sources.json'
Copy-RuntimeFile (Join-Path $supplementDirectory 'README.md') 'licenses/GStreamer/README.md'
$noticeVersions = @{
    'libass' = @('0.17.5', 'libass-0.17.5-COPYING')
    'libtheora' = @('1.2.0', 'libtheora-1.2.0-COPYING')
    'libvorbis' = @('1.3.7', 'libvorbis-1.3.7-COPYING')
    'libvpx' = @('1.16.0', 'libvpx-1.16.0-LICENSE')
    'p11-kit' = @('0.26.5', 'p11-kit-0.26.5-COPYING')
    'libx264' = @('0.165.r3222.b35605a', 'libx264-b35605a-COPYING')
}
$manifestPackages = foreach ($name in ($usedPackages | Sort-Object)) {
    $package = $packages[$name]
    $licenseFiles = @($package.Files | Where-Object { $_ -match '/share/licenses/.+[^/]$' })
    foreach ($file in $licenseFiles) {
        Copy-RuntimeFile (Join-Path $msysRoot $file) "licenses/GStreamer/packages/$name/$($file.Substring($file.IndexOf('/share/licenses/') + 16))"
    }
    $supplementalNotices = @()
    $shortName = $name -replace '^mingw-w64-(clang-aarch64|ucrt-x86_64|x86_64)-', ''
    if ($licenseFiles.Count -eq 0 -and $noticeVersions.ContainsKey($shortName)) {
        $notice = $noticeVersions[$shortName]
        if (($package.Version -replace '-[^-]+$', '') -ne $notice[0]) {
            throw "Update the verified upstream license notice for $name $($package.Version) before bundling it."
        }
        $supplementalNotices += $notice[1]
    } elseif ($licenseFiles.Count -eq 0 -and $package.License -match 'custom|BSD') {
        throw "Missing copyright notice for $name; do not publish an incomplete runtime."
    }
    [ordered]@{ Name = $name; Version = $package.Version; License = $package.License; URL = $package.URL; LicenseFiles = $licenseFiles; SupplementalNotices = $supplementalNotices }
}
$manifest = [ordered]@{
    Format = 1
    Architecture = $architecture -replace '^Arch: ', ''
    Packages = @($manifestPackages)
    Files = @($manifestFiles)
    SystemImports = @($systemImports | Sort-Object)
    ReviewedCryptoResiduals = @($cryptoResiduals)
}
if ($PlanOnly) { return [pscustomobject]$manifest }
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Destination 'gstreamer-runtime.json') -Encoding utf8
# Keep the dynamic GIO cache relocatable and limited to the bundled backends.
@('libgiognutls.dll: gio-tls-backend') |
    Set-Content -LiteralPath (Join-Path $Destination 'lib/gio/modules/giomodule.cache') -Encoding utf8NoBOM
$null = & (Join-Path $PSScriptRoot 'test-windows-gstreamer.ps1') -RuntimeDirectory $Destination
[pscustomobject]@{ Directory = $Destination; Files = $sources.Count; Packages = $usedPackages.Count; Verified = $true; Manifest = (Join-Path $Destination 'gstreamer-runtime.json') }
