#Requires -Version 5.1
[CmdletBinding(DefaultParameterSetName = 'Report')]
param(
    [Parameter(ParameterSetName = 'Report')]
    [Parameter(ParameterSetName = 'Prepare')]
    [string]$OutputPath = '.\windows-dual-boot-preflight.json',

    [Parameter(Mandatory, ParameterSetName = 'Prepare')]
    [switch]$PrepareShrink,

    [Parameter(Mandatory, ParameterSetName = 'Prepare')]
    [string]$CheckpointPath,

    [Parameter(Mandatory, ParameterSetName = 'Restore')]
    [switch]$RestoreWindowsState,

    [Parameter(Mandatory, ParameterSetName = 'Restore')]
    [string]$RestoreCheckpointPath,

    [Int64]$TargetWindowsBytes = 858993459200,
    [Int64]$RequiredPostShrinkFreeBytes = 42949672960
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ExpectedDiskNumber = 0
$ExpectedDiskSize = 1024209543168L
$ExpectedDiskGuid = '{e6a469fd-e19b-4974-ad23-9c234a4e225a}'
$ExpectedWindowsPartitionGuid = '{fa6ad2c9-bd0e-4780-9d68-324b93376d01}'

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-CapturedCommand {
    param([Parameter(Mandatory)][scriptblock]$Command)
    try {
        return [ordered]@{ Succeeded = $true; Output = (& $Command 2>&1 | Out-String).Trim() }
    } catch {
        return [ordered]@{ Succeeded = $false; Output = $_.Exception.Message }
    }
}

function Invoke-NativeCapturedCommand {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @()
    )
    try {
        $output = (& $FilePath @Arguments 2>&1 | Out-String).Trim()
        $exitCode = $LASTEXITCODE
        return [ordered]@{ Succeeded = ($exitCode -eq 0); ExitCode = $exitCode; Output = $output }
    } catch {
        return [ordered]@{ Succeeded = $false; ExitCode = $null; Output = $_.Exception.Message }
    }
}

function Get-PagingState {
    $memory = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management'
    $computer = Get-CimInstance Win32_ComputerSystem
    $usage = @(Get-CimInstance Win32_PageFileUsage -ErrorAction SilentlyContinue | ForEach-Object {
        [ordered]@{
            Name = $_.Name
            AllocatedBaseSizeMiB = $_.AllocatedBaseSize
            CurrentUsageMiB = $_.CurrentUsage
            PeakUsageMiB = $_.PeakUsage
        }
    })
    return [ordered]@{
        AutomaticManagedPagefile = [bool]$computer.AutomaticManagedPagefile
        PagingFiles = @($memory.PagingFiles)
        ExistingPageFiles = @($memory.ExistingPageFiles)
        Usage = $usage
    }
}

function Assert-ExpectedWindowsDisk {
    $disk = Get-Disk -Number $ExpectedDiskNumber
    $partition = Get-Partition -DriveLetter C
    if ($disk.Size -ne $ExpectedDiskSize -or $disk.Guid -ne $ExpectedDiskGuid) {
        throw 'Internal disk identity changed; refusing Windows preparation.'
    }
    if ($partition.Guid -ne $ExpectedWindowsPartitionGuid) {
        throw 'C: partition identity changed; refusing Windows preparation.'
    }
    return [ordered]@{ Disk = $disk; Partition = $partition }
}

function Write-JsonFile {
    param([Parameter(Mandatory)]$Value, [Parameter(Mandatory)][string]$Path)
    $fullPath = [IO.Path]::GetFullPath($Path)
    $parent = Split-Path -Parent $fullPath
    if ($parent -and -not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    $temporary = "$fullPath.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
        Move-Item -LiteralPath $temporary -Destination $fullPath -Force
    } finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
    }
    return $fullPath
}

function Get-ReadinessReport {
    $identity = Assert-ExpectedWindowsDisk
    $partition = $identity.Partition
    $volume = Get-Volume -DriveLetter C
    $supported = $null
    $supportedError = $null
    try {
        $supported = $partition | Get-PartitionSupportedSize -ErrorAction Stop
    } catch {
        $supportedError = $_.Exception.Message
    }
    $latestShrinkEvent = Get-WinEvent -FilterHashtable @{ LogName = 'Application'; ProviderName = 'Microsoft-Windows-Defrag'; Id = 259 } -MaxEvents 1 -ErrorAction SilentlyContinue
    $requiredShrink = [Math]::Max(0L, [Int64]$partition.Size - $TargetWindowsBytes)
    $requiredFree = $requiredShrink + $RequiredPostShrinkFreeBytes
    return [ordered]@{
        Schema = 'tilo.windows-shrink-readiness.v1'
        RecordedAt = (Get-Date).ToUniversalTime().ToString('o')
        Elevated = Test-IsAdministrator
        TargetWindowsBytes = $TargetWindowsBytes
        RequiredPostShrinkFreeBytes = $RequiredPostShrinkFreeBytes
        Disk = [ordered]@{ Number = $identity.Disk.Number; Guid = $identity.Disk.Guid; Size = [Int64]$identity.Disk.Size; UniqueId = $identity.Disk.UniqueId }
        WindowsPartition = [ordered]@{ Number = $partition.PartitionNumber; Guid = $partition.Guid; Offset = [Int64]$partition.Offset; Size = [Int64]$partition.Size }
        Volume = [ordered]@{
            Size = [Int64]$volume.Size
            FreeBytes = [Int64]$volume.SizeRemaining
            RequiredShrinkBytes = $requiredShrink
            RequiredFreeBytes = $requiredFree
            AdditionalFreeBytesNeeded = [Math]::Max(0L, $requiredFree - [Int64]$volume.SizeRemaining)
        }
        SupportedSize = if ($null -ne $supported) { [ordered]@{ SizeMin = [Int64]$supported.SizeMin; SizeMax = [Int64]$supported.SizeMax; TargetAdmitted = ([Int64]$supported.SizeMin -le $TargetWindowsBytes) } } else { $null }
        SupportedSizeError = $supportedError
        Paging = Get-PagingState
        PageFile = if (Test-Path -LiteralPath 'C:\pagefile.sys') { [ordered]@{ Path = 'C:\pagefile.sys'; Length = [Int64](Get-Item -LiteralPath 'C:\pagefile.sys' -Force).Length } } else { $null }
        Hibernation = Invoke-NativeCapturedCommand -FilePath 'powercfg.exe' -Arguments @('/a')
        FastStartupPreference = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power' -ErrorAction SilentlyContinue).HiberbootEnabled
        LatestShrinkAnalysis = if ($null -ne $latestShrinkEvent) { [ordered]@{ TimeCreated = $latestShrinkEvent.TimeCreated.ToUniversalTime().ToString('o'); RecordId = $latestShrinkEvent.RecordId; Message = $latestShrinkEvent.Message } } else { $null }
        BitLocker = Invoke-NativeCapturedCommand -FilePath 'manage-bde.exe' -Arguments @('-status', 'C:')
        BitLockerProtectors = Invoke-NativeCapturedCommand -FilePath 'manage-bde.exe' -Arguments @('-protectors', '-get', 'C:')
        SecureBoot = Invoke-CapturedCommand { Confirm-SecureBootUEFI }
        WinRE = Invoke-NativeCapturedCommand -FilePath 'reagentc.exe' -Arguments @('/info')
        ShadowStorage = Invoke-NativeCapturedCommand -FilePath 'vssadmin.exe' -Arguments @('list', 'shadowstorage', '/for=C:')
        ShadowCopies = Invoke-CapturedCommand { Get-CimInstance Win32_ShadowCopy | Select-Object ID, InstallDate, VolumeName, State, Persistent, ClientAccessible | Format-List }
        UsnJournal = Invoke-NativeCapturedCommand -FilePath 'fsutil.exe' -Arguments @('usn', 'queryjournal', 'C:')
        NtfsInfo = Invoke-NativeCapturedCommand -FilePath 'fsutil.exe' -Arguments @('fsinfo', 'ntfsinfo', 'C:')
        CheckDisk = Invoke-NativeCapturedCommand -FilePath 'chkdsk.exe' -Arguments @('C:', '/scan')
    }
}

if ($PSCmdlet.ParameterSetName -eq 'Prepare') {
    if (-not (Test-IsAdministrator)) { throw 'PrepareShrink requires an elevated PowerShell.' }
    Assert-ExpectedWindowsDisk | Out-Null
    $activeBuilder = @(Get-Process -Name 'qemu-system-aarch64','qemu-system-x86_64' -ErrorAction SilentlyContinue)
    if ($activeBuilder.Count -gt 0) { throw 'A QEMU VM is still active. Finish the local Tilo boot work before preparing Windows for reboot.' }
    $checkpointFullPath = [IO.Path]::GetFullPath($CheckpointPath)
    if (Test-Path -LiteralPath $checkpointFullPath) { throw "Checkpoint already exists: $checkpointFullPath" }
    $checkpoint = [ordered]@{
        Schema = 'tilo.windows-shrink-checkpoint.v1'
        RecordedAt = (Get-Date).ToUniversalTime().ToString('o')
        Paging = Get-PagingState
        HibernationEnabled = [bool](Test-Path -LiteralPath 'C:\hiberfil.sys')
    }
    Write-JsonFile -Value $checkpoint -Path $checkpointFullPath | Out-Null
    powercfg /hibernate off
    $computer = Get-CimInstance Win32_ComputerSystem
    Set-CimInstance -InputObject $computer -Property @{ AutomaticManagedPagefile = $false } | Out-Null
    Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management' -Name PagingFiles -Type MultiString -Value @()
    Write-Host 'Prepared reversible paging/hibernation state. Reboot before measuring SizeMin.'
    Write-Host "Restore after the shrink with: -RestoreWindowsState -RestoreCheckpointPath '$checkpointFullPath'"
    exit 0
}

if ($PSCmdlet.ParameterSetName -eq 'Restore') {
    if (-not (Test-IsAdministrator)) { throw 'RestoreWindowsState requires an elevated PowerShell.' }
    Assert-ExpectedWindowsDisk | Out-Null
    $checkpoint = Get-Content -LiteralPath $RestoreCheckpointPath -Raw | ConvertFrom-Json
    if ($checkpoint.Schema -ne 'tilo.windows-shrink-checkpoint.v1') { throw 'Checkpoint schema is not recognized.' }
    $computer = Get-CimInstance Win32_ComputerSystem
    Set-CimInstance -InputObject $computer -Property @{ AutomaticManagedPagefile = [bool]$checkpoint.Paging.AutomaticManagedPagefile } | Out-Null
    $pagingFiles = @($checkpoint.Paging.PagingFiles)
    Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management' -Name PagingFiles -Type MultiString -Value $pagingFiles
    if ([bool]$checkpoint.HibernationEnabled) { powercfg /hibernate on } else { powercfg /hibernate off }
    Write-Host 'Restored the recorded paging and hibernation configuration. Reboot to activate it.'
    exit 0
}

$result = Get-ReadinessReport
$written = Write-JsonFile -Value $result -Path $OutputPath
Write-Host "Windows shrink readiness written to $written"
if ($null -eq $result.SupportedSize) {
    Write-Warning 'SizeMin was unavailable. Re-run from an elevated PowerShell before resizing.'
    exit 2
}
if (-not $result.SupportedSize.TargetAdmitted -or $result.Volume.AdditionalFreeBytesNeeded -gt 0) { exit 3 }
