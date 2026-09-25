<#
.SYNOPSIS
Starts disposable Tilo ARM64 boot attempts on native Windows QEMU/WHPX.

.DESCRIPTION
Treats the assembled raw disk as an immutable base. Every boot gets a new
qcow2 overlay, a new copy of the UEFI variable store, its own serial log and a
free QMP port. A failed early-boot experiment can therefore be discarded
without copying or rebuilding the raw disk.

.EXAMPLE
.\tools\iterate-arm64-disk.ps1 -CheckOnly

.EXAMPLE
.\tools\iterate-arm64-disk.ps1

.EXAMPLE
.\tools\iterate-arm64-disk.ps1 -Action List

.EXAMPLE
.\tools\iterate-arm64-disk.ps1 -Action Cleanup -AttemptId 20260921T201530123Z-a1b2c3

.EXAMPLE
.\tools\iterate-arm64-disk.ps1 -Action Cleanup -AllStopped
#>
[CmdletBinding()]
param(
    [ValidateSet('Start', 'List', 'Cleanup')]
    [string]$Action = 'Start',
    [string]$AttemptId,
    [switch]$AllStopped,
    [string]$DiskPath,
    [string]$WslDistribution = 'FedoraLinux-42',
    [string]$QemuRoot = (Join-Path $env:USERPROFILE 'msys64\clangarm64'),
    [string]$StateDirectory = (Join-Path $env:LOCALAPPDATA 'Tilo\qemu-arm64-iterations'),
    [ValidateRange(1, 256)]
    [int]$CpuCount = 8,
    [ValidateRange(1024, 1048576)]
    [int]$MemoryMiB = 8192,
    [ValidateRange(1, 65535)]
    [int]$QmpPortStart = 4445,
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Test-ProcessRunning {
    param([int]$ProcessId)

    if ($ProcessId -le 0) {
        return $false
    }
    return $null -ne (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)
}

function Read-AttemptMetadata {
    param([System.IO.DirectoryInfo]$Directory)

    $metadataPath = Join-Path $Directory.FullName 'attempt.json'
    if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    } catch {
        Write-Warning "Ignoring invalid attempt metadata: $metadataPath"
        return $null
    }
}

function Get-Attempts {
    param([string]$Root)

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        return @()
    }
    return @(
        Get-ChildItem -LiteralPath $Root -Directory | ForEach-Object {
            $metadata = Read-AttemptMetadata -Directory $_
            if ($null -ne $metadata) {
                $pidValue = 0
                if ($null -ne $metadata.pid) {
                    $pidValue = [int]$metadata.pid
                }
                [pscustomobject]@{
                    AttemptId = $_.Name
                    Status = if (Test-ProcessRunning -ProcessId $pidValue) { 'running' } else { 'stopped' }
                    PID = $pidValue
                    QMP = "127.0.0.1:$($metadata.qmpPort)"
                    CreatedUtc = $metadata.createdUtc
                    Directory = $_.FullName
                }
            }
        }
    )
}

$stateRoot = [System.IO.Path]::GetFullPath($StateDirectory)

if ($Action -eq 'List') {
    if ($CheckOnly -or $AttemptId -or $AllStopped) {
        throw '-Action List does not accept -CheckOnly, -AttemptId or -AllStopped.'
    }
    $attempts = @(Get-Attempts -Root $stateRoot)
    if ($attempts.Count -eq 0) {
        Write-Host "No boot attempts under $stateRoot"
        return
    }
    $attempts | Sort-Object CreatedUtc -Descending | Format-Table AttemptId, Status, PID, QMP, CreatedUtc, Directory -AutoSize
    return
}

if ($Action -eq 'Cleanup') {
    if ($CheckOnly) {
        throw '-CheckOnly is only valid with -Action Start.'
    }
    $hasAttemptId = -not [string]::IsNullOrWhiteSpace($AttemptId)
    if ($hasAttemptId -eq $AllStopped.IsPresent) {
        throw 'Choose exactly one cleanup scope: -AttemptId <id> or -AllStopped.'
    }

    $attempts = @(Get-Attempts -Root $stateRoot)
    $selected = @(
        if ($AllStopped) {
            $attempts | Where-Object Status -eq 'stopped'
        } else {
            $attempts | Where-Object AttemptId -eq $AttemptId
        }
    )
    if (-not $AllStopped -and $selected.Count -eq 0) {
        throw "Boot attempt does not exist: $AttemptId"
    }
    $running = @($selected | Where-Object Status -eq 'running')
    if ($running.Count -gt 0) {
        throw "Refusing to delete a running attempt: $($running.AttemptId -join ', ')"
    }
    foreach ($attempt in $selected) {
        $resolvedRoot = [System.IO.Path]::GetFullPath($stateRoot).TrimEnd('\') + '\'
        $resolvedAttempt = [System.IO.Path]::GetFullPath($attempt.Directory)
        if (-not $resolvedAttempt.StartsWith($resolvedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to clean a path outside the iteration state directory: $resolvedAttempt"
        }
        Remove-Item -LiteralPath $resolvedAttempt -Recurse -Force
        Write-Host "Deleted stopped boot attempt $($attempt.AttemptId)"
    }
    if ($selected.Count -eq 0) {
        Write-Host 'No stopped boot attempts to delete.'
    }
    return
}

if ($AttemptId -or $AllStopped) {
    throw '-AttemptId and -AllStopped are only valid with -Action Cleanup.'
}

if (-not $DiskPath) {
    if ($env:TILO_ARM64_DISK) {
        $DiskPath = $env:TILO_ARM64_DISK
    } else {
        $linuxHome = (& wsl.exe --distribution $WslDistribution -- sh -lc 'printf %s "$HOME"') -join ''
        if ($LASTEXITCODE -ne 0 -or -not $linuxHome.StartsWith('/')) {
            throw "Could not discover the home directory in WSL distribution '$WslDistribution'. Pass -DiskPath explicitly."
        }
        $linuxDisk = "$linuxHome/tilo-vm/media/disk.raw"
        $DiskPath = "\\wsl.localhost\$WslDistribution$($linuxDisk.Replace('/', '\'))"
    }
}

$qemuExe = Join-Path $QemuRoot 'bin\qemu-system-aarch64.exe'
$qemuImgExe = Join-Path $QemuRoot 'bin\qemu-img.exe'
$firmwareCode = Join-Path $QemuRoot 'share\qemu\edk2-aarch64-code.fd'
$firmwareVarsTemplate = Join-Path $QemuRoot 'share\qemu\edk2-arm-vars.fd'

foreach ($requiredFile in @($qemuExe, $qemuImgExe, $firmwareCode, $firmwareVarsTemplate)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "Required file is missing: $requiredFile"
    }
}

$qemuVersionOutput = @(& $qemuExe --version)
if ($LASTEXITCODE -ne 0) {
    throw "Could not execute QEMU: $qemuExe"
}
$qemuVersion = $qemuVersionOutput[0]

$accelerators = (& $qemuExe -accel help) -join "`n"
if ($LASTEXITCODE -ne 0 -or $accelerators -notmatch '(?m)^whpx$') {
    throw 'This QEMU build does not advertise the WHPX accelerator.'
}

$displays = (& $qemuExe -display help 2>&1) -join "`n"
if ($LASTEXITCODE -ne 0 -or $displays -notmatch '(?m)^gtk$') {
    throw 'This QEMU build does not advertise the GTK display backend.'
}

$codeSize = (Get-Item -LiteralPath $firmwareCode).Length
$varsSize = (Get-Item -LiteralPath $firmwareVarsTemplate).Length
if ($codeSize -ne 64MB -or $varsSize -ne 64MB) {
    throw "Unexpected pflash sizes: code=$codeSize bytes, vars=$varsSize bytes (expected 67108864 each)."
}

$diskExists = Test-Path -LiteralPath $DiskPath -PathType Leaf
$diskHasGpt = $false
$diskLength = 0
if ($diskExists) {
    $diskStream = [System.IO.File]::Open($DiskPath, 'Open', 'Read', 'ReadWrite')
    try {
        $diskLength = $diskStream.Length
        if ($diskLength -ge 1024) {
            $header = [byte[]]::new(8)
            $diskStream.Position = 512
            if ($diskStream.Read($header, 0, $header.Length) -eq $header.Length) {
                $diskHasGpt = [System.Text.Encoding]::ASCII.GetString($header) -eq 'EFI PART'
            }
        }
    } finally {
        $diskStream.Dispose()
    }
}

if ($CheckOnly) {
    Write-Host "QEMU:        $qemuVersion"
    Write-Host 'Accelerator: WHPX'
    Write-Host 'Display:     GTK + virtio-gpu-pci'
    Write-Host "qemu-img:    $qemuImgExe"
    Write-Host "Firmware:    $firmwareCode"
    Write-Host "Vars source: $firmwareVarsTemplate"
    Write-Host "State root:  $stateRoot"
    if (-not $diskExists) {
        throw "Raw base disk is missing: $DiskPath"
    }
    if (-not $diskHasGpt) {
        throw "Raw base disk is blank or non-GPT ($diskLength bytes): $DiskPath"
    }
    Write-Host "Base disk:   $DiskPath [raw, GPT, $diskLength bytes; boots use disposable overlays]"
    return
}

if (-not $diskExists) {
    throw "Raw base disk is missing; no boot attempt was created: $DiskPath"
}
if (-not $diskHasGpt) {
    throw "Raw base disk is blank or non-GPT; no boot attempt was created: $DiskPath"
}

$attemptTimestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfffZ')
$attemptName = "$attemptTimestamp-$([guid]::NewGuid().ToString('N').Substring(0, 6))"
$attemptDir = Join-Path $stateRoot $attemptName
$overlayPath = Join-Path $attemptDir 'disk.qcow2'
$firmwareVars = Join-Path $attemptDir 'edk2-arm-vars.fd'
$serialLog = Join-Path $attemptDir 'serial.log'
$metadataPath = Join-Path $attemptDir 'attempt.json'

$qmpPort = $null
for ($candidate = $QmpPortStart; $candidate -le [Math]::Min(65535, $QmpPortStart + 199); $candidate++) {
    $listener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $candidate -State Listen -ErrorAction SilentlyContinue
    if (-not $listener) {
        $qmpPort = $candidate
        break
    }
}
if ($null -eq $qmpPort) {
    throw "No free QMP port found in range $QmpPortStart-$([Math]::Min(65535, $QmpPortStart + 199))."
}

New-Item -ItemType Directory -Path $attemptDir -Force | Out-Null
$process = $null
try {
    Copy-Item -LiteralPath $firmwareVarsTemplate -Destination $firmwareVars
    if ((Get-Item -LiteralPath $firmwareVars).Length -ne $varsSize) {
        throw "The fresh UEFI variables copy has the wrong size: $firmwareVars"
    }

    & $qemuImgExe create -f qcow2 -F raw -b $DiskPath -o 'cluster_size=2M,lazy_refcounts=on' $overlayPath
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $overlayPath -PathType Leaf)) {
        throw "qemu-img could not create the disposable overlay: $overlayPath"
    }

    $overlayInfo = (& $qemuImgExe info --output=json $overlayPath) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw "qemu-img could not inspect the disposable overlay: $overlayPath"
    }
    $parsedOverlay = $overlayInfo | ConvertFrom-Json
    if ($parsedOverlay.format -ne 'qcow2' -or $parsedOverlay.'backing-filename-format' -ne 'raw') {
        throw "Unexpected disposable overlay metadata: $overlayInfo"
    }

    [System.IO.File]::Open($serialLog, 'CreateNew', 'Write', 'ReadWrite').Dispose()

    $qemuArgs = @(
        '-name', "tilo-arm64-$attemptName"
        '-machine', 'virt,gic-version=3,highmem=on'
        '-accel', 'whpx'
        '-cpu', 'host'
        '-smp', "cpus=$CpuCount"
        '-m', "$MemoryMiB"
        '-drive', "if=pflash,format=raw,readonly=on,file=$firmwareCode"
        '-drive', "if=pflash,format=raw,file=$firmwareVars"
        '-drive', "if=none,id=osdisk,format=qcow2,file=$overlayPath,cache=writeback,aio=threads"
        '-device', 'virtio-blk-pci,drive=osdisk,bootindex=1'
        '-device', 'virtio-gpu-pci'
        '-display', 'gtk,show-cursor=on'
        '-device', 'qemu-xhci'
        '-device', 'usb-kbd'
        '-device', 'usb-tablet'
        '-netdev', 'user,id=net0'
        '-device', 'virtio-net-pci,netdev=net0'
        '-chardev', "file,id=serial0,path=$serialLog,append=on"
        '-serial', 'chardev:serial0'
        '-monitor', 'none'
        '-qmp', "tcp:127.0.0.1:$qmpPort,server=on,wait=off"
        '-boot', 'menu=on'
    )

    $process = Start-Process -FilePath $qemuExe -ArgumentList $qemuArgs -PassThru
    Start-Sleep -Milliseconds 500
    if ($process.HasExited) {
        throw "QEMU exited immediately with status $($process.ExitCode). See $serialLog"
    }

    $metadata = [ordered]@{
        schemaVersion = 1
        attemptId = $attemptName
        createdUtc = (Get-Date).ToUniversalTime().ToString('o')
        pid = $process.Id
        qmpPort = $qmpPort
        baseDisk = $DiskPath
        baseLength = $diskLength
        overlay = $overlayPath
        firmwareVars = $firmwareVars
        serialLog = $serialLog
        qemu = $qemuExe
        qemuVersion = $qemuVersion
        cpuCount = $CpuCount
        memoryMiB = $MemoryMiB
    }
    $metadata | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $metadataPath -Encoding utf8

    Write-Host "Started disposable Tilo boot attempt $attemptName"
    Write-Host "PID:        $($process.Id)"
    Write-Host "Base disk:  $DiskPath [opened through qcow2 backing file]"
    Write-Host "Overlay:    $overlayPath"
    Write-Host "UEFI vars:  $firmwareVars"
    Write-Host "Serial log: $serialLog"
    Write-Host "QMP:        tcp:127.0.0.1:$qmpPort"
    Write-Host "Cleanup:    $($MyInvocation.MyCommand.Path) -Action Cleanup -AttemptId $attemptName"
} catch {
    if ($null -ne $process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(5000)
    }
    if (Test-Path -LiteralPath $attemptDir -PathType Container) {
        Remove-Item -LiteralPath $attemptDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    throw
}
