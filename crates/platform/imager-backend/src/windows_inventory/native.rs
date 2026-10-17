//! Native registry seam. Queries metadata only; never enumerates profile files.
use super::RootRole;
#[cfg(any(windows, test))]
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize)]
pub struct RootSuggestion {
    pub path: PathBuf,
    pub role: RootRole,
    pub label: String,
}

/// Suggestions require independent selection. Not a discovery of host data.
/// The metadata adapter is unavailable off Windows, including mounted hives.
///
/// # Errors
/// Refuses unsupported platforms, failed metadata queries and malformed results.
pub fn suggestions() -> Result<Vec<RootSuggestion>, String> {
    #[cfg(windows)]
    {
        use std::{process::Command, time::Duration};
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT]);
        let output =
            bounded_command::run_prepared(command, None, 256 * 1024, Duration::from_secs(10))
                .map_err(|error| format!("Windows root metadata lookup failed: {error}"))?;
        if !output.status.success() {
            return Err("Windows root metadata lookup failed".into());
        }
        parse(&output.stdout)
    }
    #[cfg(not(windows))]
    {
        Err("native Windows root metadata requires Windows".into())
    }
}

#[cfg(any(windows, test))]
#[derive(Deserialize)]
struct Record {
    kind: String,
    path: String,
    label: String,
}

#[cfg(any(windows, test))]
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<RootSuggestion>, String> {
    if bytes.len() > 256 * 1024 {
        return Err("Windows root metadata exceeds limit".into());
    }
    let records: Vec<Record> =
        serde_json::from_slice(bytes).map_err(|_| "invalid Windows root metadata")?;
    if records.len() > 256 {
        return Err("too many Windows root suggestions".into());
    }
    let mut result = Vec::new();
    for record in records {
        let role = match record.kind.as_str() {
            "known" => RootRole::KnownFolder,
            "onedrive" => RootRole::OneDrive,
            "outlook" => RootRole::Outlook,
            "steam" => RootRole::Steam,
            _ => return Err("unrecognized Windows root metadata kind".into()),
        };
        if record.path.is_empty() || record.path.contains('\0') || record.path.len() > 32768 {
            return Err("invalid Windows root suggestion path".into());
        }
        result.push(RootSuggestion {
            path: record.path.into(),
            role,
            label: record.label,
        });
    }
    Ok(result)
}

// No dynamic script interpolation, wildcard profile paths, credentials or
// recursive registry walks. Values are suggestions, not filesystem authority.
#[cfg(windows)]
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$rows = [System.Collections.Generic.List[object]]::new()
function Add-Root($kind, $path, $label) {
 if ($path -is [string] -and $path.Length -gt 0) {
  $rows.Add(@{ kind=$kind; path=[Environment]::ExpandEnvironmentVariables($path); label=$label })
 }
}
$folders = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders'
if (Test-Path -LiteralPath $folders) {
 $v = Get-ItemProperty -LiteralPath $folders
 foreach ($name in @('Personal','Desktop','My Pictures','My Music','My Video','{374DE290-123F-4565-9164-39C4925E467B}','{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}')) {
  Add-Root 'known' $v.$name $name
 }
 if ($v.Personal) { Add-Root 'outlook' (Join-Path ([Environment]::ExpandEnvironmentVariables($v.Personal)) 'Outlook Files') 'Outlook archives' }
}
$accounts = 'HKCU:\Software\Microsoft\OneDrive\Accounts'
if (Test-Path -LiteralPath $accounts) {
 $keys = @(Get-ChildItem -LiteralPath $accounts)
 if ($keys.Count -gt 128) { throw 'Too many OneDrive accounts' }
 foreach ($key in $keys) {
  $v = Get-ItemProperty -LiteralPath $key.PSPath
  Add-Root 'onedrive' $v.UserFolder $key.PSChildName
 }
}
$steam = 'HKCU:\Software\Valve\Steam'
if (Test-Path -LiteralPath $steam) {
 $v = Get-ItemProperty -LiteralPath $steam
 Add-Root 'steam' $v.SteamPath 'Steam'
}
foreach ($version in @('16.0','15.0','14.0')) {
 $outlook = 'HKCU:\Software\Microsoft\Office\' + $version + '\Outlook'
 if (Test-Path -LiteralPath $outlook) {
  $v = Get-ItemProperty -LiteralPath $outlook
  Add-Root 'outlook' $v.ForcePSTPath 'Redirected Outlook archive directory'
  Add-Root 'outlook' $v.ForceOSTPath 'Redirected Outlook local store directory'
 }
}
$local = [Environment]::GetFolderPath('LocalApplicationData')
if ($local) { Add-Root 'outlook' (Join-Path $local 'Microsoft\Outlook') 'Outlook local stores and drafts' }
[Console]::Out.Write((ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 3 -Compress))
"#;
