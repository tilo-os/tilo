//! Bounded native Windows account metadata. Suggestions never attest credentials.
/// Private registry metadata, consumed only after an exact folder grant.
/// No serialization/debug implementation: identities never enter public media JSON.
pub struct WindowsProfile {
    pub root: std::path::PathBuf,
    pub email: String,
}

/// Read only bounded OneDrive account metadata; never scan profile files.
///
/// # Errors
/// Refuses failed, oversized or malformed registry responses.
pub fn native_windows_profiles() -> Result<Vec<WindowsProfile>, String> {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            PRIVATE_PROFILES,
        ]);
        let output = bounded_command::run_prepared(
            command,
            None,
            256 * 1024,
            std::time::Duration::from_secs(10),
        )
        .map_err(|_| "Windows account metadata unavailable")?;
        if !output.status.success() {
            return Err("Windows account metadata unavailable".into());
        }
        parse_profiles(&output.stdout)
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// Decode the bounded registry reader's private metadata response.
///
/// # Errors
/// Rejects unknown fields, oversized responses and invalid identities.
pub fn parse_profiles(bytes: &[u8]) -> Result<Vec<WindowsProfile>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Profile {
        root: std::path::PathBuf,
        email: String,
    }
    if bytes.len() > 256 * 1024 {
        return Err("Windows account metadata exceeds limit".into());
    }
    let rows: Vec<Profile> =
        serde_json::from_slice(bytes).map_err(|_| "Invalid Windows account metadata")?;
    if rows.len() > 128
        || rows
            .iter()
            .any(|row| row.email.len() > 2048 || row.email.chars().any(char::is_control))
    {
        return Err("Invalid Windows account metadata".into());
    }
    Ok(rows
        .into_iter()
        .map(|row| WindowsProfile {
            root: row.root,
            email: row.email,
        })
        .collect())
}

#[cfg(windows)]
const PRIVATE_PROFILES: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$rows = [System.Collections.Generic.List[object]]::new()
$accounts = 'HKCU:\Software\Microsoft\OneDrive\Accounts'
if (Test-Path -LiteralPath $accounts) {
 $keys = @(Get-ChildItem -LiteralPath $accounts)
 if ($keys.Count -gt 128) { throw 'Too many accounts' }
 foreach ($key in $keys) {
  $v = Get-ItemProperty -LiteralPath $key.PSPath
  if ($v.UserFolder -and $v.UserEmail) {
   $rows.Add(@{ root=$v.UserFolder; email=$v.UserEmail })
  }
 }
}
[Console]::Out.Write((ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 3 -Compress))
"#;
