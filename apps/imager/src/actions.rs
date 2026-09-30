//! The imager's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! ## The split — the imager is mostly external
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) sorts every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`] (a serializable value that deterministically changes
//! the shared Automerge document, so undo and multi-peer replay come for free),
//! while presentation / view state stays plain app code. The imager is a
//! near-textbook **external** app:
//!
//! - **External — never actions.** Writing an image to a device, the read-back
//!   verify pass, the live removable-drive inventory ([`crate::probe`], an
//!   `lsblk` projection re-read each rescan), the live job progress, and any ISO
//!   download are dangerous / network system side effects that touch the
//!   *device*, not the shared CRDT. None of them is an action.
//!
//! - **Presentation / transient — never actions.** The selected source, the
//!   chosen drive, the wizard step, the flash/install mode, the typed
//!   device-name confirmation, and the status line are per-instance view state.
//!   Confirmation is additionally peer-local so remote document changes can
//!   never arm a destructive write.
//!   The recent-sources list is auto-accumulated MRU bookkeeping written as a
//!   side effect of selecting a source (itself presentation) with no discrete
//!   affordance, and the saved-profiles list has no save/delete affordance at
//!   all — so neither is a user-authored action either. The imager persists
//!   the non-sensitive view state in the document for instant-on and
//!   replication, but its *nature* means dispatching it as actions would be
//!   wrong.
//!
//! ## What genuinely lives in the shared document as authored config
//!
//! That leaves exactly one thing: the **guided-customization install profile**
//! (see [`InstallProfile`]) — the non-secret
//! identity / region fields that the live installer actually consumes
//! (hostname, full name, username, locale, timezone, keyboard, filesystem). These are
//! user-authored, they survive restarts, and they replicate to collaborators,
//! so editing one is a document-mutating operation: an
//! [`Action`]. Each carries the **absolute** target value in its payload, so
//! [`Action::apply`] is a pure function of `(payload, document)` — it reads the
//! current profile out of the document, sets one field, and writes it back, with
//! no wall-clock, no RNG, no external state — and replays byte-identically on
//! every replica. A malformed install-mode string normalises to the canonical
//! form rather than erroring, so a bad replay can't wedge a peer.
//!
//! The resource tree mirrors that: an authenticated, installable Tilo image (the only source a
//! profile applies to) is the presented resource, and those applied fields are
//! the [`ActionSpec`]s Ctrl+K offers for it. A plain ISO — written as-is, no
//! customization — publishes nothing. Credentials, destructive disk choices,
//! autologin and persistence mode are deliberately absent: they are not in the
//! media contract and must not appear to be preconfigured.

use automerge::AutoCommit;

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    ConfirmationPolicy, DiscoveryScope, Doc, EncodedAction, ExecutionLocus, ExecutionMode,
    OperationKind, ParamType, PermissionRequirement, Resource, ResourceProvider, ResultSpec,
    RetryPolicy, RiskLevel,
};

use crate::doc::ImagerDoc;
use crate::model::{Filesystem, InstallMode, InstallProfile, Source, SourceKind, TargetDrive};

/// Registry `type_id` for every imager action.
pub const IMAGER_ACTION_TYPE: &str = "imager.action";
pub const IMAGER_OPERATION_TYPE: &str = "imager.operation";

/// App-owned operations. These are deliberately separate from replayable
/// profile edits: media I/O and job inspection are local side effects.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImagerOperation {
    Write {
        device: String,
        confirmation: String,
    },
    Verify,
    Cancel,
    Status,
    ExportConfig,
    /// Host-local Wi-Fi passphrase for the next write. Never collaborative.
    SetWifiPsk {
        value: String,
    },
}

pub fn encode_operation(operation: &ImagerOperation) -> EncodedAction {
    EncodedAction::encode_operation(IMAGER_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: IMAGER_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

/// Decode an app-owned operation payload.
///
/// # Errors
///
/// Returns the JSON decoding error when `type_id` identifies an imager
/// operation but `payload` is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<ImagerOperation>, String> {
    if type_id != IMAGER_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) fn operation_spec(
    id: &'static str,
    title: &'static str,
    kind: OperationKind,
    operation: ImagerOperation,
) -> ActionSpec {
    ActionSpec::operation(id, title, kind, IMAGER_OPERATION_TYPE, &operation).unwrap_or_else(|_| {
        ActionSpec::typed_operation(
            id,
            title,
            "",
            kind,
            IMAGER_OPERATION_TYPE,
            Vec::new(),
            move |_| encode_operation(&operation),
        )
    })
}

fn apply_write_availability(
    write: ActionSpec,
    plan_complete: bool,
    job_active: bool,
    write_allowed: bool,
) -> ActionSpec {
    if !write_allowed {
        write.unavailable(
            "write_backend_disabled",
            "Raw-device writing is disabled; enable the privileged Imager backend first",
        )
    } else if job_active {
        write.unavailable("write_in_progress", "An image write is already in progress")
    } else if !plan_complete {
        write.unavailable(
            "incomplete_plan",
            "Select an image source and target drive before writing",
        )
    } else {
        write
    }
}

/// Build the live operational surface for the selected write plan.
pub fn operation_resource(
    device: Option<&str>,
    plan_complete: bool,
    job_active: bool,
    write_allowed: bool,
) -> Resource {
    let device = device.unwrap_or("").to_string();
    let expected = device.clone();
    let write = ActionSpec::typed_operation(
        "write",
        "Erase drive and write image",
        "Type the exact device name",
        OperationKind::Effect,
        IMAGER_OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "confirmation",
            ParamType::Text,
            "Exact target device name shown by Imager",
        )],
        move |args: &ActionArgs| {
            encode_operation(&ImagerOperation::Write {
                device: expected.clone(),
                confirmation: args.text("confirmation").unwrap_or("").to_string(),
            })
        },
    )
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Destructive,
        confirmation: ConfirmationPolicy::Phrase { phrase: device },
        retry: RetryPolicy::AtMostOnce,
        // The handler starts the worker; status is a separate operation.
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        tags: vec!["storage".into(), "raw-device".into()],
        ..ActionContract::default()
    });

    let write = apply_write_availability(write, plan_complete, job_active, write_allowed);

    let verify = operation_spec(
        "verify",
        "Verify written image",
        OperationKind::Effect,
        ImagerOperation::Verify,
    )
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        retry: RetryPolicy::Idempotent,
        tags: vec!["storage".into(), "verification".into()],
        ..ActionContract::default()
    })
    .unavailable(
        "verify_is_write_phase",
        "Read-back verification runs automatically after every successful write",
    );

    let cancel = operation_spec(
        "cancel",
        "Cancel image write",
        OperationKind::Effect,
        ImagerOperation::Cancel,
    )
    .with_contract(ActionContract {
        // Cancelling only narrows the authority of an already-authorized,
        // peer-local job; it must never trigger a second elevation prompt.
        permission: PermissionRequirement::Write,
        retry: RetryPolicy::Idempotent,
        tags: vec!["storage".into(), "job".into()],
        ..ActionContract::default()
    });
    let cancel = if job_active {
        cancel
    } else {
        cancel.unavailable("no_active_write", "No image write is currently running")
    };

    let status = operation_spec(
        "status",
        "Show image-write status",
        OperationKind::View,
        ImagerOperation::Status,
    )
    .with_contract(ActionContract {
        permission: PermissionRequirement::Read,
        retry: RetryPolicy::Idempotent,
        discovery: DiscoveryScope::Launchable,
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "object" }),
        },
        ..ActionContract::view()
    });

    Resource::new("imager.write-plan", "imager.write-plan", "Image write plan")
        .with_action(write)
        .with_action(verify)
        .with_action(cancel)
        .with_action(status)
}

/// Every collaborative operation the imager exports on the shared document.
///
/// All are absolute *sets* of one field of the guided-customization install
/// profile in `root["profile"]` — never in-place toggles — so the payload fully
/// determines the resulting state and replay is deterministic. Any prior value
/// is read (once, presentation-side) at *authoring* time to pick the opposite
/// value for a toggle; the action itself carries only the target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImagerAction {
    /// Review and select a source. This is an absolute authored record; the
    /// privileged writer still re-validates its identity at execution time.
    SelectSource {
        source: Box<Source>,
        /// The previous reviewed source, captured at authoring time so its
        /// history update is deterministic on every peer.
        previous: Option<Box<Source>>,
        /// Authored MRU position; same-rank concurrent choices use source id.
        recent_rank: i64,
    },
    /// Clear the reviewed source without deleting its tombstoned history.
    ClearSource,
    /// Set the install mode. `mode` is the document's string form
    /// (`"live"` | `"persistent"` | `"disk"`); any other value normalises to
    /// `"live"`.
    SetInstallMode { mode: String },
    /// Set whether the installed system's disk is encrypted.
    SetEncrypt { on: bool },
    /// Set whether the first user account autologs in.
    SetAutologin { on: bool },
    /// Set the installed system's hostname.
    SetHostname { value: String },
    /// Set the first user's full name.
    SetFullName { value: String },
    /// Set the first user's login name.
    SetUsername { value: String },
    /// Set the installed system locale.
    SetLocale { value: String },
    /// Set the system timezone.
    SetTimezone { value: String },
    /// Set the keyboard layout.
    SetKeyboard { value: String },
    /// Set the installed system's filesystem.
    SetFilesystem { filesystem: Filesystem },
    /// Set the Wi-Fi network name the new machine joins. The passphrase is
    /// entered on this machine and is not part of the action.
    SetWifiSsid { value: String },
    /// Set the name of the device identity created for the target.
    SetDeviceLabel { value: String },
    /// Enroll the setup machine's person onto the target as a new device.
    SetEnrollLocalIdentity { on: bool },
}

impl Action for ImagerAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ImagerAction::SelectSource {
                source,
                previous,
                recent_rank,
            } => {
                if let Some(previous) = previous.as_deref()
                    && ImagerDoc::source_id(previous) != ImagerDoc::source_id(source)
                {
                    ImagerDoc::remember_recent_source(doc, previous, *recent_rank);
                }
                ImagerDoc::set_source(doc, Some(source));
            }
            ImagerAction::ClearSource => ImagerDoc::set_source(doc, None),
            // Each action addresses only its own CRDT field. No action reads
            // and re-saves a compound profile, so concurrent edits merge.
            ImagerAction::SetInstallMode { mode } => {
                ImagerDoc::set_profile_mode(doc, InstallMode::from_key(mode));
            }
            ImagerAction::SetEncrypt { on } => ImagerDoc::set_profile_encrypt(doc, *on),
            ImagerAction::SetAutologin { on } => ImagerDoc::set_profile_autologin(doc, *on),
            ImagerAction::SetHostname { value } => {
                ImagerDoc::set_profile_text(doc, "hostname", value);
            }
            ImagerAction::SetFullName { value } => {
                ImagerDoc::set_profile_text(doc, "full_name", value);
            }
            ImagerAction::SetUsername { value } => {
                ImagerDoc::set_profile_text(doc, "username", value);
            }
            ImagerAction::SetLocale { value } => ImagerDoc::set_profile_text(doc, "locale", value),
            ImagerAction::SetTimezone { value } => {
                ImagerDoc::set_profile_text(doc, "timezone", value);
            }
            ImagerAction::SetKeyboard { value } => {
                ImagerDoc::set_profile_text(doc, "keyboard", value);
            }
            ImagerAction::SetFilesystem { filesystem } => {
                ImagerDoc::set_profile_filesystem(doc, *filesystem);
            }
            ImagerAction::SetWifiSsid { value } => {
                ImagerDoc::set_profile_text(doc, "wifi_ssid", value);
            }
            ImagerAction::SetDeviceLabel { value } => {
                ImagerDoc::set_profile_text(doc, "device_label", value);
            }
            ImagerAction::SetEnrollLocalIdentity { on } => {
                ImagerDoc::set_profile_enroll_local_identity(doc, *on);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // A personal install-config edit: this client applies + replicates, peers
        // replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the imager's action decoder for palette execution + remote replay.
/// Called once at app start.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<ImagerAction>(IMAGER_ACTION_TYPE);
}

/// Encode any [`ImagerAction`] into the wire form the palette and the on-screen
/// affordances both dispatch — the single point that turns an imager action into
/// a replayable envelope.
///
/// # Panics
///
/// Never panics: an `ImagerAction` that fails to serialize (it never should —
/// every field is a plain string or bool) falls back to an empty-payload
/// envelope rather than crashing the caller.
pub fn encode(action: &ImagerAction) -> EncodedAction {
    EncodedAction::encode(IMAGER_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: IMAGER_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

/// The install mode the on-screen "persistent" toggle flips to from `current`:
/// persistent ⇄ live (any other mode, e.g. a disk install, flips to persistent).
/// The single source of truth shared by the Customize toggle (see [`crate::app`])
/// and the Ctrl+K resource tree below.
pub fn toggled_install_mode(current: InstallMode) -> InstallMode {
    if current == InstallMode::Persistent {
        InstallMode::Live
    } else {
        InstallMode::Persistent
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn profile_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["configuration".into(), "image-profile".into()],
        ..ActionContract::default()
    }
}

/// Add the identity/region prompted fields — hostname, full name, username,
/// timezone, keyboard — to `res` in order. Split out of [`provide_resources`]
/// to keep it under the line budget; these fields don't depend on `profile`,
/// only on the shape of [`ImagerAction`]. The password is intentionally
/// absent: a secret must never be echoed through a palette prompt.
fn with_identity_fields(mut res: Resource) -> Resource {
    res = res.with_action(
        ActionSpec::prompted(
            "locale",
            "Set locale",
            "Locale",
            "en_US.UTF-8",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetLocale {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "hostname",
            "Set hostname",
            "Hostname",
            "tilo",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetHostname {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "full-name",
            "Set full name",
            "Full name",
            "Tilo User",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetFullName {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "username",
            "Set username",
            "Username",
            "quser",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetUsername {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "timezone",
            "Set timezone",
            "Timezone",
            "UTC",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetTimezone {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "keyboard",
            "Set keyboard layout",
            "Keyboard",
            "us",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetKeyboard {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res
}

/// Wi-Fi and enrollment applied by this machine onto a written Tilo disk image.
fn with_write_setup_fields(mut res: Resource) -> Resource {
    res = res.with_action(
        ActionSpec::prompted(
            "wifi-ssid",
            "Set Wi-Fi network",
            "Wi-Fi network",
            "",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetWifiSsid {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted_operation(
            "wifi-psk",
            "Set Wi-Fi password",
            "Wi-Fi password",
            "",
            OperationKind::Effect,
            IMAGER_OPERATION_TYPE,
            |text| {
                encode_operation(&ImagerOperation::SetWifiPsk {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "device-label",
            "Set device name",
            "Device name",
            "",
            ExecutionLocus::ClientLocal,
            |text| {
                encode(&ImagerAction::SetDeviceLabel {
                    value: text.to_string(),
                })
            },
        )
        .with_contract(profile_contract()),
    );
    res = res.with_action(
        ActionSpec::prompted(
            "enroll-identity",
            "Enroll this machine's identity",
            "Enroll (yes or no)",
            "no",
            ExecutionLocus::ClientLocal,
            |text| {
                let on = matches!(text.trim(), "yes" | "true" | "1");
                encode(&ImagerAction::SetEnrollLocalIdentity { on })
            },
        )
        .with_contract(profile_contract()),
    );
    res
}

fn with_filesystem(res: Resource, profile: &InstallProfile) -> Resource {
    let current = profile.filesystem;
    res.with_action(
        ActionSpec::prompted(
            "filesystem",
            "Set filesystem",
            "Filesystem (btrfs, ext4 or xfs)",
            current.key(),
            ExecutionLocus::ClientLocal,
            move |text| {
                encode(&ImagerAction::SetFilesystem {
                    filesystem: Filesystem::from_key(&text.to_ascii_lowercase()).unwrap_or(current),
                })
            },
        )
        .with_contract(profile_contract()),
    )
}

/// The applied, non-secret install-profile fields exported as Ctrl+K actions on
/// an open customizable image. Media-profile fields are what the live installer
/// consumes. Wi-Fi, device name, and identity enrollment live on
/// [`provide_write_setup`] and are applied onto the installed root after write.
pub fn provide_resources(source_label: &str, profile: &InstallProfile) -> Vec<Resource> {
    let label = if source_label.trim().is_empty() {
        "Tilo install options".to_string()
    } else {
        format!("Customize {source_label}")
    };
    let res = Resource::new(
        format!("imager.customization:{source_label}"),
        "imager.customization",
        label,
    );

    // Identity / region fields — prompted (the palette collects the new value).
    let res = with_filesystem(with_identity_fields(res), profile);

    vec![res]
}

/// Wi-Fi and identity enrollment applied by this machine during the write.
pub fn provide_write_setup(source_label: &str) -> Vec<Resource> {
    let label = if source_label.trim().is_empty() {
        "Setup on write".to_string()
    } else {
        format!("Setup on write: {source_label}")
    };
    let res = Resource::new(
        format!("imager.write-setup:{source_label}"),
        "imager.write-setup",
        label,
    );
    vec![with_write_setup_fields(res)]
}

fn source_resource(source: &Source) -> Resource {
    let clear = ActionSpec::immediate_encoded(
        "clear-source",
        "Clear reviewed image",
        ExecutionLocus::ClientLocal,
        encode(&ImagerAction::ClearSource),
    )
    .with_contract(ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["image-source".into(), "review".into()],
        ..ActionContract::default()
    });
    Resource::new(
        ImagerDoc::source_id(source),
        "imager.source",
        source.name.clone(),
    )
    .with_action(clear)
}

fn target_id(target: &TargetDrive) -> String {
    let identity = target.stable_id.as_deref().unwrap_or(&target.name);
    format!("imager.target:{identity}")
}

/// The complete resource topology, including local device/job observations.
/// Source/profile records are collaborative; device and job resources are
/// explicitly local capability projections and expose only safe status/write
/// operations with their contracts.
pub fn plan_resources(
    source: Option<&Source>,
    targets: &[TargetDrive],
    progress: &crate::model::Progress,
    profile: &InstallProfile,
) -> Vec<Resource> {
    let mut resources = Vec::new();
    let source_root = source.map_or_else(
        || Resource::new("imager.sources", "imager.sources", "Image sources"),
        |source| {
            Resource::new("imager.sources", "imager.sources", "Image sources")
                .with_child(source_resource(source))
        },
    );
    resources.push(source_root);

    if let Some(source) = source.filter(|source| {
        source.installable
            && source.kind == SourceKind::TiloImage
            && source.expected_sha256.as_deref().is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    }) {
        resources.extend(provide_resources(&source.name, profile));
        // Official images also offer on-write Wi-Fi / enrollment; the media
        // profile stays secret-free and never carries the passphrase or key.
        resources.extend(provide_write_setup(&source.name));
    } else if let Some(source) = source {
        resources.extend(provide_write_setup(&source.name));
    }

    let target_children = targets
        .iter()
        .map(|target| Resource::new(target_id(target), "imager.target", target.dev_path()));
    resources.push(
        Resource::new("imager.targets", "imager.targets", "Local write targets")
            .with_children(target_children),
    );

    let phase = progress.phase.key();
    resources.push(
        Resource::new(
            format!("imager.job:{phase}"),
            "imager.job",
            format!("Image write: {phase}"),
        )
        .with_action(operation_spec(
            "status",
            "Show image-write status",
            OperationKind::View,
            ImagerOperation::Status,
        )),
    );
    if matches!(
        progress.phase,
        crate::model::JobPhase::Success | crate::model::JobPhase::Failed
    ) {
        resources.push(Resource::new(
            format!("imager.result:{phase}"),
            "imager.result",
            if progress.phase == crate::model::JobPhase::Success {
                "Verified image result"
            } else {
                "Image write failure"
            },
        ));
    }
    resources
}

impl ResourceProvider for crate::app::ImagerApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        plan_resources(
            self.resource_source(),
            self.resource_targets(),
            self.resource_progress(),
            &ImagerDoc::profile(doc),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Source, SourceKind, sample_targets};
    use tilo_actions::LocalTransport;
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Dispatch an action through the executor, exactly as the on-screen
    /// affordance path does (decode from the wire, apply through a local
    /// transport).
    fn dispatch(doc: &mut AutoCommit, action: &ImagerAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A populated demo document (sample drives + an installable Tilo image +
    /// the default profile), so editing a profile field is a genuine change and
    /// the customizable source publishes its resource tree.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        doc.commit();
        doc
    }

    /// Profile field actions, normalisation and concurrent merges.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn profile_actions_write_fields_and_normalise_without_clobbering() {
        // Phase `profile_actions_write_the_profile`.
        // Each profile action writes its absolute value into `root["profile"]`.
        {
            let mut doc = seeded();
            assert_eq!(ImagerDoc::profile(&doc).mode, InstallMode::Live);
            assert!(!ImagerDoc::profile(&doc).encrypt);

            dispatch(
                &mut doc,
                &ImagerAction::SetInstallMode {
                    mode: "persistent".into(),
                },
            );
            dispatch(&mut doc, &ImagerAction::SetEncrypt { on: true });
            dispatch(&mut doc, &ImagerAction::SetAutologin { on: true });
            dispatch(
                &mut doc,
                &ImagerAction::SetHostname {
                    value: "lab01".into(),
                },
            );
            dispatch(
                &mut doc,
                &ImagerAction::SetFullName {
                    value: "Ada Lovelace".into(),
                },
            );
            dispatch(
                &mut doc,
                &ImagerAction::SetUsername {
                    value: "ada".into(),
                },
            );
            dispatch(
                &mut doc,
                &ImagerAction::SetLocale {
                    value: "nl_NL.UTF-8".into(),
                },
            );
            dispatch(
                &mut doc,
                &ImagerAction::SetTimezone {
                    value: "Europe/Amsterdam".into(),
                },
            );
            dispatch(&mut doc, &ImagerAction::SetKeyboard { value: "nl".into() });
            dispatch(
                &mut doc,
                &ImagerAction::SetFilesystem {
                    filesystem: Filesystem::Xfs,
                },
            );

            let p = ImagerDoc::profile(&doc);
            assert_eq!(p.mode, InstallMode::Persistent);
            assert!(p.encrypt);
            assert!(p.user.autologin);
            assert_eq!(p.hostname, "lab01");
            assert_eq!(p.user.full_name, "Ada Lovelace");
            assert_eq!(p.user.username, "ada");
            assert!(
                p.user.password.is_empty(),
                "secrets are never collaborative"
            );
            assert_eq!(p.locale, "nl_NL.UTF-8");
            assert_eq!(p.timezone, "Europe/Amsterdam");
            assert_eq!(p.keyboard, "nl");
            assert_eq!(p.filesystem, Filesystem::Xfs);
        }

        // Phase `field_set_preserves_the_other_fields`.
        // A single field-set leaves every *other* profile field untouched — the
        // action is a surgical set, not a whole-profile clobber.
        {
            let mut doc = seeded();
            let before = ImagerDoc::profile(&doc);
            dispatch(
                &mut doc,
                &ImagerAction::SetHostname {
                    value: "only-host".into(),
                },
            );
            let after = ImagerDoc::profile(&doc);
            assert_eq!(after.hostname, "only-host");
            assert_eq!(after.user.username, before.user.username);
            assert_eq!(after.timezone, before.timezone);
            assert_eq!(after.mode, before.mode);
            assert_eq!(after.encrypt, before.encrypt);
        }

        // Phase `malformed_install_mode_normalises_instead_of_wedging`.
        // A malformed install-mode payload normalises to the canonical default
        // rather than erroring, so an out-of-range replay lands on a valid mode.
        {
            let mut doc = seeded();
            dispatch(
                &mut doc,
                &ImagerAction::SetInstallMode {
                    mode: "nonsense".into(),
                },
            );
            assert_eq!(
                ImagerDoc::profile(&doc).mode,
                InstallMode::Live,
                "an unknown mode falls back to the canonical default"
            );
        }

        // Phase `concurrent_profile_fields_merge_without_a_whole_profile_clobber`.
        {
            let mut left = seeded();
            let bytes = left.save();
            let mut right = AutoCommit::load(&bytes).unwrap();
            ImagerAction::SetHostname {
                value: "studio".into(),
            }
            .apply(&mut left)
            .unwrap();
            ImagerAction::SetKeyboard { value: "de".into() }
                .apply(&mut right)
                .unwrap();
            left.commit();
            right.commit();
            left.merge_incoming(&mut right).unwrap();
            let profile = ImagerDoc::profile(&left);
            assert_eq!(profile.hostname, "studio");
            assert_eq!(profile.keyboard, "de");
        }
    }

    /// Palette and resource publication.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn published_resources_and_palette_advertise_only_transported_choices() {
        // Phase `ctrl_k_publishes_the_customization_and_profile_actions`.
        // Ctrl+K on an open customizable image publishes the customization resource
        // exporting the profile actions. Drives the real resource builder.
        {
            let resources = provide_resources("Tilo 1.0", &InstallProfile::default());
            assert!(
                resources[0]
                    .actions
                    .iter()
                    .all(|action| action.contract.reversible
                        && action.contract.retry == RetryPolicy::Idempotent)
            );
            assert_eq!(resources.len(), 1, "one customization root");
            let root = &resources[0];
            assert_eq!(root.kind, "imager.customization");
            assert_eq!(root.label, "Customize Tilo 1.0");
            assert_eq!(root.id.as_str(), "imager.customization:Tilo 1.0");

            let ids: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(
                ids,
                vec![
                    "locale",
                    "hostname",
                    "full-name",
                    "username",
                    "timezone",
                    "keyboard",
                    "filesystem",
                ],
            );
            // The password is deliberately never a palette row.
            assert!(
                !ids.contains(&"password"),
                "the secret is not exposed via Ctrl+K"
            );
        }

        // Phase `plain_iso_publishes_write_setup_not_media_profile`.
        // A plain ISO stays a first-class source resource. It never publishes a
        // media customization resource, but it does publish on-write Wi-Fi /
        // enrollment for a disk image flash.
        {
            use crate::app::ImagerApp;
            let mut app = ImagerApp::new();
            let mut doc = seeded();
            app.sync(&mut doc, &automerge::ROOT);
            // Seeded source is an installable Tilo image → customization and
            // on-write Wi-Fi / enrollment are both published.
            {
                let resources = ResourceProvider::resources(&app, &doc);
                assert!(
                    resources
                        .iter()
                        .any(|resource| resource.kind == "imager.customization"),
                    "official image publishes media customization"
                );
                let setup = resources
                    .iter()
                    .find(|resource| resource.kind == "imager.write-setup")
                    .expect("official image publishes write setup");
                let ids: Vec<&str> = setup.actions.iter().map(|a| a.id.as_str()).collect();
                assert_eq!(
                    ids,
                    vec!["wifi-ssid", "wifi-psk", "device-label", "enroll-identity"]
                );
            }

            // Switch to a plain, non-installable ISO → write setup, not media profile.
            let iso = Source {
                kind: SourceKind::IsoFile,
                path: "/home/user/plain.iso".into(),
                name: "plain.iso".into(),
                size: 1_000_000,
                label: "PLAIN".into(),
                installable: false,
                identity: None,
                expected_sha256: None,
            };
            ImagerDoc::set_source(&mut doc, Some(&iso));
            app.sync(&mut doc, &automerge::ROOT);
            let resources = ResourceProvider::resources(&app, &doc);
            assert!(resources.iter().any(|resource| {
                resource.kind == "imager.source"
                    || resource
                        .children
                        .iter()
                        .any(|child| child.kind == "imager.source")
            }));
            assert!(
                !resources
                    .iter()
                    .any(|resource| resource.kind == "imager.customization")
            );
            let setup = resources
                .iter()
                .find(|resource| resource.kind == "imager.write-setup")
                .expect("disk image publishes write setup");
            let ids: Vec<&str> = setup.actions.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["wifi-ssid", "wifi-psk", "device-label", "enroll-identity"]
            );
        }

        // Phase `palette_does_not_advertise_untransported_choices`.
        // Choices outside the secret-free media contract remain decodable for old
        // documents but are not offered as if the live installer would apply them.
        {
            let resources = provide_resources("Tilo 1.0", &InstallProfile::default());
            let ids = resources[0]
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>();
            assert!(!ids.contains(&"install-mode"));
            assert!(!ids.contains(&"encrypt"));
            assert!(!ids.contains(&"autologin"));
            assert!(!ids.contains(&"wifi-ssid"));
            assert!(!ids.contains(&"wifi-psk"));
            assert!(!ids.contains(&"enroll-identity"));
        }
    }

    /// Write operation availability, confirmation and cancellation.
    #[test]
    fn write_operation_is_confirmed_admin_gated_and_cancellable() {
        // Phase `write_operation_requires_typed_device_admin_and_destructive_confirmation`.
        {
            let resource = operation_resource(Some("/dev/sdz"), true, false, true);
            let write = resource
                .actions
                .iter()
                .find(|action| action.id == "write")
                .unwrap();
            assert!(write.contract.availability.is_available());
            assert_eq!(write.contract.permission, PermissionRequirement::Admin);
            assert_eq!(write.contract.risk, RiskLevel::Destructive);
            assert_eq!(
                write.contract.confirmation,
                ConfirmationPolicy::Phrase {
                    phrase: "/dev/sdz".into()
                }
            );
            assert_eq!(write.params().len(), 1);
            assert_eq!(write.params()[0].name.as_ref(), "confirmation");
            let encoded = write
                .build_args([(
                    "confirmation".to_string(),
                    tilo_actions::ArgValue::Text("/dev/sdz".into()),
                )])
                .unwrap();
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(ImagerOperation::Write {
                    device: "/dev/sdz".into(),
                    confirmation: "/dev/sdz".into(),
                })
            );
        }

        // Phase `write_operation_is_unavailable_when_the_real_backend_is_disabled`.
        {
            let resource = operation_resource(Some("/dev/sdz"), true, false, false);
            let write = resource
                .actions
                .iter()
                .find(|action| action.id == "write")
                .unwrap();
            assert!(!write.contract.availability.is_available());
            assert!(matches!(
                &write.contract.availability,
                tilo_actions::Availability::Unavailable { code, .. }
                    if code == "write_backend_disabled"
            ));
        }

        // Phase `automatic_verify_is_unavailable_and_active_write_can_be_cancelled`.
        {
            let resource = operation_resource(Some("/dev/sdz"), true, true, true);
            let verify = resource
                .actions
                .iter()
                .find(|action| action.id == "verify")
                .unwrap();
            assert!(!verify.contract.availability.is_available());
            let cancel = resource
                .actions
                .iter()
                .find(|action| action.id == "cancel")
                .unwrap();
            assert!(cancel.contract.availability.is_available());
            assert_eq!(cancel.contract.permission, PermissionRequirement::Write);
            let status = resource
                .actions
                .iter()
                .find(|action| action.id == "status")
                .unwrap();
            assert_eq!(status.contract.discovery, DiscoveryScope::Launchable);
        }
    }

    /// What a peer holding `doc` publishes: the collaborative source and
    /// profile come from the document, the local device observations from the
    /// sample drives with no job running.
    fn doc_resources(doc: &AutoCommit) -> Vec<Resource> {
        plan_resources(
            ImagerDoc::source(doc).as_ref(),
            &sample_targets(),
            &crate::model::Progress::default(),
            &ImagerDoc::profile(doc),
        )
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| doc_resources(doc),
        palette: ["Set hostname", "Set filesystem", "Clear reviewed image"],
        replay: [
            set_hostname => |_| encode(&ImagerAction::SetHostname {
                value: "lab01".into(),
            }),
            reselect_source => |doc| encode(&ImagerAction::SelectSource {
                source: Box::new(crate::model::sample_recent_sources()[1].clone()),
                previous: ImagerDoc::source(doc).map(Box::new),
                recent_rank: ImagerDoc::next_recent_rank(doc),
            }),
        ],
        undo: [
            set_encrypt => |_| encode(&ImagerAction::SetEncrypt { on: true }),
            clear_source => |_| encode(&ImagerAction::ClearSource),
        ],
        encodes: [
            hostname => ("imager.customization", "hostname", arg: "lab01", |_, _| {
                encode(&ImagerAction::SetHostname {
                    value: "lab01".into(),
                })
            }),
            filesystem => ("imager.customization", "filesystem", arg: "xfs", |_, _| {
                encode(&ImagerAction::SetFilesystem {
                    filesystem: Filesystem::Xfs,
                })
            }),
            clear_source => ("imager.source", "clear-source", |_, _| {
                encode(&ImagerAction::ClearSource)
            }),
        ],
    }

    /// Source MRU order and concurrent tie-breaking.
    #[test]
    fn source_history_orders_by_recency_with_stable_ties() {
        // Phase `sequential_source_reselection_moves_the_previous_source_to_mru_front`.
        {
            let mut doc = seeded();
            let first = crate::model::sample_source();
            let second = crate::model::sample_recent_sources()[1].clone();
            ImagerAction::SelectSource {
                source: Box::new(second.clone()),
                previous: Some(Box::new(first.clone())),
                recent_rank: -10,
            }
            .apply(&mut doc)
            .unwrap();
            ImagerAction::SelectSource {
                source: Box::new(first),
                previous: Some(Box::new(second.clone())),
                recent_rank: -11,
            }
            .apply(&mut doc)
            .unwrap();
            assert_eq!(
                ImagerDoc::recent(&doc).first().map(|source| &source.path),
                Some(&second.path)
            );
        }

        // Phase `concurrent_equal_rank_history_entries_have_stable_id_tie_breaking`.
        {
            let mut left = seeded();
            let mut right = AutoCommit::load(&left.save()).unwrap();
            let a = crate::model::sample_recent_sources()[1].clone();
            let b = crate::model::sample_recent_sources()[2].clone();
            ImagerAction::SelectSource {
                source: Box::new(a.clone()),
                previous: Some(Box::new(b.clone())),
                recent_rank: -77,
            }
            .apply(&mut left)
            .unwrap();
            ImagerAction::SelectSource {
                source: Box::new(b.clone()),
                previous: Some(Box::new(a.clone())),
                recent_rank: -77,
            }
            .apply(&mut right)
            .unwrap();
            left.commit();
            right.commit();
            left.merge_incoming(&mut right).unwrap();
            let head = ImagerDoc::recent(&left);
            assert_eq!(
                head[0].path,
                if ImagerDoc::source_id(&a) < ImagerDoc::source_id(&b) {
                    a.path
                } else {
                    b.path
                }
            );
        }
    }
}
