//! PowerShell programs the Windows adapter runs, built as pure strings so
//! tests can pin exactly what an elevated shell would execute.
//!
//! Only values this module validated itself are interpolated: integers, a
//! drive letter `A`–`Z`, and GUIDs re-rendered from [`normalize_guid`]. Every
//! program reaches `powershell.exe` through `-EncodedCommand` (base64 of
//! UTF-16LE), so no Windows command-line quoting rule can reinterpret it.

use super::layout::normalize_guid;

/// Prepended to every program: stop on the first error, no progress stream.
const PRELUDE: &str = "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n";

/// A Windows drive letter, validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveLetter(char);

impl DriveLetter {
    pub fn new(letter: char) -> Option<Self> {
        letter
            .is_ascii_alphabetic()
            .then(|| Self(letter.to_ascii_uppercase()))
    }

    /// From a Windows directory such as `C:\Windows`.
    pub fn of_path(path: &str) -> Option<Self> {
        let mut chars = path.chars();
        let letter = chars.next()?;
        (chars.next() == Some(':'))
            .then(|| Self::new(letter))
            .flatten()
    }

    pub fn letter(self) -> char {
        self.0
    }
}

/// `{guid}` for a PowerShell single-quoted literal, or `None` when `text` is
/// not a GUID.
pub fn ps_guid(text: &str) -> Option<String> {
    normalize_guid(text).map(|guid| format!("{{{guid}}}"))
}

/// Read-only snapshot of the disk holding `drive`: `Get-Disk`, every
/// `Get-Partition` on it, `Get-PartitionSupportedSize` of the Windows volume,
/// and the Shell BitLocker property. Output: one compressed JSON object that
/// [`super::storage::parse_snapshot`] decodes.
pub fn snapshot(drive: DriveLetter) -> String {
    snapshot_program(drive, true)
}

/// Disk identity and geometry only: firmware handoffs do not resize NTFS.
pub fn geometry_snapshot(drive: DriveLetter) -> String {
    snapshot_program(drive, false)
}

fn snapshot_program(drive: DriveLetter, shrink_limits: bool) -> String {
    let letter = drive.letter();
    let limits = if shrink_limits {
        "try { $s = $p | Get-PartitionSupportedSize; $min = [uint64]$s.SizeMin; $max = [uint64]$s.SizeMax } catch { }"
    } else {
        ""
    };
    format!(
        r"{PRELUDE}$p = Get-Partition -DriveLetter {letter}
$d = Get-Disk -Number $p.DiskNumber
$min = $null
$max = $null
{limits}
$bl = $null
try {{ $bl = (New-Object -ComObject Shell.Application).NameSpace('{letter}:\').Self.ExtendedProperty('System.Volume.BitLockerProtection') }} catch {{ }}
$parts = @(Get-Partition -DiskNumber $d.Number | ForEach-Object {{
  [PSCustomObject]@{{ PartitionNumber = [uint32]$_.PartitionNumber; Offset = [uint64]$_.Offset; Size = [uint64]$_.Size; Guid = [string]$_.Guid; GptType = [string]$_.GptType; DriveLetter = [string]$_.DriveLetter; IsBoot = [bool]$_.IsBoot; IsSystem = [bool]$_.IsSystem }}
}})
[PSCustomObject]@{{
  Disk = [PSCustomObject]@{{ Number = [uint32]$d.Number; Guid = [string]$d.Guid; Size = [uint64]$d.Size; LogicalSectorSize = [uint32]$d.LogicalSectorSize; PartitionStyle = [string]$d.PartitionStyle; BusType = [string]$d.BusType; IsBoot = [bool]$d.IsBoot; IsSystem = [bool]$d.IsSystem; IsClustered = [bool]$d.IsClustered; IsReadOnly = [bool]$d.IsReadOnly; IsOffline = [bool]$d.IsOffline }}
  System = [PSCustomObject]@{{ PartitionNumber = [uint32]$p.PartitionNumber; Offset = [uint64]$p.Offset; Size = [uint64]$p.Size; Guid = [string]$p.Guid; SizeMin = $min; SizeMax = $max }}
  BitLockerProtection = $bl
  Partitions = $parts
}} | ConvertTo-Json -Compress -Depth 5
"
    )
}

/// Resize the partition `number` on disk `disk` to exactly `size` bytes after
/// re-proving its identity and the supported range. Prints `{{Size, Guid}}`.
pub fn resize(disk: u32, number: u32, guid: &str, size: u64) -> Option<String> {
    let guid = ps_guid(guid)?;
    Some(format!(
        r"{PRELUDE}$p = Get-Partition -DiskNumber {disk} -PartitionNumber {number}
if ([string]$p.Guid -ne '{guid}') {{ throw 'the partition identity changed' }}
$s = $p | Get-PartitionSupportedSize
if ({size} -lt [uint64]$s.SizeMin -or {size} -gt [uint64]$s.SizeMax) {{ throw 'the requested size is outside what Windows supports' }}
if ([uint64]$p.Size -ne {size}) {{ Resize-Partition -DiskNumber {disk} -PartitionNumber {number} -Size {size} }}
$p = Get-Partition -DiskNumber {disk} -PartitionNumber {number}
[PSCustomObject]@{{ Size = [uint64]$p.Size; Guid = [string]$p.Guid }} | ConvertTo-Json -Compress
"
    ))
}

/// Create a partition at exactly `offset`/`size` with `type_guid`, keep it
/// free of drive letters and mount points, and print it as read back.
///
/// `New-Partition` has no `-NoDefaultDriveLetter` switch (that lives on
/// `Set-Partition`, and only applies to basic-data types); it assigns no
/// letter unless `-AssignDriveLetter` is given, and neither staging type is
/// auto-mounted. Any access path other than the volume GUID path is removed
/// anyway, and the caller re-checks the printed `AccessPaths`.
pub fn create(disk: u32, offset: u64, size: u64, type_guid: &str) -> Option<String> {
    let type_guid = ps_guid(type_guid)?;
    Some(format!(
        r"{PRELUDE}$d = Get-Disk -Number {disk}
if ([string]$d.PartitionStyle -ne 'GPT') {{ throw 'the disk is not GPT' }}
$null = New-Partition -DiskNumber {disk} -Offset {offset} -Size {size} -GptType '{type_guid}'
$found = @(Get-Partition -DiskNumber {disk} | Where-Object {{ [uint64]$_.Offset -eq {offset} -and [uint64]$_.Size -eq {size} }})
if ($found.Count -ne 1) {{ throw 'the new partition is not at the requested extent' }}
$p = $found[0]
try {{ $p | Set-Partition -NoDefaultDriveLetter $true }} catch {{ }}
foreach ($path in @($p.AccessPaths)) {{
  if ($path -and -not ([string]$path).StartsWith('\\?\Volume')) {{ Remove-PartitionAccessPath -DiskNumber {disk} -PartitionNumber $p.PartitionNumber -AccessPath $path }}
}}
$p = Get-Partition -DiskNumber {disk} -PartitionNumber $p.PartitionNumber
[PSCustomObject]@{{ PartitionNumber = [uint32]$p.PartitionNumber; Offset = [uint64]$p.Offset; Size = [uint64]$p.Size; Guid = [string]$p.Guid; GptType = [string]$p.GptType; DriveLetter = [string]$p.DriveLetter; AccessPaths = @(@($p.AccessPaths) | ForEach-Object {{ [string]$_ }}) }} | ConvertTo-Json -Compress -Depth 3
"
    ))
}

/// Delete partition `number` only while its GUID, offset and size still
/// match and Windows does not consider it a boot or system partition.
pub fn delete(disk: u32, number: u32, guid: &str, offset: u64, size: u64) -> Option<String> {
    let guid = ps_guid(guid)?;
    Some(format!(
        r"{PRELUDE}$p = Get-Partition -DiskNumber {disk} -PartitionNumber {number}
if ([string]$p.Guid -ne '{guid}' -or [uint64]$p.Offset -ne {offset} -or [uint64]$p.Size -ne {size}) {{ throw 'the partition identity changed' }}
if ($p.IsBoot -or $p.IsSystem -or [string]$p.DriveLetter -match '[A-Za-z]') {{ throw 'refusing to delete a boot, system or mounted partition' }}
Remove-Partition -DiskNumber {disk} -PartitionNumber {number} -Confirm:$false
'{{}}'
"
    ))
}

/// Base64 (RFC 4648, padded).
pub fn base64(data: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
}

/// The `-EncodedCommand` argument for `script`.
pub fn encode_command(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&utf16)
}

/// Arguments that run `script` in a fresh, profile-free, non-interactive
/// Windows PowerShell.
pub fn powershell_args(script: &str) -> Vec<String> {
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-EncodedCommand".into(),
        encode_command(script),
    ]
}

/// `ERROR_CANCELLED`: the person declined the UAC prompt.
pub const EXIT_ELEVATION_DECLINED: i32 = 1223;

/// A program that asks UAC to run `inner` elevated and exits with its exit
/// code. `powershell` is the absolute path of `powershell.exe`, which the
/// elevated process is started from (never a `PATH` lookup).
pub fn elevated(powershell: &str, inner: &str) -> String {
    let path = powershell.replace('\'', "''");
    let arguments = powershell_args(inner).join(" ");
    format!(
        r"{PRELUDE}try {{
  $p = Start-Process -FilePath '{path}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '{arguments}'
}} catch {{ exit {EXIT_ELEVATION_DECLINED} }}
exit $p.ExitCode
"
    )
}
