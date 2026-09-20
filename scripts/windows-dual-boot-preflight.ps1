#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

function Invoke-Observed {
    param([Parameter(Mandatory = $true)][scriptblock]$Script)

    try {
        [ordered]@{ ok = $true; value = (& $Script) }
    }
    catch {
        [ordered]@{ ok = $false; error = $_.Exception.Message }
    }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this read-only preflight from an administrator PowerShell.'
}

$partition = Get-Partition -DriveLetter C
$volume = Get-Volume -DriveLetter C
$limits = $partition | Get-PartitionSupportedSize
$memory = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management'
$sessionManager = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager'
$power = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Power'
$boot = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power'
$os = Get-CimInstance Win32_OperatingSystem
$computer = Get-CimInstance Win32_ComputerSystem

$hiberfile = Get-Item C:\hiberfil.sys -Force -ErrorAction SilentlyContinue
$pagefile = Get-Item C:\pagefile.sys -Force -ErrorAction SilentlyContinue
$swapfile = Get-Item C:\swapfile.sys -Force -ErrorAction SilentlyContinue
$lastShrinkEvent = Get-WinEvent -FilterHashtable @{
    LogName = 'Application'
    ProviderName = 'Microsoft-Windows-Defrag'
    Id = 259
} -MaxEvents 1 -ErrorAction SilentlyContinue

$result = [ordered]@{
    schema = 'tilo.windows-dual-boot-preflight.v1'
    observed_at_utc = [DateTime]::UtcNow.ToString('o')
    read_only = $true
    administrator = $true
    reboot = [ordered]@{
        last_boot_at_utc = $os.LastBootUpTime.ToUniversalTime().ToString('o')
        pending_file_rename_operations = @($sessionManager.PendingFileRenameOperations)
    }
    disk = [ordered]@{
        number = [int]$partition.DiskNumber
        partition_number = [int]$partition.PartitionNumber
        partition_style = (Get-Disk -Number $partition.DiskNumber).PartitionStyle.ToString()
        partition_size_bytes = [long]$partition.Size
        partition_offset_bytes = [long]$partition.Offset
        volume_size_bytes = [long]$volume.Size
        volume_free_bytes = [long]$volume.SizeRemaining
        file_system = [string]$volume.FileSystem
        health_status = $volume.HealthStatus.ToString()
        supported_minimum_size_bytes = [long]$limits.SizeMin
        supported_maximum_size_bytes = [long]$limits.SizeMax
        supported_shrink_bytes = [long]$partition.Size - [long]$limits.SizeMin
        last_shrink_analysis_event = if ($lastShrinkEvent) {
            [ordered]@{
                created_at_utc = $lastShrinkEvent.TimeCreated.ToUniversalTime().ToString('o')
                message = $lastShrinkEvent.Message
            }
        } else { $null }
    }
    paging = [ordered]@{
        automatic_managed = [bool]$computer.AutomaticManagedPagefile
        configured_registry_entries = @($memory.PagingFiles)
        settings = @(Get-CimInstance Win32_PageFileSetting | Select-Object Name, InitialSize, MaximumSize)
        active = @(Get-CimInstance Win32_PageFileUsage | Select-Object Name, AllocatedBaseSize, CurrentUsage, PeakUsage, TempPageFile)
        file = if ($pagefile) { [ordered]@{ path = $pagefile.FullName; length_bytes = [long]$pagefile.Length } } else { $null }
    }
    hibernation = [ordered]@{
        enabled = [bool]$hiberfile
        registry_enabled = $power.HibernateEnabled
        file_type = $power.HiberFileType
        file_size_percent = $power.HiberFileSizePercent
        fast_startup_enabled = [bool]$boot.HiberbootEnabled
        file = if ($hiberfile) { [ordered]@{ path = $hiberfile.FullName; length_bytes = [long]$hiberfile.Length } } else { $null }
    }
    swap_file = if ($swapfile) { [ordered]@{ path = $swapfile.FullName; length_bytes = [long]$swapfile.Length } } else { $null }
    encryption = Invoke-Observed {
        Get-BitLockerVolume -MountPoint C: |
            Select-Object MountPoint, VolumeStatus, ProtectionStatus, EncryptionPercentage, LockStatus, EncryptionMethod
    }
    secure_boot = Invoke-Observed { [bool](Confirm-SecureBootUEFI) }
    windows_recovery = Invoke-Observed {
        $recoveryOutput = (& reagentc.exe /info 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) { throw "reagentc exited with code $LASTEXITCODE`: $recoveryOutput" }
        $recoveryOutput
    }
    shadow_storage = Invoke-Observed {
        @(Get-CimInstance Win32_ShadowStorage | Select-Object Volume, DiffVolume, UsedSpace, AllocatedSpace, MaxSpace)
    }
    shadow_copies = Invoke-Observed {
        @(Get-CimInstance Win32_ShadowCopy | Select-Object ID, InstallDate, VolumeName, State, Persistent, ClientAccessible)
    }
}

$json = $result | ConvertTo-Json -Depth 10
if ($OutputPath) {
    $resolvedParent = Split-Path -Parent $OutputPath
    if ($resolvedParent -and -not (Test-Path -LiteralPath $resolvedParent -PathType Container)) {
        throw "Output directory does not exist: $resolvedParent"
    }
    $temporary = "$OutputPath.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
        Move-Item -LiteralPath $temporary -Destination $OutputPath -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
    }
}

$json
