//! Private Windows account suggestions matched to explicitly granted roots.
use super::{Candidate, Inventory, Origin, Provider};
pub use crate::windows_accounts::{WindowsProfile, native_windows_profiles};
use crate::windows_inventory::{Inventory as WindowsInventory, RootRole};

/// Suggestions are not credentials and never prove cloud synchronization.
/// Registry paths must match a separately granted OneDrive root exactly.
pub fn private_windows_hints(
    selected: &[crate::windows_inventory::SelectedRoot],
    profiles: &[WindowsProfile],
) -> Vec<account_custody::hints::AccountHint> {
    let hints = profiles
        .iter()
        .filter(|profile| selected.iter().any(|grant| grant.path == profile.root))
        .map(|profile| account_custody::hints::AccountHint {
            provider: account_custody::hints::Provider::Microsoft,
            email: profile.email.clone(),
            server: String::new(),
            sources: account_custody::hints::HintSources {
                onedrive: true,
                ..Default::default()
            },
        })
        .collect();
    account_custody::hints::group_hints(hints)
        .into_iter()
        .take(32)
        .collect()
}

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
            services: Vec::new(),
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
    use crate::windows_accounts::parse_profiles;

    #[test]
    fn multiple_profiles_merge_only_exact_granted_account_identities() {
        let profiles = parse_profiles(br#"[{"root":"personal","email":"personal@example.org"},{"root":"work","email":"work@example.org"},{"root":"duplicate","email":"personal@example.org"},{"root":"alias","email":"personal+alias@example.org"}]"#).unwrap();
        let grants: Vec<_> = ["personal", "work", "duplicate", "alias"]
            .into_iter()
            .map(|path| crate::windows_inventory::SelectedRoot {
                path: path.into(),
                role: RootRole::OneDrive,
            })
            .collect();
        let hints = private_windows_hints(&grants, &profiles);
        assert_eq!(hints.len(), 3);
        assert_eq!(
            hints
                .iter()
                .map(|hint| hint.email.as_str())
                .collect::<Vec<_>>(),
            [
                "personal@example.org",
                "work@example.org",
                "personal+alias@example.org"
            ]
        );
        assert!(hints.iter().all(|hint| hint.sources.onedrive));
    }

    #[test]
    fn parent_and_child_paths_do_not_grant_account_hints() {
        let profiles = parse_profiles(
            br#"[{"root":"chosen/onedrive","email":"private-fixture@example.org"}]"#,
        )
        .unwrap();
        for path in ["chosen", "chosen/onedrive/subfolder"] {
            let grants = [crate::windows_inventory::SelectedRoot {
                path: path.into(),
                role: RootRole::OneDrive,
            }];
            assert!(private_windows_hints(&grants, &profiles).is_empty());
        }
    }

    #[test]
    fn registry_profiles_are_private_suggestions_only_for_exact_grants() {
        let profiles = parse_profiles(br#"[{"root":"chosen","email":"private-fixture@example.org"},{"root":"not-granted","email":"other@example.org"}]"#).unwrap();
        let grants = [crate::windows_inventory::SelectedRoot {
            path: "chosen".into(),
            role: RootRole::OneDrive,
        }];
        let hints = private_windows_hints(&grants, &profiles);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].email, "private-fixture@example.org");
        assert!(hints[0].sources.onedrive);
        assert!(!hints[0].sources.outlook);
        assert!(!hints[0].sources.google_web);
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
