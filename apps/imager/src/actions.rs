//! The imager's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! ## The split — the imager is mostly external
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) sorts every
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
//! (see [`InstallProfile`](crate::model::InstallProfile)) — the non-secret
//! identity / region fields that the live installer actually consumes
//! (hostname, full name, username, locale, timezone, keyboard). These are
//! user-authored, they survive restarts, and they replicate to collaborators,
//! so editing one is a document-mutating operation: an
//! [`Action`]. Each carries the **absolute** target value in its payload, so
//! [`Action::apply`] is a pure function of `(payload, document)` — it reads the
//! current profile out of the document, sets one field, and writes it back, with
//! no wall-clock, no RNG, no external state — and replays byte-identically on
//! every replica. A malformed install-mode string normalises to the canonical
//! form rather than erroring, so a bad replay can't wedge a peer.
//!
//! The resource tree mirrors that: an *installable* qOS image (the only source a
//! profile applies to) is the presented resource, and those applied fields are
//! the [`ActionSpec`]s Ctrl+K offers for it. A plain ISO — written as-is, no
//! customization — publishes nothing. Credentials, destructive disk choices,
//! autologin and persistence mode are deliberately absent: they are not in the
//! media contract and must not appear to be preconfigured.

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    ConfirmationPolicy, DiscoveryScope, Doc, EncodedAction, ExecutionLocus, ExecutionMode,
    OperationKind, ParamType, PermissionRequirement, Resource, ResourceProvider, ResultSpec,
    RetryPolicy, RiskLevel,
};

use crate::doc::ImagerDoc;
use crate::model::{InstallMode, InstallProfile};

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
        permission: PermissionRequirement::Admin,
        retry: RetryPolicy::Idempotent,
        tags: vec!["storage".into(), "job".into()],
        ..ActionContract::default()
    });
    let cancel = if job_active {
        cancel.unavailable(
            "cancel_not_cooperative",
            "The raw-device writer cannot yet stop safely between write chunks",
        )
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
    /// Set the first user's password (a secret — dispatched by the on-screen
    /// field, never surfaced through the Ctrl+K palette).
    SetPassword { value: String },
    /// Set the installed system locale.
    SetLocale { value: String },
    /// Set the system timezone.
    SetTimezone { value: String },
    /// Set the keyboard layout.
    SetKeyboard { value: String },
}

impl Action for ImagerAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Read the current profile, mutate exactly one field, write it back.
        // Reading the document is deterministic; the payload carries the
        // absolute target, so the same envelope replays identically everywhere.
        let mut p = ImagerDoc::profile(doc);
        match self {
            // Normalise through the enum so an unknown payload lands on a valid
            // mode instead of wedging.
            ImagerAction::SetInstallMode { mode } => p.mode = InstallMode::from_key(mode),
            ImagerAction::SetEncrypt { on } => p.encrypt = *on,
            ImagerAction::SetAutologin { on } => p.user.autologin = *on,
            ImagerAction::SetHostname { value } => p.hostname.clone_from(value),
            ImagerAction::SetFullName { value } => p.user.full_name.clone_from(value),
            ImagerAction::SetUsername { value } => p.user.username.clone_from(value),
            ImagerAction::SetPassword { value } => p.user.password.clone_from(value),
            ImagerAction::SetLocale { value } => p.locale.clone_from(value),
            ImagerAction::SetTimezone { value } => p.timezone.clone_from(value),
            ImagerAction::SetKeyboard { value } => p.keyboard.clone_from(value),
        }
        ImagerDoc::set_profile(doc, &p);
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
/// to keep it under the line budget; these five don't depend on `profile`,
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
            "qos",
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
            "qOS User",
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

/// The applied, non-secret install-profile fields exported as Ctrl+K actions on
/// an open customizable image. This list intentionally mirrors
/// `InstallProfile::to_media_profile_json`: never advertise a host-side choice
/// that the live installer cannot consume.
pub fn provide_resources(source_label: &str, _profile: &InstallProfile) -> Vec<Resource> {
    let label = if source_label.trim().is_empty() {
        "qOS install options".to_string()
    } else {
        format!("Customize {source_label}")
    };
    let res = Resource::new(
        format!("imager.customization:{source_label}"),
        "imager.customization",
        label,
    );

    // Identity / region fields — prompted (the palette collects the new value).
    let res = with_identity_fields(res);

    vec![res]
}

impl ResourceProvider for crate::app::ImagerApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Only an installable qOS image has a profile to customize; a plain ISO
        // is written as-is, so it publishes nothing.
        match self.customization_source_label() {
            Some(label) => provide_resources(&label, &ImagerDoc::profile(doc)),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Source, SourceKind, sample_targets};
    use qos_actions::{ActionInput, LocalTransport};
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
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A populated demo document (sample drives + an installable qOS image +
    /// the default profile), so editing a profile field is a genuine change and
    /// the customizable source publishes its resource tree.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        doc.commit();
        doc
    }

    /// Each profile action writes its absolute value into `root["profile"]`.
    #[test]
    fn profile_actions_write_the_profile() {
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
            &ImagerAction::SetPassword {
                value: "s3cret".into(),
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

        let p = ImagerDoc::profile(&doc);
        assert_eq!(p.mode, InstallMode::Persistent);
        assert!(p.encrypt);
        assert!(p.user.autologin);
        assert_eq!(p.hostname, "lab01");
        assert_eq!(p.user.full_name, "Ada Lovelace");
        assert_eq!(p.user.username, "ada");
        assert_eq!(p.user.password, "s3cret");
        assert_eq!(p.locale, "nl_NL.UTF-8");
        assert_eq!(p.timezone, "Europe/Amsterdam");
        assert_eq!(p.keyboard, "nl");
    }

    /// A single field-set leaves every *other* profile field untouched — the
    /// action is a surgical set, not a whole-profile clobber.
    #[test]
    fn field_set_preserves_the_other_fields() {
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

    /// A malformed install-mode payload normalises to the canonical default
    /// rather than erroring, so an out-of-range replay lands on a valid mode.
    #[test]
    fn malformed_install_mode_normalises_instead_of_wedging() {
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

    /// Ctrl+K on an open customizable image publishes the customization resource
    /// exporting the profile actions, and the one palette lists them as rows.
    /// Drives the real resource-builder and the real [`ui::ActionPalette`].
    #[test]
    fn ctrl_k_publishes_the_customization_and_profile_actions() {
        let resources = provide_resources("qOS 1.0", &InstallProfile::default());
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
        assert_eq!(root.label, "Customize qOS 1.0");
        assert_eq!(root.id.as_str(), "imager.customization:qOS 1.0");

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
            ],
        );
        // The password is deliberately never a palette row.
        assert!(
            !ids.contains(&"password"),
            "the secret is not exposed via Ctrl+K"
        );

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("hostname")), "{labels:?}");
        assert!(
            labels.iter().all(|label| !label.contains("encryption")
                && !label.contains("autologin")
                && !label.contains("persistent")),
            "untransported choices must not be advertised: {labels:?}"
        );
    }

    #[test]
    fn write_operation_requires_typed_device_admin_and_destructive_confirmation() {
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
                qos_actions::ArgValue::Text("/dev/sdz".into()),
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

    #[test]
    fn write_operation_is_unavailable_when_the_real_backend_is_disabled() {
        let resource = operation_resource(Some("/dev/sdz"), true, false, false);
        let write = resource
            .actions
            .iter()
            .find(|action| action.id == "write")
            .unwrap();
        assert!(!write.contract.availability.is_available());
        assert!(matches!(
            &write.contract.availability,
            qos_actions::Availability::Unavailable { code, .. }
                if code == "write_backend_disabled"
        ));
    }

    #[test]
    fn unsupported_verify_and_cancel_are_explicitly_unavailable() {
        let resource = operation_resource(Some("/dev/sdz"), true, true, true);
        for id in ["verify", "cancel"] {
            let action = resource
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap();
            assert!(!action.contract.availability.is_available(), "{id}");
        }
        let status = resource
            .actions
            .iter()
            .find(|action| action.id == "status")
            .unwrap();
        assert_eq!(status.contract.discovery, DiscoveryScope::Launchable);
    }

    /// A plain ISO (or no source) has no profile to customize, so the app
    /// publishes an empty resource tree. Drives the real `ResourceProvider`
    /// through the app's own `sync` (the source is presentation state read back
    /// from the document each frame).
    #[test]
    fn plain_iso_publishes_no_resources() {
        use crate::app::ImagerApp;
        let mut app = ImagerApp::new();
        let mut doc = seeded();
        app.sync(&mut doc, &automerge::ROOT);
        // Seeded source is an installable qOS image → it publishes.
        assert!(!ResourceProvider::resources(&app, &doc).is_empty());

        // Switch to a plain, non-installable ISO → nothing to customize.
        let iso = Source {
            kind: SourceKind::IsoFile,
            path: "/home/user/plain.iso".into(),
            name: "plain.iso".into(),
            size: 1_000_000,
            label: "PLAIN".into(),
            installable: false,
        };
        ImagerDoc::set_source(&mut doc, Some(&iso));
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            ResourceProvider::resources(&app, &doc).is_empty(),
            "a plain ISO exposes no customization actions"
        );
    }

    /// A prompted field action builds the byte-identical [`EncodedAction`] the
    /// on-screen field dispatches for the same typed value.
    #[test]
    fn prompted_field_builds_the_expected_action() {
        let resources = provide_resources("qOS 1.0", &InstallProfile::default());
        let hostname = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "hostname")
            .unwrap();
        let built = match &hostname.input {
            ActionInput::Prompted { build, .. } => build("lab01"),
            ActionInput::Immediate(_) => panic!("hostname is a prompted action"),
        };
        assert_eq!(
            built,
            encode(&ImagerAction::SetHostname {
                value: "lab01".into()
            })
        );
    }

    /// The full undo covenant on the real executor path: dispatched through the
    /// executor the action mutates the document and records exactly one undo
    /// boundary, and Ctrl+Z (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        ImagerDoc::seed(&mut state.doc, &sample_targets());
        state.doc.commit();
        assert!(!ImagerDoc::profile(&state.doc).encrypt);
        let before_heads = state.doc.get_heads();

        let encoded = encode(&ImagerAction::SetEncrypt { on: true });
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            ImagerDoc::profile(&state.doc).encrypt,
            "the action enabled encryption"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            !ImagerDoc::profile(&state.doc).encrypt,
            "undo reverted the encryption change and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same value.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = ImagerAction::SetHostname {
            value: "lab01".into(),
        };
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(ImagerDoc::profile(&b).hostname, "lab01");
    }

    /// Choices outside the secret-free media contract remain decodable for old
    /// documents but are not offered as if the live installer would apply them.
    #[test]
    fn palette_does_not_advertise_untransported_choices() {
        let resources = provide_resources("qOS 1.0", &InstallProfile::default());
        let ids = resources[0]
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert!(!ids.contains(&"install-mode"));
        assert!(!ids.contains(&"encrypt"));
        assert!(!ids.contains(&"autologin"));
    }
}
