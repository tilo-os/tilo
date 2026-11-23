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
    type StoredDeclaration = prepared_setup_model::stored::StoredApplicationsExpectation;
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

    fn into_declaration(stored: Self::StoredDeclaration) -> Self::Declaration {
        stored.0
    }
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
        let expected =
            serde_json::from_slice::<prepared_setup_model::stored::StoredApplicationsExpectation>(
                &continuation::private_file::<Self>(&directory.join("expectation.json"), 8192)?,
            )
            .map_err(|_| failure())?
            .0;
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
            operation: operation.clone(),
            filename: format!("applications-{operation}.sealed"),
            setup_binding: "a".repeat(256),
            sealed_bytes: u64::try_from(ciphertext.len()).unwrap(),
            plaintext_bytes: u64::try_from(metadata.len())
                .unwrap()
                .saturating_add(state_bytes),
            entries,
            ciphertext_sha256: Sha256::digest(ciphertext).into(),
            limits: crate::prepared::ApplicationLimits {
                plaintext_bytes: 1024 * 1024,
                entries: 128,
                metadata_bytes: 64 * 1024,
            },
            kind: prepared_setup_model::Applications {
                consumer_protocol: 2,
                manifest_sha256: Sha256::digest(&metadata).into(),
            },
        };
        let signer = tilo_identity::store::load_from_path(identity.join("primary.json")).unwrap();
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
    fn actual_owned_helper_protocol_checks_completion_and_binds_saved_declaration() {
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
        changed.kind.manifest_sha256 = [9; 32];
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
            declaration.kind.manifest_sha256,
            <[u8; 32]>::from(Sha256::digest(&selected))
        );
        let mut with_unknown = serde_json::to_value(&declaration).unwrap();
        with_unknown["unexpected"] = serde_json::json!(true);
        std::fs::write(
            root(home.path()).join("expectation.json"),
            serde_json::to_vec(&with_unknown).unwrap(),
        )
        .unwrap();
        assert!(reopened.run_with("review", &helper).is_err());
        std::fs::remove_file(root(home.path()).join("expectation.json")).unwrap();
        symlink(
            home.path().join("inert-helper"),
            root(home.path()).join("expectation.json"),
        )
        .unwrap();
        assert!(reopened.run_with("review", &helper).is_err());
    }

    #[test]
    fn package_reader_accepts_flat_and_kind_expectations_and_refuses_flat_protocol() {
        let (home, identity, mut declaration) = fixture();
        declaration.kind.consumer_protocol = 1;
        let flat = serde_json::json!({
            "schema": 1,
            "operation": declaration.operation,
            "purpose": "flatpak_applications",
            "filename": declaration.filename,
            "setup_binding": declaration.setup_binding,
            "sealed_bytes": declaration.sealed_bytes,
            "plaintext_bytes": declaration.plaintext_bytes,
            "entries": declaration.entries,
            "ciphertext_sha256": declaration.ciphertext_sha256,
            "manifest_sha256": declaration.kind.manifest_sha256,
            "limits": declaration.limits,
        });
        let directory = root(home.path());
        let cx = Context {
            home: home.path().to_path_buf(),
            identity,
            declaration: declaration.clone(),
            digest: [0; 32],
        };
        std::fs::write(
            directory.join("expectation.json"),
            serde_json::to_vec(&declaration).unwrap(),
        )
        .unwrap();
        assert_eq!(
            Applications::package(&cx, &directory).unwrap(),
            (
                directory.join(&declaration.filename),
                declaration.operation.clone()
            )
        );

        std::fs::write(
            directory.join("expectation.json"),
            serde_json::to_vec(&flat).unwrap(),
        )
        .unwrap();
        assert_eq!(
            Applications::package(&cx, &directory).unwrap(),
            (directory.join(&declaration.filename), declaration.operation)
        );

        for protocol in [1, 2] {
            let mut with_protocol = flat.clone();
            with_protocol["consumer_protocol"] = serde_json::json!(protocol);
            std::fs::write(
                directory.join("expectation.json"),
                serde_json::to_vec(&with_protocol).unwrap(),
            )
            .unwrap();
            assert!(Applications::package(&cx, &directory).is_err());
        }
    }

    fn flat_choice_json(digest: [u8; 32], declaration: &PortableApplications) -> String {
        // Exact literal wire layout and field order written by main's
        // `Choice<PortableApplications>` (`schema`, `setup_digest`,
        // `declaration` with flat 11-key `PortableApplications`, `outcome`).
        format!(
            concat!(
                r#"{{"schema":1,"setup_digest":{digest},"declaration":{{"#,
                r#""schema":1,"operation":"{operation}","purpose":"flatpak_applications","#,
                r#""filename":"{filename}","setup_binding":"{binding}","#,
                r#""sealed_bytes":{sealed_bytes},"plaintext_bytes":{plaintext_bytes},"#,
                r#""entries":{entries},"ciphertext_sha256":{ciphertext},"#,
                r#""manifest_sha256":{manifest},"limits":{{"plaintext_bytes":{limit_plain},"#,
                r#""entries":{limit_entries},"metadata_bytes":{limit_meta}}}}},"#,
                r#""outcome":"deferred"}}"#
            ),
            digest = serde_json::to_string(&digest).unwrap(),
            operation = declaration.operation,
            filename = declaration.filename,
            binding = declaration.setup_binding,
            sealed_bytes = declaration.sealed_bytes,
            plaintext_bytes = declaration.plaintext_bytes,
            entries = declaration.entries,
            ciphertext = serde_json::to_string(&declaration.ciphertext_sha256).unwrap(),
            manifest = serde_json::to_string(&declaration.kind.manifest_sha256).unwrap(),
            limit_plain = declaration.limits.plaintext_bytes,
            limit_entries = declaration.limits.entries,
            limit_meta = declaration.limits.metadata_bytes,
        )
    }

    fn v1_fixture() -> (tempfile::TempDir, PathBuf, PortableApplications) {
        let (home, identity, mut declaration) = fixture();
        declaration.kind.consumer_protocol = 1;
        let prepared = Prepared::open(home.path()).unwrap();
        std::fs::remove_file(
            home.path()
                .join(".local/state/tilo/prepared-setup/activated.json"),
        )
        .unwrap();
        let signer = tilo_identity::store::load_from_path(identity.join("primary.json")).unwrap();
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
        std::fs::write(
            root(home.path()).join("expectation.json"),
            serde_json::to_vec(&declaration).unwrap(),
        )
        .unwrap();
        (home, identity, declaration)
    }

    #[test]
    fn stored_main_flat_choice_json_loads_stays_on_equal_replaces_and_refuses_unknown_keys() {
        let (home, identity, declaration) = v1_fixture();
        let prepared = Prepared::open(home.path()).unwrap();
        let directory = root(home.path());
        let flat_choice = flat_choice_json(prepared.digest(), &declaration);
        let choice_path = directory.join("choice.json");
        std::fs::write(&choice_path, flat_choice.as_bytes()).unwrap();
        std::fs::set_permissions(&choice_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(pending.deferred());
        assert!(!pending.pending());
        assert_eq!(pending.status()["state"], "deferred");

        // Re-acknowledging the equal outcome leaves the existing flat file untouched on disk.
        pending.invoke("later", b"").unwrap();
        assert_eq!(std::fs::read(&choice_path).unwrap(), flat_choice.as_bytes());

        // Replacing the deferred flat choice with completion writes the nested shape.
        pending.invoke("resume", b"").unwrap();
        assert!(pending.pending());
        pending
            .receive_prepared_password(
                prepared.digest(),
                Zeroizing::new(b"private password".to_vec()),
            )
            .unwrap();
        pending.manual();
        let helper = home.path().join("inert-helper");
        std::fs::write(
            &helper,
            b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"complete\",\"restored\":2,\"verified\":true}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        pending.run_with("restore", &helper).unwrap();
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "complete");
        assert!(!pending.pending());

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&choice_path).unwrap()).unwrap();
        assert_eq!(written["outcome"], "complete");
        assert!(written["declaration"]["kind"]["manifest_sha256"].is_array());
        assert!(written["declaration"].get("schema").is_none());
        assert!(written["declaration"].get("purpose").is_none());

        let reopened =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(!reopened.pending());
        assert!(!reopened.deferred());

        // Unknown keys (outer or declaration) and flat `consumer_protocol` are refused on both readers.
        let mut with_outer_unknown: serde_json::Value = serde_json::from_str(&flat_choice).unwrap();
        with_outer_unknown["unexpected"] = serde_json::json!(true);
        let mut with_decl_unknown: serde_json::Value = serde_json::from_str(&flat_choice).unwrap();
        with_decl_unknown["declaration"]["extra"] = serde_json::json!(true);
        let mut with_flat_protocol_1: serde_json::Value =
            serde_json::from_str(&flat_choice).unwrap();
        with_flat_protocol_1["declaration"]["consumer_protocol"] = serde_json::json!(1);
        let mut with_flat_protocol_2: serde_json::Value =
            serde_json::from_str(&flat_choice).unwrap();
        with_flat_protocol_2["declaration"]["consumer_protocol"] = serde_json::json!(2);
        let mut with_nested_unknown = written;
        with_nested_unknown["declaration"]["extra"] = serde_json::json!(true);

        for (label, bad) in [
            ("outer unknown", with_outer_unknown),
            ("flat declaration unknown", with_decl_unknown),
            ("flat consumer_protocol=1", with_flat_protocol_1),
            ("flat consumer_protocol=2", with_flat_protocol_2),
            ("nested declaration unknown", with_nested_unknown),
        ] {
            std::fs::write(&choice_path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(
                Pending::open(home.path().into(), identity.clone(), declaration.clone()).is_err(),
                "open must refuse {label}"
            );
            assert!(
                pending.invoke("later", b"").is_err(),
                "acknowledge must refuse {label}"
            );
        }
    }
}
