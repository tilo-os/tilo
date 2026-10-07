//! Importing a provider profile into `NetworkManager`, off the UI thread.
//!
//! The file comes from the desktop portal; it is size-checked, read,
//! validated and handed straight to `NetworkManager`, which becomes the sole
//! owner of the profile and its secrets. What comes back is a secret-free
//! profile identity for the shared fleet (a new import) and this device's
//! display facts for it. The raw text is zeroized before the job returns.

use std::collections::HashSet;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use ui::framework::Progress;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use zeroize::Zeroize;

use crate::config;
use crate::model::{ConnKind, Connection, LocalProfileDetails};

/// The portal request for a profile of `kind`.
pub fn request(kind: ConnKind) -> PickRequest {
    let (title, filter) = match kind {
        ConnKind::WireGuard => (
            "Import WireGuard tunnel",
            FileFilter::new("WireGuard (.conf)").glob("*.conf"),
        ),
        ConnKind::OpenVpn => (
            "Import OpenVPN tunnel",
            FileFilter::new("OpenVPN (.ovpn)").glob("*.ovpn"),
        ),
    };
    PickRequest::open(title, "Import")
        .thread_name("vpn-portal-import")
        .filter(filter)
        .all_files()
}

/// What an import job was asked to do.
pub struct Import {
    /// The protocol asked for, used when the file's extension says nothing.
    pub hint: ConnKind,
    /// The shared profile being set up here; `None` imports a new one.
    pub target: Option<Connection>,
    /// The portal's answer, once the person picked a file.
    pub picked: PortalChannel,
}

/// How an import ended.
pub enum Imported {
    /// The person closed the picker, or the caller cancelled.
    Cancelled,
    /// Nothing changed; trying again is safe.
    Failed(String),
    /// `NetworkManager` holds a new profile it could not be told apart from
    /// the others: trying again could duplicate a credential-bearing profile.
    Ambiguous(String),
    /// The profile is imported.
    Done(Box<Done>),
}

/// A profile imported (or, in a test process, parsed only).
pub struct Done {
    pub connection: Connection,
    pub details: LocalProfileDetails,
    pub log: String,
}

const AMBIGUOUS: &str = "NetworkManager imported a profile, but its exact new UUID could not be identified. The external profile exists; recover or remove it in NetworkManager before attempting another import.";

/// Wait for the picked file and import it. Blocks: run it as a job.
pub fn run(import: Import, progress: &Progress) -> Imported {
    progress.report("choosing a file", None, None);
    let outcome = loop {
        if progress.is_cancelled() {
            return Imported::Cancelled;
        }
        match import.picked.recv_timeout(Duration::from_millis(200)) {
            Ok(outcome) => break outcome,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Imported::Cancelled,
        }
    };
    let path = match outcome {
        PortalOutcome::Opened(path) => path,
        PortalOutcome::Cancelled => return Imported::Cancelled,
        PortalOutcome::Failed(reason) => {
            return Imported::Failed(format!("The file picker failed: {reason}"));
        }
    };
    progress.report("importing", None, None);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > config::MAX_CONFIG_BYTES) {
        return Imported::Failed(format!(
            "The selected profile exceeds the {} KiB safety limit.",
            config::MAX_CONFIG_BYTES / 1024
        ));
    }
    let mut text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return Imported::Failed(format!("Could not read the selected profile: {error}"));
        }
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported")
        .to_string();
    let kind = config::kind_from_filename(path.file_name().and_then(|s| s.to_str()).unwrap_or(""))
        .unwrap_or(import.hint);
    if let Err(error) = config::validate_config(kind, &text) {
        text.zeroize();
        return Imported::Failed(error);
    }
    let connection = match import.target {
        Some(existing) if existing.kind != kind => {
            text.zeroize();
            return Imported::Failed(
                "The selected provider file has a different VPN protocol than this shared profile."
                    .into(),
            );
        }
        Some(existing) => existing,
        None => config::parse_config(&stem, kind, &text),
    };
    let details = config::parse_local_details(kind, &text);
    text.zeroize();
    if !config::exec_allowed() {
        return Imported::Done(Box::new(Done {
            connection,
            details,
            log: format!(
                "parsed {} '{stem}' (host import disabled in test process)",
                kind.badge()
            ),
        }));
    }
    hand_to_network_manager(&path, kind, &stem, connection, details)
}

/// Import `path` into `NetworkManager` and find the exact profile it made.
fn hand_to_network_manager(
    path: &std::path::Path,
    kind: ConnKind,
    stem: &str,
    connection: Connection,
    mut details: LocalProfileDetails,
) -> Imported {
    let known: HashSet<String> = match os_actions::list_vpn_connections() {
        Ok(profiles) => profiles.into_iter().map(|profile| profile.uuid).collect(),
        Err(error) => {
            return Imported::Failed(format!(
                "NetworkManager availability could not be verified before import: {error}"
            ));
        }
    };
    let nm_kind = match kind {
        ConnKind::WireGuard => "wireguard",
        ConnKind::OpenVpn => "openvpn",
    };
    if let Err(error) = os_actions::import_vpn_config(nm_kind, &path.to_string_lossy()) {
        return Imported::Failed(format!(
            "NetworkManager could not import {} '{stem}': {error}",
            kind.badge()
        ));
    }
    let discovered: Vec<_> = os_actions::list_vpn_connections()
        .map(|profiles| {
            profiles
                .into_iter()
                .filter(|profile| !known.contains(&profile.uuid))
                .collect()
        })
        .unwrap_or_default();
    let [profile] = discovered.as_slice() else {
        return Imported::Ambiguous(AMBIGUOUS.into());
    };
    if profile.uuid.is_empty() {
        return Imported::Ambiguous(AMBIGUOUS.into());
    }
    details.network_manager_uuid.clone_from(&profile.uuid);
    Imported::Done(Box::new(Done {
        connection,
        details,
        log: format!("imported {} '{stem}' into NetworkManager", kind.badge()),
    }))
}
