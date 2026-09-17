# Build native applications and install them for the current Windows user.
# Cap'n Proto and the pinned Rust/MSVC toolchain must be available on PATH.
[CmdletBinding()]
param(
    [ValidateSet('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer')]
    [string[]]$Apps = @('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer'),
    [ValidateSet('dev', 'release')]
    [string]$Profile = 'dev'
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Run this installer from PowerShell on Windows.' }
if ($Apps.Count -eq 0) { throw 'Select at least one application.' }
$repository = Split-Path -Parent $PSScriptRoot
$applications = $Apps | Select-Object -Unique
$buildArguments = @('build', '--locked', '--profile', $Profile, '--message-format=json-render-diagnostics')
foreach ($application in $applications) { $buildArguments += @('-p', $application) }

Push-Location -LiteralPath $repository
try {
    $messages = & cargo @buildArguments
    $buildExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

$executables = @{}
foreach ($line in $messages) {
    $entry = $line | ConvertFrom-Json
    if ($entry.reason -eq 'compiler-message' -and $entry.message.rendered) {
        Write-Host $entry.message.rendered
    }
    if ($entry.reason -eq 'compiler-artifact' -and $entry.executable -and
        $entry.target.name -in $applications) {
        $executables[$entry.target.name] = $entry.executable
    }
}
if ($buildExitCode -ne 0) { throw "Cargo build failed ($buildExitCode)." }
foreach ($application in $applications) {
    if (-not $executables.ContainsKey($application) -or
        -not (Test-Path -LiteralPath $executables[$application] -PathType Leaf)) {
        throw "Cargo did not produce an executable for $application."
    }
}

$installDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'
$menuDirectory = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tilo'
New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $menuDirectory -Force | Out-Null
$shortcutShell = New-Object -ComObject WScript.Shell
foreach ($application in $applications) {
    $destination = Join-Path $installDirectory "$application.exe"
    Copy-Item -LiteralPath $executables[$application] -Destination $destination -Force
    $title = if ($application -eq 'file-explorer') { 'Files' } else { (Get-Culture).TextInfo.ToTitleCase($application) }
    $shortcutPath = Join-Path $menuDirectory "$title.lnk"
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $destination
    $shortcut.WorkingDirectory = [Environment]::GetFolderPath('UserProfile')
    $shortcut.Description = "Tilo $title"
    $shortcut.Save()
    [pscustomobject]@{ Application = $title; Executable = $destination; Shortcut = $shortcutPath }
}
