# Build native applications and install them for the current Windows user.
# Cap'n Proto and the pinned Rust/MSVC toolchain must be available on PATH.
[CmdletBinding()]
param(
    [ValidateSet('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer', 'word-processor', 'terminal', 'pdf-viewer', 'ebook', 'clocks')]
    [string[]]$Apps = @('calculator', 'notes', 'calendar', 'contacts', 'sheets', 'slides', 'weather', 'scratchpad', 'file-explorer', 'word-processor', 'terminal', 'pdf-viewer', 'ebook', 'clocks'),
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

$hostHelper = $null
if ('clocks' -in $applications) {
    Push-Location -LiteralPath $repository
    try {
        $hostMessages = & cargo build --locked --profile $Profile -p ui --example windows-host --features notifications --message-format=json-render-diagnostics
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

$installDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Tilo/Applications'
$menuDirectory = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tilo'
New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $menuDirectory -Force | Out-Null
$fontDirectory = Join-Path $installDirectory 'fonts'
New-Item -ItemType Directory -Path $fontDirectory -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $repository 'ui/assets/fonts') | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $fontDirectory -Recurse -Force
}
$shortcutShell = New-Object -ComObject WScript.Shell
if ('pdf-viewer' -in $applications -or 'file-explorer' -in $applications) {
    & (Join-Path $PSScriptRoot 'install-windows-pdfium.ps1')
}
foreach ($application in $applications) {
    $destination = Join-Path $installDirectory "$application.exe"
    Copy-Item -LiteralPath $executables[$application] -Destination $destination -Force
    $title = switch ($application) {
        'file-explorer' { 'Files' }
        'word-processor' { 'Word Processor' }
        'pdf-viewer' { 'PDF Viewer' }
        'ebook' { 'Books' }
        default { (Get-Culture).TextInfo.ToTitleCase($application) }
    }
    $shortcutPath = Join-Path $menuDirectory "$title.lnk"
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $destination
    $shortcut.WorkingDirectory = [Environment]::GetFolderPath('UserProfile')
    $shortcut.Description = "Tilo $title"
    $shortcut.Save()
    if ($application -eq 'clocks') {
        & $hostHelper register 'dev.tilo.clocks' "Tilo $title" $destination $shortcutPath
        if ($LASTEXITCODE -ne 0) { throw 'Could not register Clocks for Windows notifications.' }
        $notificationKey = 'HKCU:/Software/Classes/AppUserModelId/dev.tilo.clocks'
        New-Item -Path $notificationKey -Force | Out-Null
        New-ItemProperty -LiteralPath $notificationKey -Name DisplayName -Value "Tilo $title" -PropertyType String -Force | Out-Null
    }
    [pscustomobject]@{ Application = $title; Executable = $destination; Shortcut = $shortcutPath }
}
