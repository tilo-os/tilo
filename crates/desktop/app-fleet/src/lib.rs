//! What the policy reads of the Tilo fleet's signed manifests, compiled in: whose each app's
//! windows are, which of its records are sealed, and the records the system keeps for it
//! ([`WORKSPACE_DOCUMENTS`]). Gathered at build time from the
//! catalogue entry every app commits beside itself (`fleet.json`), reading only those fields; how
//! people find and know an app (its names, keywords, offered actions, surface) is
//! `fleet_catalogue`'s, so a change to how an app is listed rebuilds nothing that reads this.

mod kept;

pub use kept::{
    EARLIER_RECORDS, PERSON_VIEW_PREFIX, Readers, VAULT_KIND, VAULT_RECORD, WORKSPACE_DOCUMENTS,
    WorkspaceDocument, declared_record, derived_kind, derived_record, fixed_kind, person_record,
};

/// What each app declares of its windows and records, read from its committed entry by `build.rs`.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/kinds.rs"));
}

/// Whether a window of `package_id` is the person's own, as the app declares it
/// (`ResourceApp::WINDOWS_ARE_PERSONAL`): it shows what the person keeps for themselves, and
/// never stands on a workspace's shared desktop.
#[must_use]
pub fn windows_are_own(package_id: &str) -> bool {
    let app = tilo_app_id::fleet_id(package_id);
    generated::OWN_WINDOWS
        .iter()
        .any(|own| own.eq_ignore_ascii_case(&app))
}

/// Whether records of `kind` are kept under keys of their own from their first write, as the
/// app that declares the kind says (`[resources] sealed`, a tilolang root's `sealed: true`).
#[must_use]
pub fn is_sealed(kind: &str) -> bool {
    generated::SEALED_KINDS
        .iter()
        .any(|(_, declared)| *declared == kind)
}

/// The kind of `package_id`'s own record, which a tilolang package keeps its root in.
#[must_use]
pub fn package_kind(package_id: &str) -> String {
    format!("tl.{}", tilo_app_id::fleet_id(package_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_read_per_app() {
        // Settings shows what the person keeps for themselves, so its windows are theirs.
        assert!(windows_are_own("dev.tilo.settings"));
        assert!(!windows_are_own("so.tilo.chat"));
    }
}
