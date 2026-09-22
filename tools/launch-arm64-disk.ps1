<#
.SYNOPSIS
Boots a Tilo ARM64 raw disk with native Windows QEMU and WHPX.

.DESCRIPTION
Uses the MSYS2 CLANGARM64 QEMU package and a persistent writable UEFI variable
store. The source disk uses QEMU's transient snapshot mode by default so the
guest can write during boot without modifying the installer artifact. Pass
-WritableSourceDisk only for a disposable installed target that must retain
guest changes across reboots. Writable source mode rejects an installer target
and verifies that no QEMU process or other local file handle already uses the
disk. By default, the disk is discovered at $HOME/tilo-vm/media/disk.raw in
the selected WSL distribution. Set TILO_ARM64_DISK or pass -DiskPath to use a
different artifact. Supplying -InstallTargetDisk attaches a separate writable
disk with the stable `tilo-install-target` serial. Run with -CheckOnly to
validate the host without booting.

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -CheckOnly

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -ResetVars

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -InstallTargetDisk C:\VMs\tilo-target.raw -CreateInstallTarget

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -DiskPath C:\VMs\tilo-installed-target.raw -WritableSourceDisk
#>
[CmdletBinding()]
param(
    [string]$DiskPath,
    [string]$WslDistribution = 'FedoraLinux-42',
    [string]$QemuRoot = (Join-Path $env:USERPROFILE 'msys64\clangarm64'),
    [string]$StateDirectory = (Join-Path $env:LOCALAPPDATA 'Tilo\qemu-arm64'),
    [ValidateRange(1, 256)]
    [int]$CpuCount = 8,
    [ValidateRange(1024, 1048576)]
    [int]$MemoryMiB = 8192,
    [ValidateRange(1, 65535)]
    [int]$QmpPort = 4444,
    [ValidateRange(1, 65535)]
    [int]$SerialTcpPort,
    [ValidateRange(1, 65535)]
    [int]$SshHostPort,
    [switch]$ResetVars,
    [switch]$CheckOnly,
    [switch]$WritableSourceDisk,
    [string]$InstallTargetDisk,
    [ValidateRange(34, 4096)]
    [int]$InstallTargetGiB = 64,
    [switch]$CreateInstallTarget,
    [switch]$ResetInstallTarget
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Test-QemuUsesPath([string]$Path) {
    $normalized = [System.IO.Path]::GetFullPath($Path)
    $escaped = [regex]::Escape($normalized)
    # Match a complete drive-file value, rather than a path prefix. This keeps
    # a disk such as `target.raw.bak` from blocking `target.raw` and ensures
    # the writable-mode guard names the process using this exact source disk.
    $drivePattern = '(?i)(?:^|[,\s])file="?' + $escaped + '"?(?=,|[\s"]|$)'
    $plainPattern = '(?i)(?:^|[\s"])' + $escaped + '(?=[\s"]|$)'
    $processes = @(Get-CimInstance Win32_Process -Filter "Name='qemu-system-aarch64.exe'" |
        Where-Object { $_.CommandLine -match $drivePattern -or $_.CommandLine -match $plainPattern })
    return $processes
}

function Assert-FileIsExclusivelyWritable([string]$Path) {
    $probe = $null
    try {
        # QEMU's source drive is a plain file on this host. Opening it with no
        # sharing detects a conflicting process even when it is not QEMU.
        $probe = [System.IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
    } catch {
        throw "Writable source disk cannot be opened exclusively: $Path. Stop the process using it before launch."
    } finally {
        if ($null -ne $probe) {
            $probe.Dispose()
        }
    }
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
$qemuImg = Join-Path $QemuRoot 'bin\qemu-img.exe'
$firmwareCode = Join-Path $QemuRoot 'share\qemu\edk2-aarch64-code.fd'
$firmwareVarsTemplate = Join-Path $QemuRoot 'share\qemu\edk2-arm-vars.fd'
$stateDir = [System.IO.Path]::GetFullPath($StateDirectory)
$firmwareVars = Join-Path $stateDir 'edk2-arm-vars.fd'
$serialLog = Join-Path $stateDir 'serial.log'

foreach ($requiredFile in @($qemuExe, $firmwareCode, $firmwareVarsTemplate)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "Required file is missing: $requiredFile"
    }
}

$DiskPath = [System.IO.Path]::GetFullPath($DiskPath)
$targetIsRequested = -not [string]::IsNullOrWhiteSpace($InstallTargetDisk)
if ($WritableSourceDisk -and $targetIsRequested) {
    throw '-WritableSourceDisk cannot be combined with -InstallTargetDisk. Use the installer target in the default snapshot-safe mode, then boot that disposable target alone with -WritableSourceDisk.'
}
if ($ResetInstallTarget -and -not $targetIsRequested) {
    throw '-ResetInstallTarget requires -InstallTargetDisk.'
}
if ($CreateInstallTarget -and -not $targetIsRequested) {
    throw '-CreateInstallTarget requires -InstallTargetDisk.'
}
if ($targetIsRequested) {
    $InstallTargetDisk = [System.IO.Path]::GetFullPath($InstallTargetDisk)
    if ($InstallTargetDisk -ieq $DiskPath) {
        throw "Installer target must differ from the protected source disk: $DiskPath"
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

$listener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $QmpPort -State Listen -ErrorAction SilentlyContinue
if ($listener) {
    throw "QMP port 127.0.0.1:$QmpPort is already in use by PID $($listener.OwningProcess)."
}

New-Item -ItemType Directory -Path $stateDir -Force | Out-Null
if ($ResetVars -and (Test-Path -LiteralPath $firmwareVars)) {
    Remove-Item -LiteralPath $firmwareVars -Force
}
$varsWasCopied = $false
if (-not (Test-Path -LiteralPath $firmwareVars -PathType Leaf)) {
    Copy-Item -LiteralPath $firmwareVarsTemplate -Destination $firmwareVars
    $varsWasCopied = $true
}
if ((Get-Item -LiteralPath $firmwareVars).Length -ne $varsSize) {
    throw "Persistent UEFI variables file has the wrong size: $firmwareVars"
}
if ($varsWasCopied) {
    $templateHash = (Get-FileHash -LiteralPath $firmwareVarsTemplate -Algorithm SHA256).Hash
    $copyHash = (Get-FileHash -LiteralPath $firmwareVars -Algorithm SHA256).Hash
    if ($copyHash -ne $templateHash) {
        throw "Persistent UEFI variables copy failed verification: $firmwareVars"
    }
}
$serialEndpoint = if ($SerialTcpPort) {
    if ($SerialTcpPort -eq $QmpPort) {
        throw 'SerialTcpPort must differ from QmpPort.'
    }
    $serialListener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $SerialTcpPort -State Listen -ErrorAction SilentlyContinue
    if ($serialListener) {
        throw "Serial console port 127.0.0.1:$SerialTcpPort is already in use by PID $($serialListener.OwningProcess)."
    }
    "tcp:127.0.0.1:$SerialTcpPort,server=on,wait=off"
} else {
    $serialProbe = [System.IO.File]::Open($serialLog, 'OpenOrCreate', 'Write', 'ReadWrite')
    $serialProbe.Dispose()
    "file:$serialLog"
}
$networkEndpoint = 'user,id=net0'
if ($SshHostPort) {
    if ($SshHostPort -eq $QmpPort -or ($SerialTcpPort -and $SshHostPort -eq $SerialTcpPort)) {
        throw 'SshHostPort must differ from QmpPort and SerialTcpPort.'
    }
    $sshListener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $SshHostPort -State Listen -ErrorAction SilentlyContinue
    if ($sshListener) {
        throw "SSH forwarding port 127.0.0.1:$SshHostPort is already in use by PID $($sshListener.OwningProcess)."
    }
    $networkEndpoint = "user,id=net0,hostfwd=tcp:127.0.0.1:$SshHostPort-:22"
}

$diskExists = Test-Path -LiteralPath $DiskPath -PathType Leaf
$diskHasGpt = $false
if ($diskExists) {
    $diskStream = [System.IO.File]::Open($DiskPath, 'Open', 'Read', 'ReadWrite')
    try {
        if ($diskStream.Length -ge 1024) {
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
if ($WritableSourceDisk) {
    if (-not $diskExists) {
        throw "Writable source disk is missing: $DiskPath"
    }
    $diskUsers = @(Test-QemuUsesPath $DiskPath)
    if ($diskUsers.Count -ne 0) {
        throw "Writable source disk is already in use by QEMU PID(s): $($diskUsers.ProcessId -join ', ')."
    }
    Assert-FileIsExclusivelyWritable $DiskPath
}
$sourceMode = if ($WritableSourceDisk) { 'writable persistent' } else { 'transient snapshot' }
if ($CheckOnly) {
    $diskStatus = if ($diskHasGpt) {
        "present with GPT ($((Get-Item -LiteralPath $DiskPath).Length) bytes)"
    } elseif ($diskExists) {
        "present but blank or non-GPT ($((Get-Item -LiteralPath $DiskPath).Length) bytes); launch mode will refuse to start"
    } else {
        'MISSING; launch mode will refuse to start'
    }
    Write-Host "QEMU:        $qemuVersion"
    Write-Host 'Accelerator: WHPX'
    Write-Host 'Display:     GTK + virtio-gpu-pci'
    Write-Host "Firmware:    $firmwareCode"
    Write-Host "Vars source: $firmwareVarsTemplate"
    Write-Host "Vars copy:   $firmwareVars [ready]"
    Write-Host "Disk:        $DiskPath [$diskStatus; $sourceMode]"
    if ($targetIsRequested) {
        if ($targetNeedsCreation) {
            Write-Host "Target:      $InstallTargetDisk (will create fresh ${InstallTargetGiB}GiB sparse raw disk, serial=tilo-install-target)"
        } else {
            Write-Host "Target:      $InstallTargetDisk ($targetLength bytes, serial=tilo-install-target)"
        }
    } else {
        Write-Host 'Target:      none'
    }
    Write-Host "Serial:      $serialEndpoint"
    if ($SshHostPort) { Write-Host "SSH:         127.0.0.1:$SshHostPort -> guest:22 [available]" }
    Write-Host "QMP:         tcp:127.0.0.1:$QmpPort [available]"
    return
}

if (-not $diskExists) {
    throw "Raw disk is not present yet; QEMU was not started: $DiskPath"
}
if (-not $diskHasGpt) {
    throw "Raw disk has no GPT header and is not an assembled boot target; QEMU was not started: $DiskPath"
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

$diskBefore = Get-Item -LiteralPath $DiskPath
$sourceDrive = if ($WritableSourceDisk) {
    "if=none,id=osdisk,format=raw,file=$DiskPath,cache=writeback,aio=threads"
} else {
    "if=none,id=osdisk,format=raw,file=$DiskPath,snapshot=on,cache=writeback,aio=threads"
}

$qemuArgs = @(
    '-name', 'tilo-arm64-local'
    '-machine', 'virt,gic-version=3,highmem=on'
    '-accel', 'whpx'
    '-cpu', 'host'
    '-smp', "cpus=$CpuCount"
    '-m', "$MemoryMiB"
    '-drive', "if=pflash,format=raw,readonly=on,file=$firmwareCode"
    '-drive', "if=pflash,format=raw,file=$firmwareVars"
    '-drive', $sourceDrive
    '-device', 'virtio-blk-pci,drive=osdisk,bootindex=1'
    '-device', 'virtio-gpu-pci'
    '-display', 'gtk,show-cursor=on'
    '-device', 'qemu-xhci'
    '-device', 'usb-kbd'
    '-device', 'usb-tablet'
    '-netdev', $networkEndpoint
    '-device', 'virtio-net-pci,netdev=net0'
    '-serial', $serialEndpoint
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

Write-Host "Starting $qemuVersion with WHPX"
Write-Host "Disk:       $DiskPath ($sourceMode)"
if ($targetIsRequested) {
    Write-Host "Target:     $InstallTargetDisk (serial=tilo-install-target)"
}
Write-Host "Serial:     $serialEndpoint"
if ($SshHostPort) { Write-Host "SSH:        127.0.0.1:$SshHostPort -> guest:22" }
Write-Host "QMP:        tcp:127.0.0.1:$QmpPort"
& $qemuExe @qemuArgs
if ($LASTEXITCODE -ne 0) {
    throw "QEMU exited with status $LASTEXITCODE. Serial endpoint: $serialEndpoint"
}

if (-not $WritableSourceDisk) {
    $diskAfter = Get-Item -LiteralPath $DiskPath
    if ($diskAfter.Length -ne $diskBefore.Length -or $diskAfter.LastWriteTimeUtc -ne $diskBefore.LastWriteTimeUtc) {
        throw 'Source disk metadata changed despite transient snapshot mode.'
    }
}
