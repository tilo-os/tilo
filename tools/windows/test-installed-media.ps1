# Real installed-app checks with isolated state and no visible windows.
# Total process duration is not startup-to-first-window latency.
[CmdletBinding()]
param(
    [string]$InstalledDirectory = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'),
    [ValidateRange(1, 600)][int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Run this check from PowerShell on Windows.' }
. (Join-Path $PSScriptRoot 'install-path.ps1')
$InstalledDirectory = (Resolve-Path -LiteralPath $InstalledDirectory).Path
Assert-TiloPublicInstallDirectory $InstalledDirectory
$Repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$fixture = (Resolve-Path -LiteralPath (Join-Path $Repository 'apps/video-player/tests/fixtures/h264-aac.mp4')).Path
foreach ($app in @('music', 'video-player')) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstalledDirectory "$app.exe") -PathType Leaf)) {
        throw "Missing installed $app executable."
    }
}
$proofRoot = Join-Path ([IO.Path]::GetTempPath()) ('tilo-installed-media-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $proofRoot | Out-Null
Write-Host "Headless media artifacts: $proofRoot"
function Invoke-MediaProof([string]$App, [string[]]$TestArguments) {
    $scope = Join-Path $proofRoot $App
    foreach ($subdirectory in @('', 'config', 'config/tilo', 'data', 'cache', 'media-cache')) {
        New-Item -ItemType Directory -Path (Join-Path $scope $subdirectory) -Force | Out-Null
    }
    '{"video_player":{"autoplay":false,"loop_enabled":false}}' | Set-Content -LiteralPath (Join-Path $scope 'config/tilo/settings.json')
    $launch = [Diagnostics.ProcessStartInfo]::new((Join-Path $InstalledDirectory ($App + '.exe')))
    $launch.UseShellExecute = $false
    $launch.CreateNoWindow = $true
    $launch.RedirectStandardOutput = $true
    $launch.RedirectStandardError = $true
    $launch.WorkingDirectory = $scope
    foreach ($environmentKey in @($launch.Environment.Keys)) {
        if ($environmentKey -match '^(GST_|GIO_|GSTREAMER|TILO_|QUICK_)') { [void]$launch.Environment.Remove($environmentKey) }
    }
    $launch.Environment['PATH'] = (Join-Path $env:SystemRoot 'System32') + ';' + $env:SystemRoot
    $launch.Environment['XDG_CONFIG_HOME'] = Join-Path $scope 'config'
    $launch.Environment['XDG_DATA_HOME'] = Join-Path $scope 'data'
    $launch.Environment['XDG_CACHE_HOME'] = Join-Path $scope 'cache'
    $launch.Environment['TILO_MUSIC_CACHE'] = Join-Path $scope 'media-cache'
    $launch.Environment['TILO_MUSIC_PROFILE'] = Join-Path $scope 'device-profile.json'
    $launch.Environment['GST_REGISTRY_1_0'] = Join-Path $scope 'gst-registry.bin'
    foreach ($testArgument in $TestArguments) { $launch.ArgumentList.Add($testArgument) }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $child = [Diagnostics.Process]::Start($launch)
    try {
        $outputTask = $child.StandardOutput.ReadToEndAsync()
        $errorTask = $child.StandardError.ReadToEndAsync()
        $timedOut = -not $child.WaitForExit($TimeoutSeconds * 1000)
        if ($timedOut) {
            $child.Kill($true)
            if (-not $child.WaitForExit(5000)) { throw "$App did not terminate after its test timeout." }
        }
        $output = $outputTask.GetAwaiter().GetResult()
        $errors = $errorTask.GetAwaiter().GetResult()
        $output | Set-Content -LiteralPath (Join-Path $scope 'stdout.txt')
        $errors | Set-Content -LiteralPath (Join-Path $scope 'stderr.txt')
        if ($timedOut) { throw "$App headless proof exceeded $TimeoutSeconds seconds; logs: $scope" }
        if ($child.ExitCode -ne 0) { throw "$App exited $($child.ExitCode): $errors" }
        [pscustomobject]@{ App=$App; ExitCode=$child.ExitCode; TotalProcessMs=$watch.ElapsedMilliseconds; Output=$output; Artifacts=$scope }
    } finally { $child.Dispose() }
}
# Music proves the installed loader and CPU widget path, not audio playback.
$music = Invoke-MediaProof 'music' @('--bench','3','--viewport','640x360')
if ($music.Output -notmatch '(?s)bench.*frames=3') { throw 'Music did not report the requested benchmark frames' }
$pngPath = Join-Path $proofRoot 'video.png'
# Autoplay is disabled in the isolated settings before opening the fixture;
# --scenario paused alone would be applied after the initial open.
$video = Invoke-MediaProof 'video-player' @('--shots',$pngPath,'--open',$fixture,'--scenario','paused','--viewport','640x360')
$png = [IO.File]::ReadAllBytes($pngPath)
if ($png.Length -lt 24 -or [BitConverter]::ToString($png[0..7]) -ne '89-50-4E-47-0D-0A-1A-0A') { throw 'Video proof is not a PNG' }
$width = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($png,16))
$height = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($png,20))
if ($width -ne 640 -or $height -ne 360) { throw "Unexpected PNG dimensions ${width}x${height}" }
@($music,$video) | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $proofRoot 'results.json')
@($music,$video) | ConvertTo-Json -Depth 4
