# Verify the Windows app catalog, not merely the presence of .lnk files.
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^[a-z][a-z0-9-]*$')]
    [string[]]$Apps,
    [ValidateRange(0, 60)]
    [int]$TimeoutSeconds = 15
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-app-names.ps1')
if (-not $IsWindows) { throw 'Run this check from PowerShell on Windows.' }
$installDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'
$expected = @{}
foreach ($application in ($Apps | Select-Object -Unique)) {
    $path = Join-Path $installDirectory "$application.exe"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing installed executable: $path" }
    $expected[$application] = [pscustomobject]@{
        Path = (Get-Item -LiteralPath $path).FullName
        Name = Get-TiloWindowsAppName -Application $application
    }
}

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$shell = New-Object -ComObject Shell.Application
do {
    $catalog = @($shell.Namespace('shell:AppsFolder').Items() | ForEach-Object {
        [pscustomobject]@{
            Name = $_.Name
            AppID = $_.Path
            Target = $_.ExtendedProperty('System.Link.TargetParsingPath')
        }
    })
    $startApps = @(Get-StartApps)
    $missing = @()
    $verified = @()
    foreach ($application in $expected.Keys) {
        $entry = $catalog | Where-Object {
            $_.Target -eq $expected[$application].Path -and $_.Name -eq $expected[$application].Name
        } | Select-Object -First 1
        if (-not $entry -or -not ($startApps | Where-Object { $_.AppID -eq $entry.AppID -and $_.Name -eq $entry.Name })) {
            $missing += $application
        } else {
            $verified += [pscustomobject]@{ Application = $application; StartName = $entry.Name; AppID = $entry.AppID }
        }
    }
    if ($missing.Count -eq 0) { $verified; return }
    if ([DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 500 }
} while ([DateTime]::UtcNow -lt $deadline)

throw "Windows has not registered these installed apps in Start: $($missing -join ', '). The shortcut files alone do not prove installation. Packaged terminals or agents can redirect AppData writes into a private package cache that Explorer cannot see. First verify the executable and shortcut paths from an ordinary, unpackaged PowerShell session and rerun the installer there if they are missing. If the real files exist, investigate Explorer's app catalog. Rerun tools/check-windows-start-apps.ps1 -Apps $($Apps -join ',')."
