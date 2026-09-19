# Headless relocation proof: no MSYS2 PATH, plugin overrides or reused registry.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RuntimeDirectory,
    # Optional MP4 regression fixture containing both H.264 video and AAC audio.
    [string]$MediaPath,
    [string]$ReadObj = 'llvm-readobj.exe',
    [switch]$KeepRegistry
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'gstreamer-crypto.ps1')
$RuntimeDirectory = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
Assert-TiloMediaNoOptionalOpenSsl $RuntimeDirectory
$manifest = Get-Content -LiteralPath (Join-Path $RuntimeDirectory 'gstreamer-runtime.json') -Raw | ConvertFrom-Json
$readObjPath = (Get-Command $ReadObj -ErrorAction Stop).Source
$packages = @{}
foreach ($package in $manifest.Packages) { $packages[$package.Name] = $package }
$manifestFiles = @{}
foreach ($file in $manifest.Files) {
    $manifestFiles[$file.Path] = $file
    $path = Join-Path $RuntimeDirectory $file.Path
    if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $file.SHA256) {
        throw "Runtime manifest file is missing or changed: $($file.Path)"
    }
}
$observedResiduals = [Collections.Generic.List[string]]::new()
foreach ($binary in Get-ChildItem -LiteralPath $RuntimeDirectory -Recurse -File | Where-Object { $_.Extension -in '.dll', '.exe' }) {
    $relative = [IO.Path]::GetRelativePath($RuntimeDirectory, $binary.FullName).Replace('\', '/')
    $imports = & $readObjPath --coff-imports $binary.FullName
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect staged imports: $relative" }
    foreach ($line in $imports) {
        if ($line -notmatch '^\s+Name: (.+\.dll)$') { continue }
        $dependency = $Matches[1]
        if (Test-TiloMediaOpenSslImport $dependency) {
            if (-not $manifestFiles.ContainsKey($relative)) { throw "Unmanifested OpenSSL consumer: $relative -> $dependency" }
            $owner = $packages[$manifestFiles[$relative].Package]
            Assert-TiloMediaCryptoConsumer -Consumer $relative -Dependency $dependency -Package $owner.Name -Version $owner.Version
            $observedResiduals.Add("$relative|$dependency|$($owner.Name)|$($owner.Version)")
        }
    }
}
$declaredResiduals = @($manifest.ReviewedCryptoResiduals | ForEach-Object { "$($_.Consumer)|$($_.Dependency)|$($_.Package)|$($_.Version)" })
if (Compare-Object @($observedResiduals | Sort-Object) @($declaredResiduals | Sort-Object)) {
    throw 'The runtime manifest does not match its observed OpenSSL residual imports.'
}
$registry = Join-Path ([IO.Path]::GetTempPath()) ("tilo-gstreamer-proof-" + [guid]::NewGuid().ToString('N') + '.bin')
function Invoke-IsolatedGStreamer([string]$Tool, [string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $RuntimeDirectory $Tool))
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = [IO.Path]::GetTempPath()
    foreach ($name in @($start.Environment.Keys)) {
        if ($name -match '^GST_|^GIO_|^GSTREAMER') { [void]$start.Environment.Remove($name) }
    }
    $start.Environment['PATH'] = "$env:SystemRoot/System32;$env:SystemRoot"
    # This only isolates test state; discovery must find the runtime by itself.
    $start.Environment['GST_REGISTRY_1_0'] = $registry
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(30000)) {
        $process.Kill($true)
        $process.WaitForExit()
        throw "$Tool timed out during relocation verification."
    }
    $text = $stdout.GetAwaiter().GetResult()
    $errorText = $stderr.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0 -or $errorText -match 'Failed to load plugin|External plugin loader failed') {
        throw "$Tool $Arguments failed ($($process.ExitCode)): $errorText $text"
    }
    $text
}
try {
    foreach ($element in @('playbin', 'appsink', 'avdec_h264', 'avdec_aac', 'wasapi2sink', 'mpg123audiodec', 'flacdec', 'qtdemux', 'subparse', 'souphttpsrc')) {
        $inspection = Invoke-IsolatedGStreamer 'gst-inspect-1.0.exe' @($element)
        if ($inspection -notmatch ('(?im)^\s*Filename\s+' + [regex]::Escape($RuntimeDirectory) + '[\\/]')) {
            throw "$element did not resolve to the relocated runtime."
        }
    }
    $blacklist = Invoke-IsolatedGStreamer 'gst-inspect-1.0.exe' @('--print-blacklist')
    if ($blacklist -notmatch 'Total count: 0 blacklisted files') { throw "Blacklisted runtime plugins: $blacklist" }
    $null = Invoke-IsolatedGStreamer 'gst-launch-1.0.exe' @('-q', 'audiotestsrc', 'num-buffers=4', '!', 'audioconvert', '!', 'fakesink')
    $null = Invoke-IsolatedGStreamer 'gst-launch-1.0.exe' @('-q', 'videotestsrc', 'num-buffers=4', '!', 'videoconvert', '!', 'fakesink')
    if ($MediaPath) {
        $media = (Resolve-Path -LiteralPath $MediaPath).Path
        $uri = [Uri]::new($media).AbsoluteUri
        $null = Invoke-IsolatedGStreamer 'gst-launch-1.0.exe' @('-q', 'playbin', "uri=$uri", 'video-sink=fakesink', 'audio-sink=fakesink')
        # Require both streams to traverse their real decoders. Playbin alone
        # can finish successfully when only one of a file's tracks is usable.
        $null = Invoke-IsolatedGStreamer 'gst-launch-1.0.exe' @(
            '-q', 'filesrc', ('location="' + $media.Replace('\', '/') + '"'), '!', 'qtdemux', 'name=demux',
            'demux.video_0', '!', 'queue', '!', 'h264parse', '!', 'avdec_h264',
            '!', 'videoconvert', '!', 'video/x-raw,format=RGBA', '!', 'fakesink',
            'demux.audio_0', '!', 'queue', '!', 'aacparse', '!', 'avdec_aac',
            '!', 'audioconvert', '!', 'audio/x-raw,format=F32LE', '!', 'fakesink'
        )
    }
    [pscustomobject]@{ Runtime = $RuntimeDirectory; Relocation = 'passed'; Media = $MediaPath }
} finally {
    if (-not $KeepRegistry -and (Test-Path -LiteralPath $registry -PathType Leaf)) { Remove-Item -LiteralPath $registry }
}
