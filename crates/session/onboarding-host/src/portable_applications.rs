//! Explicit first-login continuation for authenticated portable applications.
//! The canonical helper owns archive proofs, user-state adoption and signed
//! offline restoration; this controller never substitutes a public receipt for them.
use crate::continuation::{self as continuation, Context, Continuation, Kind, Settled};
use crate::prepared::{Pending as Prepared, PortableApplications};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

pub type Pending = Continuation<Applications>;

/// Portable applications carried over by a prepared setup.
pub enum Applications {}

fn failure() -> io::Error {
    continuation::failure::<Applications>()
}
#[cfg(any(test, feature = "test-support"))]
fn root(home: &Path) -> PathBuf {
    continuation::root::<Applications>(home)
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub enum Event {
    #[serde(rename = "ready-to-restore")]
    Ready {
        applications: usize,
        deployments: usize,
        user_state: bool,
    },
    #[serde(rename = "complete")]
    Complete { restored: usize, verified: bool },
}

impl Kind for Applications {
    type Declaration = PortableApplications;
    type Event = Event;
    const FAILURE: &'static str = "Portable applications are pending or unavailable";
    const ROOT: &'static str = "prepared-applications";
    const RECEIPT_BOUND: u64 = 8192;
    const LINE_BOUND: usize = 8192;
    const HELPER_FLAG: &'static str = "--portable-applications";
    const WORKSPACE: &'static str = "portable-applications";
    const WORKSPACE_NAMESPACE: bool = true;
    const OUTPUT_BOUND: usize = 64 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(1800);
    const READY: &'static str = "reviewed";
    const PROCEED: &'static str = "start";
    const PROCEED_ACTION: &'static str = "restore";
    const PROCEEDING: &'static str = "restoring-applications";
    const CONTINUES_WHEN_COMPLETE: bool = true;
    const FORGETS_PASSWORD_ON_FAILURE: bool = false;
    const FAILURE_CLEARS_RESULT: bool = false;
    const MONOTONIC_RECEIPTS: bool = false;
    const COMPLETION_NEEDS_PROOF: bool = false;

    fn declared(prepared: &Prepared, identity: &Path) -> io::Result<Option<PortableApplications>> {
        prepared.portable_applications(identity)
    }
    fn valid(declaration: &PortableApplications) -> bool {
        declaration.valid()
    }
    fn operation(declaration: &PortableApplications) -> &str {
        &declaration.operation
    }
    fn mark(_: &PortableApplications, status: &mut serde_json::Map<String, serde_json::Value>) {
        status.insert("portable_applications".to_owned(), true.into());
        status.insert("applications_only".to_owned(), true.into());
    }
    fn package(cx: &Context<Self>, directory: &Path) -> io::Result<(PathBuf, String)> {
        let expected: PortableApplications =
            serde_json::from_slice(&continuation::private_file::<Self>(
                &directory.join("expectation.json"),
                8192,
            )?)
            .map_err(|_| failure())?;
        if expected != cx.declaration {
            return Err(failure());
        }
        let package = directory.join(&cx.declaration.filename);
        continuation::private_package::<Self>(&package, Some(cx.declaration.sealed_bytes))?;
        Ok((package, cx.declaration.operation.clone()))
    }
    fn settle(_: &Context<Self>, event: Event) -> Settled {
        match event {
            Event::Ready {
                applications,
                deployments,
                user_state,
            } => Settled::Ready {
                result: serde_json::json!({"applications":applications,"deployments":deployments,"user_state":user_state}),
                proceed: true,
            },
            Event::Complete {
                restored,
                verified: true,
            } => Settled::Complete {
                result: serde_json::json!({"restored":restored,"verified":true}),
                projection: None,
            },
            Event::Complete { .. } => Settled::Failed,
        }
    }
}

/// Fixtures for the tests of this module and of the session that hosts setup.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test support: a fixture that cannot be written fails the calling test"
)]
pub mod fixtures {
    use super::{PortableApplications, Prepared, root};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    pub fn fixture() -> (tempfile::TempDir, PathBuf, PortableApplications) {
        let (home, identity, _) = crate::portable_home::fixtures::fixture();
        let prepared = Prepared::open(home.path()).unwrap();
        std::fs::remove_file(
            home.path()
                .join(".local/state/tilo/prepared-setup/activated.json"),
        )
        .unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        // Real selected user-state bytes make this declaration representative
        // without depending on the random identity's JSON decimal length.
        // This remains a controller fixture, not an archive/Flatpak restore.
        let source = home.path().join("selected-application-state");
        let mut selected = Vec::new();
        let mut state_bytes = 0_u64;
        for (id, settings) in [
            (
                "org.mozilla.firefox",
                serde_json::json!({"homepage":"about:blank","restore_previous_session":false}),
            ),
            (
                "org.videolan.VLC",
                serde_json::json!({"volume":75,"resume_playback":true}),
            ),
            (
                "org.gnome.TextEditor",
                serde_json::json!({"show_line_numbers":true,"tab_width":4}),
            ),
        ] {
            let relative = format!(".var/app/{id}/config/settings.json");
            let path = source.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = serde_json::to_vec(&settings).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            state_bytes = state_bytes.saturating_add(u64::try_from(bytes.len()).unwrap());
            selected.push(serde_json::json!({
                "reference":format!("app/{id}/x86_64/stable"),
                "state_path":relative,
                "state_sha256":<[u8;32]>::from(Sha256::digest(&bytes)),
            }));
        }
        let entries = selected.len().saturating_add(1);
        let metadata = serde_json::to_vec(&serde_json::json!({
            "schema":1,"selected_user_state":selected,
        }))
        .unwrap();
        let metadata_path = source.join("selected-user-state.json");
        std::fs::write(&metadata_path, &metadata).unwrap();
        std::fs::set_permissions(&metadata_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let ciphertext = b"fixture ciphertext";
        let declaration = PortableApplications {
            schema: 1,
            operation: operation.clone(),
            purpose: "flatpak_applications".into(),
            filename: format!("applications-{operation}.sealed"),
            setup_binding: "a".repeat(256),
            sealed_bytes: u64::try_from(ciphertext.len()).unwrap(),
            plaintext_bytes: u64::try_from(metadata.len())
                .unwrap()
                .saturating_add(state_bytes),
            entries,
            ciphertext_sha256: Sha256::digest(ciphertext).into(),
            manifest_sha256: Sha256::digest(&metadata).into(),
            limits: crate::prepared::ApplicationLimits {
                plaintext_bytes: 1024 * 1024,
                entries: 128,
                metadata_bytes: 64 * 1024,
            },
        };
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        prepared
            .acknowledge(
                signer.group_public_key(),
                None,
                Some(declaration.clone()),
                None,
                None,
                None,
            )
            .unwrap();
        let root = root(home.path());
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in [
            (
                "expectation.json".to_owned(),
                serde_json::to_vec(&declaration).unwrap(),
            ),
            (declaration.filename.clone(), ciphertext.to_vec()),
        ] {
            let file = root.join(name);
            std::fs::write(&file, bytes).unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        (home, identity, declaration)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use zeroize::Zeroizing;
    fn wait(pending: &mut Pending) {
        tilo_test_env::wait_until("the worker to exit", || {
            if !pending.busy() {
                return true;
            }
            pending.poll();
            false
        });
    }
    #[test]
    fn actual_owned_helper_protocol_gates_completion_and_binds_saved_declaration() {
        let (home, identity, declaration) = fixture();
        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        let prepared = Prepared::open(home.path()).unwrap();
        pending
            .receive_prepared_password(
                prepared.digest(),
                Zeroizing::new(b"private password".to_vec()),
            )
            .unwrap();
        pending.manual();
        let helper = home.path().join("inert-helper");
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"ready-to-restore\",\"applications\":2,\"deployments\":3,\"user_state\":true}'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        pending.run_with("review", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "reviewed");
        assert!(pending.pending());
        assert!(!root(home.path()).join("choice.json").exists());
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"complete\",\"restored\":2,\"verified\":false}'\n").unwrap();
        pending.run_with("restore", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "failed");
        assert!(!root(home.path()).join("choice.json").exists());
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"complete\",\"restored\":2,\"verified\":true}'\n").unwrap();
        pending.run_with("restore", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "complete");
        assert!(!pending.pending());
        assert!(!pending.holds_password());
        let mut reopened =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(!reopened.pending());
        let mut changed = declaration.clone();
        changed.manifest_sha256 = [9; 32];
        assert!(Pending::open(home.path().into(), identity, changed).is_err());
        let bytes = std::fs::read(root(home.path()).join(&declaration.filename)).unwrap();
        assert_eq!(bytes, b"fixture ciphertext");
        assert_eq!(
            declaration.ciphertext_sha256,
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        let selected = std::fs::read(
            home.path()
                .join("selected-application-state/selected-user-state.json"),
        )
        .unwrap();
        assert_eq!(
            declaration.manifest_sha256,
            <[u8; 32]>::from(Sha256::digest(&selected))
        );
        std::fs::remove_file(root(home.path()).join("expectation.json")).unwrap();
        symlink(
            home.path().join("inert-helper"),
            root(home.path()).join("expectation.json"),
        )
        .unwrap();
        assert!(reopened.run_with("review", &helper).is_err());
    }
}
