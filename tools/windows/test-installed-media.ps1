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
foreach ($app in @('music')) {
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
$music | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $proofRoot 'results.json')
$music | ConvertTo-Json -Depth 4
