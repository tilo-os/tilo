//! Additive Windows account review adapter. A configured sync root is only a
//! sign-in suggestion: it never attests credentials, server sync or durability.
use super::{Candidate, Inventory, Origin, Provider};
use crate::setup::windows_inventory::{Inventory as WindowsInventory, RootRole};

/// Private registry metadata, consumed only after an exact folder grant.
/// No serialization/debug implementation: identities never enter public media JSON.
pub struct WindowsProfile {
    pub root: std::path::PathBuf,
    pub email: String,
}

/// Suggestions are not credentials and never prove cloud synchronization.
/// Registry paths must match a separately granted OneDrive root exactly.
pub fn private_windows_hints(
    selected: &[crate::setup::windows_inventory::SelectedRoot],
    profiles: &[WindowsProfile],
) -> Vec<account_custody::hints::AccountHint> {
    profiles
        .iter()
        .filter(|profile| {
            selected
                .iter()
                .any(|grant| grant.role == RootRole::OneDrive && grant.path == profile.root)
        })
        .take(32)
        .map(|profile| account_custody::hints::AccountHint {
            provider: account_custody::hints::Provider::Microsoft,
            email: profile.email.clone(),
            server: String::new(),
        })
        .collect()
}

/// Read only bounded OneDrive account metadata; never scan profile files.
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

#[cfg(any(windows, test))]
fn parse_profiles(bytes: &[u8]) -> Result<Vec<WindowsProfile>, String> {
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

/// Derive account rows only from an already granted Windows inventory. No
/// environment/registry/profile probing and no credential extraction occurs.
pub fn discover_windows_sources(sources: &WindowsInventory) -> Inventory {
    let mut inventory = Inventory::default();
    for root in &sources.roots {
        if root.role != RootRole::OneDrive {
            continue;
        }
        inventory.candidates.push(Candidate {
            id: format!("windows-onedrive-{}", root.source_id),
            provider: Provider::Microsoft,
            origin: Origin::Application,
            label: "OneDrive (sync and sign-in unverified)".into(),
            identity: None,
            server: None,
            profile: root.path.clone(),
            needs_sign_in: true,
            can_import_credentials: false,
        });
        if !root.gaps.is_empty() {
            inventory.unreadable_sources.push("OneDrive local data");
        }
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_profiles_are_private_suggestions_only_for_exact_grants() {
        let profiles = parse_profiles(br#"[{"root":"chosen","email":"private-fixture@example.org"},{"root":"not-granted","email":"other@example.org"}]"#).unwrap();
        let grants = [crate::setup::windows_inventory::SelectedRoot {
            path: "chosen".into(),
            role: RootRole::OneDrive,
        }];
        let hints = private_windows_hints(&grants, &profiles);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].email, "private-fixture@example.org");
        assert!(private_windows_hints(&[], &profiles).is_empty());
        assert!(parse_profiles(br#"[{"root":"chosen","email":"a","token":"forbidden"}]"#).is_err());
        let key = tilo_seal::SymmetricKey::generate().unwrap();
        let sealed = account_custody::hints::seal(&key, "explicit-root/read-1", &hints).unwrap();
        assert!(
            !sealed
                .as_bytes()
                .windows(hints[0].email.len())
                .any(|bytes| bytes == hints[0].email.as_bytes())
        );
        assert!(account_custody::hints::open(&key, "explicit-root/read-2", &sealed).is_err());
        assert_eq!(
            account_custody::hints::open(&key, "explicit-root/read-1", &sealed).unwrap()[0].email,
            hints[0].email
        );
    }
}
