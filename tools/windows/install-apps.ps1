# Build native applications and install them for the current Windows user.
# Cap'n Proto, LLVM's llvm-readobj and the pinned Rust/MSVC toolchain must be on PATH.
[CmdletBinding()]
param(
    [ValidateSet('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer', 'word-processor', 'terminal', 'pdf-viewer', 'ebook', 'clocks', 'photos', 'hex-editor', 'code-editor', 'api-tester', 'diffmerge', 'snippets', 'db-client', 'git-client', 'feeds', 'totp', 'passwords', 'typst-editor', 'mail', 'video-player', 'music')]
    [string[]]$Apps = @('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer', 'word-processor', 'terminal', 'pdf-viewer', 'ebook', 'clocks', 'photos', 'hex-editor', 'code-editor', 'api-tester', 'diffmerge', 'snippets', 'db-client', 'git-client', 'feeds', 'totp'),
    [ValidateSet('dev', 'release')]
    [string]$Profile = 'release',
    [ValidateRange(1, 256)]
    [int]$Jobs = 2,
    [string]$GStreamerPrefix
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'app-names.ps1')
. (Join-Path $PSScriptRoot 'install-path.ps1')
. (Join-Path $PSScriptRoot 'app-build-plan.ps1')
if (-not $IsWindows) { throw 'Run this installer from PowerShell on Windows.' }
if ($Apps.Count -eq 0) { throw 'Select at least one application.' }
$installDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'
$menuDirectory = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tilo'
Assert-TiloPublicInstallDirectory $installDirectory
Assert-TiloPublicInstallDirectory $menuDirectory
$repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$buildPlan = Get-TiloWindowsAppBuildPlan -Apps $Apps -Profile $Profile -Jobs $Jobs
$applications = $buildPlan.applications
$buildArguments = $buildPlan.applicationBuild.arguments

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

$hostHelper = $null
if ($null -ne $buildPlan.notificationBuild) {
    $hostBuildArguments = $buildPlan.notificationBuild.arguments
    Push-Location -LiteralPath $repository
    try {
        $hostMessages = & cargo @hostBuildArguments
        $hostExitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    foreach ($line in $hostMessages) {
        $entry = $line | ConvertFrom-Json
        if ($entry.reason -eq 'compiler-artifact' -and $entry.target.name -eq 'windows-host' -and $entry.executable) {
            $hostHelper = $entry.executable
        }
    }
    if ($hostExitCode -ne 0 -or -not $hostHelper) { throw 'Could not build Windows notification registration helper.' }
}

New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $menuDirectory -Force | Out-Null
$fontDirectory = Join-Path $installDirectory 'fonts'
New-Item -ItemType Directory -Path $fontDirectory -Force | Out-Null
$fontMetadata = Join-Path $repository 'crates/sdk/ui-core/assets/fonts'
$python = if ($env:PYTHON) { $env:PYTHON } elseif (Get-Command python3 -ErrorAction SilentlyContinue) { 'python3' } else { 'python' }
$fontSource = & $python (Join-Path $repository 'tools/font-catalog.py') root $fontMetadata --scope fixtures
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $fontSource -PathType Container)) {
    throw 'Could not prepare the pinned offline font catalog.'
}
Get-ChildItem -LiteralPath $fontSource -Recurse -File | ForEach-Object {
    $fontTarget = Join-Path $fontDirectory ([IO.Path]::GetRelativePath($fontSource, $_.FullName))
    # Live apps memory-map these shared files. Avoid overwriting unchanged
    # fonts when installing a different app, including on repeated installs.
    if ((Test-Path -LiteralPath $fontTarget -PathType Leaf) -and
        (Get-FileHash -LiteralPath $_.FullName).Hash -eq (Get-FileHash -LiteralPath $fontTarget).Hash) {
        return
    }
    New-Item -ItemType Directory -Path (Split-Path -Parent $fontTarget) -Force | Out-Null
    Copy-Item -LiteralPath $_.FullName -Destination $fontTarget -Force
}
$shortcutShell = New-Object -ComObject WScript.Shell
& (Join-Path $PSScriptRoot 'install-dxc.ps1')
if ($buildPlan.runtime.pdfium) {
    & (Join-Path $PSScriptRoot 'install-pdfium.ps1')
}
# Detect actual native media imports, so every app shares the same runtime
# without maintaining per-app environment variables or launcher wrappers.
& (Join-Path $PSScriptRoot 'install-gstreamer.ps1') -SourcePrefix $GStreamerPrefix -Destination $installDirectory -ApplicationExecutables @($executables.Values)
foreach ($application in $applications) {
    $destination = Join-Path $installDirectory "$application.exe"
    Copy-Item -LiteralPath $executables[$application] -Destination $destination -Force
    $title = Get-TiloWindowsAppName -Application $application
    $shortcutPath = Join-Path $menuDirectory "$title.lnk"
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $workingDirectory = [Environment]::GetFolderPath('UserProfile')
    # A binary update does not require rewriting an unchanged Start entry.
    # Preserve its shell metadata and avoid needless catalog invalidation.
    if ($shortcut.TargetPath -ne $destination -or
        $shortcut.WorkingDirectory -ne $workingDirectory -or
        $shortcut.Description -ne "Tilo $title") {
        $shortcut.TargetPath = $destination
        $shortcut.WorkingDirectory = $workingDirectory
        $shortcut.Description = "Tilo $title"
        $shortcut.Save()
    }
    if ($application -eq 'clocks') {
        & $hostHelper register 'so.tilo.clocks' "Tilo $title" $destination $shortcutPath
        if ($LASTEXITCODE -ne 0) { throw 'Could not register Clocks for Windows notifications.' }
        $notificationKey = 'HKCU:/Software/Classes/AppUserModelId/so.tilo.clocks'
        New-Item -Path $notificationKey -Force | Out-Null
        New-ItemProperty -LiteralPath $notificationKey -Name DisplayName -Value "Tilo $title" -PropertyType String -Force | Out-Null
        $activationKey = 'HKCU:/Software/Classes/tilo-clocks'
        New-Item -Path $activationKey -Force | Out-Null
        Set-Item -LiteralPath $activationKey -Value 'URL:Tilo Clocks'
        New-ItemProperty -LiteralPath $activationKey -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
        New-Item -Path "$activationKey/shell/open/command" -Force | Out-Null
        # This route only reopens Clocks. Never forward untrusted URI contents
        # as command-line arguments or interpret them through a command shell.
        Set-Item -LiteralPath "$activationKey/shell/open/command" -Value ('"' + $destination + '"')
    }
    [pscustomobject]@{ Application = $title; Executable = $destination; Shortcut = $shortcutPath }
}

# Explorer can retain a stale app catalog even when the shortcuts are valid.
# Do not report a successful installation without checking what Start exposes.
& (Join-Path $PSScriptRoot 'check-start-apps.ps1') -Apps $applications
