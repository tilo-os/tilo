<#
.SYNOPSIS
Boots a Tilo ARM64 raw disk with native Windows QEMU and WHPX.

.DESCRIPTION
Uses the MSYS2 CLANGARM64 QEMU package and a persistent writable UEFI variable
store. By default, the disk is discovered at $HOME/tilo-vm/media/disk.raw in
the selected WSL distribution. Set TILO_ARM64_DISK or pass -DiskPath to use a
different artifact. Run with -CheckOnly to validate the host without booting.

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -CheckOnly

.EXAMPLE
.\tools\launch-arm64-disk.ps1 -ResetVars
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
    [switch]$ResetVars,
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

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
$serialProbe = [System.IO.File]::Open($serialLog, 'OpenOrCreate', 'Write', 'ReadWrite')
$serialProbe.Dispose()

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
    Write-Host "Disk:        $DiskPath [$diskStatus]"
    Write-Host "Serial log:  $serialLog [writable]"
    Write-Host "QMP:         tcp:127.0.0.1:$QmpPort [available]"
    return
}

if (-not $diskExists) {
    throw "Raw disk is not present yet; QEMU was not started: $DiskPath"
}
if (-not $diskHasGpt) {
    throw "Raw disk has no GPT header and is not an assembled boot target; QEMU was not started: $DiskPath"
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
    '-drive', "if=none,id=osdisk,format=raw,file=$DiskPath,cache=writeback,aio=threads"
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
    '-qmp', "tcp:127.0.0.1:$QmpPort,server=on,wait=off"
    '-boot', 'menu=on'
)

Write-Host "Starting $qemuVersion with WHPX"
Write-Host "Disk:       $DiskPath"
Write-Host "Serial log: $serialLog"
Write-Host "QMP:        tcp:127.0.0.1:$QmpPort"
& $qemuExe @qemuArgs
if ($LASTEXITCODE -ne 0) {
    throw "QEMU exited with status $LASTEXITCODE. See $serialLog"
}
