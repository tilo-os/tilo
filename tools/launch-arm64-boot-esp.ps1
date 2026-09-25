<#
.SYNOPSIS
Boot an ARM64 base disk through a small replaceable ESP with Windows QEMU/WHPX.

.DESCRIPTION
The base disk uses QEMU's transient snapshot mode, so early boot can write while
the source raw disk remains unchanged. The override ESP is attached read-only
with the first boot index. Supplying -InstallTargetDisk attaches an isolated,
fresh writable target with the stable `tilo-install-target` serial so the live
installer can be exercised without modifying the source disk. Pass -CheckOnly
to validate inputs without launching.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$BaseDisk,
    [Parameter(Mandatory)] [string]$EspDisk,
    [string]$InstallTargetDisk,
    [ValidateRange(34, 4096)] [int]$InstallTargetGiB = 64,
    [switch]$CreateInstallTarget,
    [switch]$ResetInstallTarget,
    [string]$QemuRoot = (Join-Path $env:USERPROFILE 'msys64\clangarm64'),
    [string]$StateDirectory = (Join-Path $env:LOCALAPPDATA 'Tilo\qemu-arm64-esp'),
    [ValidateRange(1, 256)] [int]$CpuCount = 8,
    [ValidateRange(1024, 1048576)] [int]$MemoryMiB = 8192,
    [ValidateRange(1, 65535)] [int]$QmpPort = 4445,
    [switch]$ResetVars,
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-GptDisk([string]$Path, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Name is missing: $Path"
    }
    $stream = [System.IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try {
        if ($stream.Length -lt 1024) { throw "$Name is too small: $Path" }
        $header = [byte[]]::new(8)
        $stream.Position = 512
        if ($stream.Read($header, 0, 8) -ne 8 -or
            [System.Text.Encoding]::ASCII.GetString($header) -ne 'EFI PART') {
            throw "$Name has no GPT header: $Path"
        }
    } finally { $stream.Dispose() }
}

function Test-QemuUsesPath([string]$Path) {
    $normalized = [System.IO.Path]::GetFullPath($Path)
    $escaped = [regex]::Escape($normalized)
    $processes = @(Get-CimInstance Win32_Process -Filter "Name='qemu-system-aarch64.exe'" |
        Where-Object { $_.CommandLine -match $escaped })
    return $processes
}

Assert-GptDisk $BaseDisk 'Base disk'
Assert-GptDisk $EspDisk 'Override ESP disk'

$qemuExe = Join-Path $QemuRoot 'bin\qemu-system-aarch64.exe'
$qemuImg = Join-Path $QemuRoot 'bin\qemu-img.exe'
$firmwareCode = Join-Path $QemuRoot 'share\qemu\edk2-aarch64-code.fd'
$firmwareVarsTemplate = Join-Path $QemuRoot 'share\qemu\edk2-arm-vars.fd'
foreach ($path in @($qemuExe, $firmwareCode, $firmwareVarsTemplate)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required file is missing: $path" }
}

$targetIsRequested = -not [string]::IsNullOrWhiteSpace($InstallTargetDisk)
if ($ResetInstallTarget -and -not $targetIsRequested) {
    throw '-ResetInstallTarget requires -InstallTargetDisk.'
}
if ($CreateInstallTarget -and -not $targetIsRequested) {
    throw '-CreateInstallTarget requires -InstallTargetDisk.'
}
if ($targetIsRequested) {
    $InstallTargetDisk = [System.IO.Path]::GetFullPath($InstallTargetDisk)
    foreach ($protectedPath in @($BaseDisk, $EspDisk)) {
        if ($InstallTargetDisk -ieq [System.IO.Path]::GetFullPath($protectedPath)) {
            throw "Installer target must differ from the protected boot input: $protectedPath"
        }
    }
    $targetUsers = @(Test-QemuUsesPath $InstallTargetDisk)
    if ($targetUsers.Count -ne 0) {
        throw "Installer target is in use by QEMU PID(s): $($targetUsers.ProcessId -join ', ')."
    }
    $targetNeedsCreation = $ResetInstallTarget -or -not (Test-Path -LiteralPath $InstallTargetDisk -PathType Leaf)
    if ($targetNeedsCreation) {
        if (-not $CreateInstallTarget) {
            throw "Installer target is absent or reset was requested. Pass -CreateInstallTarget to create a fresh ${InstallTargetGiB}GiB sparse raw disk: $InstallTargetDisk"
        }
        if (-not (Test-Path -LiteralPath $qemuImg -PathType Leaf)) {
            throw "qemu-img is required to create the installer target: $qemuImg"
        }
    } else {
        $targetLength = (Get-Item -LiteralPath $InstallTargetDisk).Length
        if ($targetLength -lt (34GB)) {
            throw "Installer target is too small ($targetLength bytes); it must be at least 34 GiB."
        }
    }
}

$accelerators = (& $qemuExe -accel help) -join "`n"
if ($LASTEXITCODE -ne 0 -or $accelerators -notmatch '(?m)^whpx$') {
    throw 'This QEMU build does not advertise WHPX.'
}

$qmpListener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $QmpPort -State Listen -ErrorAction SilentlyContinue
if ($qmpListener) {
    throw "QMP port 127.0.0.1:$QmpPort is already in use by PID $($qmpListener.OwningProcess)."
}

$stateDir = [System.IO.Path]::GetFullPath($StateDirectory)
$firmwareVars = Join-Path $stateDir 'edk2-arm-vars.fd'
$serialLog = Join-Path $stateDir 'serial.log'
New-Item -ItemType Directory -Path $stateDir -Force | Out-Null
if ($ResetVars -and (Test-Path -LiteralPath $firmwareVars)) { Remove-Item -LiteralPath $firmwareVars -Force }
if (-not (Test-Path -LiteralPath $firmwareVars)) {
    Copy-Item -LiteralPath $firmwareVarsTemplate -Destination $firmwareVars
}
if ((Get-Item $firmwareVars).Length -ne (Get-Item $firmwareVarsTemplate).Length) {
    throw "Persistent UEFI variable store has the wrong size: $firmwareVars"
}

$baseBefore = Get-Item -LiteralPath $BaseDisk
$espHash = (Get-FileHash -LiteralPath $EspDisk -Algorithm SHA256).Hash.ToLowerInvariant()
if ($CheckOnly) {
    Write-Host "QEMU:     $((& $qemuExe --version)[0])"
    Write-Host 'Mode:     WHPX, transient base-disk snapshot'
    Write-Host "Base:     $BaseDisk ($($baseBefore.Length) bytes)"
    Write-Host "ESP:      $EspDisk (sha256:$espHash)"
    if ($targetIsRequested) {
        if ($targetNeedsCreation) {
            Write-Host "Target:   $InstallTargetDisk (will create fresh ${InstallTargetGiB}GiB sparse raw disk, serial=tilo-install-target)"
        } else {
            Write-Host "Target:   $InstallTargetDisk ($targetLength bytes, serial=tilo-install-target)"
        }
    } else {
        Write-Host 'Target:   none (early-boot lane)'
    }
    Write-Host "Serial:   $serialLog"
    return
}

if ($targetIsRequested -and $targetNeedsCreation) {
    if (Test-Path -LiteralPath $InstallTargetDisk -PathType Leaf) {
        Remove-Item -LiteralPath $InstallTargetDisk -Force
    }
    $targetParent = Split-Path -Parent $InstallTargetDisk
    if ($targetParent) { New-Item -ItemType Directory -Path $targetParent -Force | Out-Null }
    & $qemuImg create -f raw $InstallTargetDisk "${InstallTargetGiB}G"
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $InstallTargetDisk -PathType Leaf)) {
        throw "Could not create installer target: $InstallTargetDisk"
    }
    $targetLength = (Get-Item -LiteralPath $InstallTargetDisk).Length
    if ($targetLength -lt (34GB)) {
        throw "New installer target is too small ($targetLength bytes); it must be at least 34 GiB."
    }
}

$qemuArgs = @(
    '-name', 'tilo-arm64-early-boot'
    '-machine', 'virt,gic-version=3,highmem=on'
    '-accel', 'whpx'
    '-cpu', 'host'
    '-smp', "cpus=$CpuCount"
    '-m', "$MemoryMiB"
    '-drive', "if=pflash,format=raw,readonly=on,file=$firmwareCode"
    '-drive', "if=pflash,format=raw,file=$firmwareVars"
    '-drive', "if=none,id=esp,format=raw,readonly=on,file=$EspDisk,cache=none"
    '-device', 'virtio-blk-pci,drive=esp,bootindex=1'
    '-drive', "if=none,id=osdisk,format=raw,file=$BaseDisk,snapshot=on,cache=writeback,aio=threads"
    '-device', 'virtio-blk-pci,drive=osdisk,bootindex=2'
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
    '-qmp', "tcp:127.0.0.1:$QmpPort,server=on,wait=off"
    '-boot', 'menu=on'
)

if ($targetIsRequested) {
    $qemuArgs += @(
        '-drive', "if=none,id=installtarget,format=raw,file=$InstallTargetDisk,cache=writeback,aio=threads"
        '-device', 'virtio-blk-pci,drive=installtarget,serial=tilo-install-target'
    )
}

if ($targetIsRequested) {
    Write-Host "Booting installer ESP sha256:$espHash with fresh target $InstallTargetDisk (serial=tilo-install-target)"
} else {
    Write-Host "Booting override ESP sha256:$espHash ahead of immutable source $BaseDisk"
}
& $qemuExe @qemuArgs
if ($LASTEXITCODE -ne 0) { throw "QEMU exited with status $LASTEXITCODE. See $serialLog" }

$baseAfter = Get-Item -LiteralPath $BaseDisk
if ($baseAfter.Length -ne $baseBefore.Length -or $baseAfter.LastWriteTimeUtc -ne $baseBefore.LastWriteTimeUtc) {
    throw 'Base disk metadata changed despite transient snapshot mode.'
}
