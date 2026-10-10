//! The imager's document **actions** and the names it offers them by.
//!
//! The document holds what a person reviews and authors, and nothing that
//! belongs to one machine:
//!
//! - **The reviewed image** ([`ImagerAction::SelectSource`] /
//!   [`ImagerAction::ClearSource`]) and the images reviewed before it, kept
//!   as a keyed, ranked list so concurrent choices keep both records. The
//!   privileged writer still re-validates the image at execution time: a
//!   collaborator's record is a claim, never trust.
//! - **The install profile**: the secret-free fields the live installer
//!   applies (hostname, names, region, filesystem) and the on-write setup
//!   (Wi-Fi network, device name, enrollment). Each action sets one field to
//!   an absolute value, so concurrent edits of different fields merge and
//!   replay is a pure function of `(payload, document)`.
//!
//! Drives, the destructive confirmation, the Wi-Fi passphrase, write jobs
//! and downloads are this machine's: they are effects and live rows of the
//! surface ([`crate::resource::ImagerEffect`]), never document state.

use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, Doc, EncodedAction, ExecutionLocus, ParamType,
};
use ui::tilo_resource::ActionDecl;

use crate::doc::ImagerDoc;
use crate::model::{Filesystem, InstallMode, Source, SourceKind};
use crate::resource::Imager;

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

impl ImagerAction {
    /// Whether `self` and `next` set the same text field of the profile, so
    /// the keystrokes of one value are one undo step.
    pub(crate) fn same_text_field(&self, next: &Self) -> bool {
        use ImagerAction::{
            SetDeviceLabel, SetFullName, SetHostname, SetKeyboard, SetLocale, SetTimezone,
            SetUsername, SetWifiSsid,
        };
        matches!(
            (self, next),
            (SetHostname { .. }, SetHostname { .. })
                | (SetFullName { .. }, SetFullName { .. })
                | (SetUsername { .. }, SetUsername { .. })
                | (SetLocale { .. }, SetLocale { .. })
                | (SetTimezone { .. }, SetTimezone { .. })
                | (SetKeyboard { .. }, SetKeyboard { .. })
                | (SetWifiSsid { .. }, SetWifiSsid { .. })
                | (SetDeviceLabel { .. }, SetDeviceLabel { .. })
        )
    }
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

// ===========================================================================
// Named actions
// ===========================================================================

/// A declaration that sets one text field of the profile to `value`.
fn text_field(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    set: fn(String) -> ImagerAction,
) -> ActionDecl<Imager> {
    ActionDecl::new(
        id,
        label,
        vec![ActionParam::required("value", ParamType::Text, description)],
        move |args, _| {
            let value = args
                .text("value")
                .ok_or_else(|| "missing value".to_owned())?;
            Ok(set(value.to_owned()))
        },
    )
}

/// The secret-free media profile the live installer applies. Credentials,
/// destructive disk choices, autologin and persistence mode are not in the
/// media contract and are not offered as if they were preconfigured.
fn media_profile_declarations() -> Vec<ActionDecl<Imager>> {
    vec![
        text_field(
            "locale",
            "Set locale",
            "Locale, e.g. en_US.UTF-8",
            |value| ImagerAction::SetLocale { value },
        ),
        text_field("hostname", "Set hostname", "Hostname, e.g. tilo", |value| {
            ImagerAction::SetHostname { value }
        }),
        text_field(
            "full-name",
            "Set full name",
            "The first user's full name",
            |value| ImagerAction::SetFullName { value },
        ),
        text_field(
            "username",
            "Set username",
            "The first user's login name",
            |value| ImagerAction::SetUsername { value },
        ),
        text_field(
            "timezone",
            "Set timezone",
            "Time zone, e.g. Europe/Berlin",
            |value| ImagerAction::SetTimezone { value },
        ),
        text_field(
            "keyboard",
            "Set keyboard layout",
            "Keyboard layout, e.g. us",
            |value| ImagerAction::SetKeyboard { value },
        ),
        ActionDecl::new(
            "filesystem",
            "Set filesystem",
            vec![
                ActionParam::required("filesystem", ParamType::Text, "The installed filesystem")
                    .with_choices(["btrfs", "ext4", "xfs"]),
            ],
            |args, _| {
                let key = args
                    .text("filesystem")
                    .ok_or_else(|| "missing filesystem".to_owned())?
                    .to_ascii_lowercase();
                let filesystem = Filesystem::from_key(&key)
                    .ok_or_else(|| format!("unknown filesystem {key:?}"))?;
                Ok(ImagerAction::SetFilesystem { filesystem })
            },
        ),
    ]
}

/// Wi-Fi and identity enrollment this machine applies onto the written root.
/// The passphrase is the [`SetWifiPsk`](crate::resource::ImagerEffect::SetWifiPsk)
/// effect: it never enters the document.
fn write_setup_declarations() -> Vec<ActionDecl<Imager>> {
    vec![
        text_field(
            "wifi-ssid",
            "Set Wi-Fi network",
            "The Wi-Fi network the new machine joins",
            |value| ImagerAction::SetWifiSsid { value },
        ),
        text_field(
            "device-label",
            "Set device name",
            "The name of the device identity created for the target",
            |value| ImagerAction::SetDeviceLabel { value },
        ),
        ActionDecl::new(
            "enroll-identity",
            "Enroll this machine's identity",
            vec![ActionParam::required(
                "on",
                ParamType::Boolean,
                "Whether to enroll the person here onto the target as a new device",
            )],
            |args, _| {
                let on = args.boolean("on").ok_or_else(|| "missing on".to_owned())?;
                Ok(ImagerAction::SetEnrollLocalIdentity { on })
            },
        ),
    ]
}

/// The edits that apply with `source` reviewed: clearing it, its media
/// profile when it is an authenticated Tilo image, and on-write setup.
pub(crate) fn profile_declarations(source: Option<&Source>) -> Vec<ActionDecl<Imager>> {
    let Some(source) = source else {
        return Vec::new();
    };
    let mut declarations = vec![
        ActionDecl::new(
            "clear-source",
            "Clear reviewed image",
            Vec::new(),
            |_, _| Ok(ImagerAction::ClearSource),
        )
        .describe("Forget the reviewed image; the images reviewed before it stay."),
    ];
    if source_is_customizable(source) {
        declarations.extend(media_profile_declarations());
    }
    declarations.extend(write_setup_declarations());
    declarations
}

/// Whether an authenticated Tilo image may carry the install profile.
pub(crate) fn source_is_customizable(source: &Source) -> bool {
    source.installable
        && source.kind == SourceKind::TiloImage
        && source.expected_sha256.as_deref().is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tilo_actions::LocalTransport;
    use tilo_automerge::ApplyIncoming;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
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

    /// The ids `profile_declarations` offers with `source` reviewed.
    fn offered(source: Option<&Source>) -> Vec<&'static str> {
        profile_declarations(source)
            .iter()
            .map(|declaration| declaration.id)
            .collect()
    }

    /// Only an authenticated Tilo image carries the media profile; any image
    /// carries on-write setup; choices outside the media contract and the
    /// passphrase are never offered.
    #[test]
    fn named_actions_offer_only_what_the_reviewed_image_carries() {
        assert!(offered(None).is_empty(), "nothing to edit before a review");

        let official = crate::model::sample_source();
        assert_eq!(
            offered(Some(&official)),
            vec![
                "clear-source",
                "locale",
                "hostname",
                "full-name",
                "username",
                "timezone",
                "keyboard",
                "filesystem",
                "wifi-ssid",
                "device-label",
                "enroll-identity",
            ]
        );

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
        assert_eq!(
            offered(Some(&iso)),
            vec![
                "clear-source",
                "wifi-ssid",
                "device-label",
                "enroll-identity"
            ],
            "a plain image is written as-is"
        );
        for absent in [
            "install-mode",
            "encrypt",
            "autologin",
            "password",
            "wifi-psk",
        ] {
            assert!(!offered(Some(&official)).contains(&absent), "{absent}");
        }
    }

    /// What a peer holding `doc` publishes: the resource and the actions its
    /// reviewed image carries.
    fn doc_resources(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let state =
            <crate::resource::Imager as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
        let resources = vec![ui::tilo_resource::publish::<crate::resource::Imager>(
            "imager.sources",
            "Image sources",
            &state,
        )];
        resources
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

    #[test]
    fn a_new_imager_record_starts_with_empty_records() {
        use ui::tilo_resource::Resource as _;
        let mut doc = automerge::AutoCommit::new();
        assert!(ImagerDoc::is_empty(&doc));
        crate::resource::Imager::initialize(&mut doc).unwrap();
        assert!(!ImagerDoc::is_empty(&doc));
        crate::resource::Imager::initialize(&mut doc).unwrap();
        let state = crate::resource::Imager::read(&doc).unwrap();
        assert!(state.source.is_none());
        assert!(state.recent.is_empty());
    }
}
