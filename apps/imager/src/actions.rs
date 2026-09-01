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
//! (see [`InstallProfile`](crate::model::InstallProfile)) — the install mode,
//! whether the disk is encrypted, whether the first user autologs in, and the
//! identity / region fields (hostname, full name, username, password, timezone,
//! keyboard). These are user-authored, they survive restarts, and they replicate
//! to collaborators, so editing one is a document-mutating operation: an
//! [`Action`]. Each carries the **absolute** target value in its payload, so
//! [`Action::apply`] is a pure function of `(payload, document)` — it reads the
//! current profile out of the document, sets one field, and writes it back, with
//! no wall-clock, no RNG, no external state — and replays byte-identically on
//! every replica. A malformed install-mode string normalises to the canonical
//! form rather than erroring, so a bad replay can't wedge a peer.
//!
//! The resource tree mirrors that: an *installable* qOS image (the only source a
//! profile applies to) is the presented resource, and the profile toggles +
//! fields are the [`ActionSpec`]s Ctrl+K offers for it. A plain ISO — written
//! as-is, no customization — publishes nothing. The password is dispatched as an
//! action by its on-screen field but deliberately kept **out** of the palette:
//! echoing a secret through a Ctrl+K prompt would leak it.

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionError, ActionInput, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource, ResourceProvider,
};

use crate::doc::ImagerDoc;
use crate::model::{InstallMode, InstallProfile};

/// Registry `type_id` for every imager action.
pub const IMAGER_ACTION_TYPE: &str = "imager.action";

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

/// [`ActionSpec::immediate`] for an [`ImagerAction`], falling back to the same
/// infallible envelope [`encode`] uses instead of panicking — see its
/// `# Panics` note. `ImagerAction`'s fields are plain strings/bools, so this
/// fallback path is never actually exercised.
fn immediate_spec(id: &str, label: &str, action: &ImagerAction) -> ActionSpec {
    ActionSpec::immediate(id, label, IMAGER_ACTION_TYPE, action).unwrap_or_else(|_| ActionSpec {
        id: id.to_string(),
        label: label.to_string(),
        description: None,
        locus: action.locus(),
        kind: qos_actions::OperationKind::Document,
        input: ActionInput::Immediate(encode(action)),
    })
}

/// Add the identity/region prompted fields — hostname, full name, username,
/// timezone, keyboard — to `res` in order. Split out of [`provide_resources`]
/// to keep it under the line budget; these five don't depend on `profile`,
/// only on the shape of [`ImagerAction`]. The password is intentionally
/// absent: a secret must never be echoed through a palette prompt.
fn with_identity_fields(mut res: Resource) -> Resource {
    res = res.with_action(ActionSpec::prompted(
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
    ));
    res = res.with_action(ActionSpec::prompted(
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
    ));
    res = res.with_action(ActionSpec::prompted(
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
    ));
    res = res.with_action(ActionSpec::prompted(
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
    ));
    res = res.with_action(ActionSpec::prompted(
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
    ));
    res
}

/// The install-profile toggles + fields, exported as Ctrl+K actions on the open
/// customizable image. The three toggles are immediate actions that set their
/// field to the *opposite* of its current document value, so choosing one in the
/// palette flips it exactly as the on-screen control does; the identity / region
/// fields are prompted actions that set the typed value. Level-triggered:
/// recomputed from `profile` each frame, so the labels and target values always
/// track the live state.
pub fn provide_resources(source_label: &str, profile: &InstallProfile) -> Vec<Resource> {
    let label = if source_label.trim().is_empty() {
        "qOS install options".to_string()
    } else {
        format!("Customize {source_label}")
    };
    let mut res = Resource::new(
        format!("imager.customization:{source_label}"),
        "imager.customization",
        label,
    );

    // Install mode — flip live ⇄ persistent (matches the Customize toggle).
    let next_mode = toggled_install_mode(profile.mode);
    res = res.with_action(immediate_spec(
        "install-mode",
        match next_mode {
            InstallMode::Persistent => "Switch to a persistent USB install",
            _ => "Switch to a live / installer stick",
        },
        &ImagerAction::SetInstallMode {
            mode: next_mode.key().to_string(),
        },
    ));

    // Disk encryption.
    res = res.with_action(immediate_spec(
        "encrypt",
        if profile.encrypt {
            "Disable disk encryption"
        } else {
            "Enable disk encryption"
        },
        &ImagerAction::SetEncrypt {
            on: !profile.encrypt,
        },
    ));

    // First-user autologin.
    res = res.with_action(immediate_spec(
        "autologin",
        if profile.user.autologin {
            "Turn off autologin"
        } else {
            "Turn on autologin"
        },
        &ImagerAction::SetAutologin {
            on: !profile.user.autologin,
        },
    ));

    // Identity / region fields — prompted (the palette collects the new value).
    res = with_identity_fields(res);

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

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
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
        assert_eq!(resources.len(), 1, "one customization root");
        let root = &resources[0];
        assert_eq!(root.kind, "imager.customization");
        assert_eq!(root.label, "Customize qOS 1.0");
        assert_eq!(root.id.as_str(), "imager.customization:qOS 1.0");

        let ids: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "install-mode",
                "encrypt",
                "autologin",
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
        assert!(
            labels.iter().any(|l| l.contains("encryption")),
            "{labels:?}"
        );
        assert!(labels.iter().any(|l| l.contains("hostname")), "{labels:?}");
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

    /// The palette's exported immediate actions track the live document: with a
    /// non-default profile seeded, each toggle targets the opposite of the
    /// current value.
    #[test]
    fn palette_actions_target_the_opposite_of_current_state() {
        let profile = InstallProfile {
            mode: InstallMode::Persistent,
            encrypt: true,
            ..InstallProfile::default()
        };
        let resources = provide_resources("qOS 1.0", &profile);

        let mode = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "install-mode")
            .unwrap();
        // Currently persistent → the exported action flips back to live.
        assert_eq!(
            immediate_encode(mode),
            encode(&ImagerAction::SetInstallMode {
                mode: "live".into()
            }),
        );
        let enc = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "encrypt")
            .unwrap();
        assert_eq!(
            immediate_encode(enc),
            encode(&ImagerAction::SetEncrypt { on: false }),
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

    /// Single source of truth: the palette's "encrypt" spec encodes
    /// byte-identically to the action the on-screen Customize toggle dispatches,
    /// for the same starting state.
    #[test]
    fn palette_and_toggle_encode_identically() {
        // Default profile (encrypt off) → the exported toggle targets `on: true`.
        let resources = provide_resources("qOS 1.0", &InstallProfile::default());
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "encrypt")
            .unwrap();

        let from_spec = immediate_encode(spec);
        let from_toggle = encode(&ImagerAction::SetEncrypt { on: true });
        assert_eq!(from_spec.type_id, from_toggle.type_id);
        assert_eq!(
            from_spec.payload, from_toggle.payload,
            "encrypt is one source of truth"
        );
    }
}
