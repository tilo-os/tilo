# Time the Windows ARM64 WHPX development loop. Without -Execute this prints
# the exact commands. Run from a complete Tilo worktree for swap phases.
param(
    [Parameter(Mandatory = $true)][string]$CliPath,
    [string]$VmName = 'tilo-whpx-dev',
    [string]$ImagePath,
    [string]$Tag,
    [string]$DiskPath = 'C:\tilo-build\tilo-whpx-dev.qcow2',
    [string]$DirectBootBundle,
    [string]$LogPath = 'C:\tilo-build\tilo-vm-bench.jsonl',
    [int]$MemoryMB = 4096,
    [int]$Cpus = 4,
    [int]$SshPort = 2244,
    [ValidateSet('create', 'start', 'slot', 'swap', 'restart', 'app', 'stop')]
    [string[]]$Phase = @('create', 'start', 'slot', 'swap', 'restart', 'app'),
    [string]$Component = 'shell',
    [string]$App = 'notes',
    [switch]$Execute
)

$ErrorActionPreference = 'Stop'
$CliPath = (Resolve-Path -LiteralPath $CliPath).Path
$workspace = (Get-Location).Path
$revision = (& git rev-parse HEAD 2>$null)
if ($LASTEXITCODE -ne 0) { $revision = '' }

if ($Phase -contains 'create') {
    if ([bool]$ImagePath -eq [bool]$Tag) {
        throw 'Specify exactly one of -ImagePath or -Tag for the create phase.'
    }
    if ($ImagePath) {
        $ImagePath = [System.IO.Path]::GetFullPath($ImagePath)
        if ($Execute) { $ImagePath = (Resolve-Path -LiteralPath $ImagePath).Path }
    }
    if ($DirectBootBundle) {
        $DirectBootBundle = [System.IO.Path]::GetFullPath($DirectBootBundle)
        if ($Execute) { $DirectBootBundle = (Resolve-Path -LiteralPath $DirectBootBundle).Path }
    }
}
if ($Execute -and ($Phase -contains 'swap' -or $Phase -contains 'app')) {
    if (-not (Test-Path -LiteralPath 'os/components.toml')) {
        throw 'Swap phases need a complete Tilo worktree; run git sparse-checkout disable here first.'
    }
}

# All slot operations address this VM without writing .tilo-vm into the tree.
$env:TILO_VM = $VmName
$commands = [ordered]@{}
$create = @('vm', 'create', $VmName, '--hypervisor', 'whpx',
            '--disk', $DiskPath, '--headless', '--memory-mb', [string]$MemoryMB,
            '--cpus', [string]$Cpus, '--ssh-port', [string]$SshPort)
if ($Tag) { $create += @('--tag', $Tag) } else { $create += @('--image', $ImagePath) }
if ($DirectBootBundle) { $create += @('--direct-boot-bundle', $DirectBootBundle) }
$commands.create = $create
$commands.start = @('vm', 'start', $VmName)
$commands.slot = @('slot', 'create', '--headless')
$commands.swap = @('swap', $Component, '--profile', 'dev')
$commands.restart = @('vm', 'restart', $VmName)
$commands.app = @('swap', "app:$App")
$commands.stop = @('vm', 'stop', $VmName)

foreach ($name in $Phase) {
    $arguments = $commands[$name]
    $display = @($CliPath) + $arguments | ForEach-Object {
        if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
    }
    Write-Host ("{0}: {1}" -f $name, ($display -join ' '))
    if (-not $Execute) { continue }

    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $CliPath @arguments
    $exitCode = $LASTEXITCODE
    $watch.Stop()
    $record = [ordered]@{
        utc = [DateTime]::UtcNow.ToString('o')
        phase = $name
        elapsedMs = $watch.ElapsedMilliseconds
        exitCode = $exitCode
        vm = $VmName
        revision = $revision
        image = $ImagePath
        tag = $Tag
        directBoot = [bool]$DirectBootBundle -or [bool]$Tag
        component = $(if ($name -eq 'swap') { $Component } else { $null })
        app = $(if ($name -eq 'app') { $App } else { $null })
    }
    $line = ($record | ConvertTo-Json -Compress) + [Environment]::NewLine
    $parent = Split-Path -Parent $LogPath
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    [System.IO.File]::AppendAllText($LogPath, $line, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host ("{0}: {1} ms, exit {2}" -f $name, $watch.ElapsedMilliseconds, $exitCode)
    if ($exitCode -ne 0) { throw "$name failed with exit code $exitCode; see $LogPath" }
}
