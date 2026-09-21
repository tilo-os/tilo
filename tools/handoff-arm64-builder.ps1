<#
.SYNOPSIS
Cleanly hands a completed creator disk to disposable local ARM64 boot attempts.

.DESCRIPTION
The creator remains untouched in -CheckOnly mode. In normal mode, this script
verifies and hashes its completed disk, powers the creator off, mounts the
native output store read-only in WSL, makes a sparse verified copy, then starts
native Windows QEMU/WHPX with a qcow2 overlay and fresh UEFI variables.
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 65535)] [int]$SshPort = 2222,
    [ValidateRange(1, 65535)] [int]$BuilderQmpPort = 4445,
    [string]$BuilderPidFile = (Join-Path $env:USERPROFILE 'qos\.qemu\arm64-local\disposable-qemu.pid'),
    [string]$OutputStore = 'C:\tilo-build\output-store.raw',
    [string]$WslDistribution = 'FedoraLinux-42',
    [string]$DestinationLinux = '/home/xemmerling/tilo-vm/media/disk.raw',
    [switch]$CheckOnly,
    [switch]$NoLaunch
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$extractor = Join-Path $PSScriptRoot 'extract-arm64-disk-from-store.sh'
$launcher = Join-Path $PSScriptRoot 'iterate-arm64-disk.ps1'
foreach ($required in @($BuilderPidFile, $OutputStore, $extractor, $launcher)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required handoff file is missing: $required"
    }
}

$builderPid = [int](Get-Content -Raw -LiteralPath $BuilderPidFile)
if (-not (Get-Process -Id $builderPid -ErrorAction SilentlyContinue)) {
    throw "Builder QEMU PID $builderPid is not running"
}

$extractorLinux = ((& wsl.exe --distribution $WslDistribution --exec wslpath -a $extractor) -join '').Trim()
$storeLinux = ((& wsl.exe --distribution $WslDistribution --exec wslpath -a $OutputStore) -join '').Trim()
if ($LASTEXITCODE -ne 0 -or -not $extractorLinux.StartsWith('/') -or -not $storeLinux.StartsWith('/')) {
    throw 'Could not translate the extractor or output-store path into WSL.'
}
& wsl.exe -d $WslDistribution -- bash -n $extractorLinux
if ($LASTEXITCODE -ne 0) {
    throw 'The WSL extractor has invalid shell syntax.'
}

$destinationWindows = "\\wsl.localhost\$WslDistribution$($DestinationLinux.Replace('/', '\'))"
if ($CheckOnly) {
    $freeBytes = (Get-Volume -DriveLetter C).SizeRemaining
    Write-Host "Builder:      PID $builderPid [running; untouched]"
    Write-Host "Output store: $OutputStore [$((Get-Item -LiteralPath $OutputStore).Length) bytes]"
    Write-Host "Extractor:    $extractorLinux [syntax valid]"
    Write-Host "Destination:  $destinationWindows"
    Write-Host "C: free:      $freeBytes bytes (actual copy guard uses sparse allocation + 8 GiB reserve)"
    Write-Host "Next:         rerun without -CheckOnly only after creator completion"
    return
}

$sshOptions = @(
    '-o', 'BatchMode=yes',
    '-o', 'StrictHostKeyChecking=no',
    '-o', 'UserKnownHostsFile=NUL',
    '-o', 'ConnectTimeout=5',
    '-p', "$SshPort",
    'builder@127.0.0.1'
)
$guestPreflight = @'
set -euo pipefail
disk=/mnt/tilo-output/disk.raw
test -f "$disk"
test "$(stat -c %s "$disk")" -eq 68719476736
test "$(( $(stat -c %b "$disk") * 512 ))" -gt 0
sudo sync
sudo sgdisk -v "$disk"
'@
& ssh.exe @sshOptions $guestPreflight
if ($LASTEXITCODE -ne 0) {
    throw "Guest disk completion preflight failed with status $LASTEXITCODE; builder was left running"
}

# An SSH disconnect is expected during a successful poweroff. Process exit is
# the authority that the native output store is no longer open by QEMU.
& ssh.exe @sshOptions 'sudo systemctl poweroff' 2>$null
$deadline = [DateTime]::UtcNow.AddSeconds(90)
while ((Get-Process -Id $builderPid -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 500
}
if (Get-Process -Id $builderPid -ErrorAction SilentlyContinue) {
    Write-Warning 'SSH shutdown did not finish in 90 seconds; requesting ACPI poweroff through QMP.'
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $client.Connect('127.0.0.1', $BuilderQmpPort)
        $stream = $client.GetStream()
        $stream.ReadTimeout = 2000
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $false, 1024, $true)
        $writer = [IO.StreamWriter]::new($stream, [Text.Encoding]::ASCII, 1024, $true)
        $writer.NewLine = "`r`n"
        $writer.AutoFlush = $true
        [void]$reader.ReadLine()
        $writer.WriteLine('{"execute":"qmp_capabilities"}')
        [void]$reader.ReadLine()
        $writer.WriteLine('{"execute":"system_powerdown"}')
    } finally {
        $client.Dispose()
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    while ((Get-Process -Id $builderPid -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 500
    }
}
if (Get-Process -Id $builderPid -ErrorAction SilentlyContinue) {
    throw "Builder QEMU PID $builderPid did not shut down cleanly; output store was not mounted"
}

& wsl.exe -d $WslDistribution -- bash $extractorLinux $storeLinux $DestinationLinux
if ($LASTEXITCODE -ne 0) {
    throw "Read-only sparse extraction failed with status $LASTEXITCODE"
}

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $launcher -DiskPath $destinationWindows -CheckOnly
if ($LASTEXITCODE -ne 0) {
    throw "WHPX disposable-boot preflight failed with status $LASTEXITCODE"
}
if (-not $NoLaunch) {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $launcher -DiskPath $destinationWindows
    if ($LASTEXITCODE -ne 0) {
        throw "Disposable Tilo boot launch failed with status $LASTEXITCODE"
    }
}
