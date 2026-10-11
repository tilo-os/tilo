//! The imager's resource: what a person reviews and authors.
//!
//! The document holds the image to write, the images reviewed before it and
//! the install profile. Profile fields change through the built-in field
//! edits (`Imager::paths().profile.hostname.replace(..)`); reviewing an
//! image is a verb, because it also remembers the image reviewed before it.
//! The privileged writer re-validates the reviewed image at execution time:
//! a collaborator's record is a claim, never trust.
//!
//! Everything else belongs to this machine: the drives the probe sees are
//! [`LiveRow`]s beneath the resource, and writing one is the
//! [`ImagerEffect::Write`] job, whose progress, cancellation and outcome
//! reach whoever started it — the Write button, the palette or an agent.
//! The Wi-Fi passphrase, credentials and the destructive confirmation never
//! enter the document.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, Helper, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Availability, Effect, Id, Invoke, Keyed, Model, resource};
use ui::widget::Element;

use crate::app::ImagerApp;
use crate::model::{InstallProfile, RecentSource, Source};

/// The imager's Fluent catalog.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "imager-setup-action-next").with_system_locale();

/// The reviewed image, the images reviewed before it and the install
/// profile: everything the imager keeps in its document.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Imager {
    /// Image to write. Only reviewing or clearing it changes it.
    #[model(readonly)]
    pub source: Option<Source>,
    /// Images reviewed before, by source id.
    #[model(readonly)]
    pub recent: Keyed<RecentSource>,
    /// Install profile
    pub profile: InstallProfile,
}

#[resource(
    kind = "imager.sources",
    label = "Image sources",
    initialize = Imager::default,
    available = Imager::available
)]
impl Imager {
    /// Review an image to write.
    ///
    /// The image reviewed before it moves to the front of the recent ones.
    #[action]
    pub fn select_source(&mut self, #[param("The reviewed image")] source: Source) {
        if let Some(previous) = self
            .source
            .take()
            .filter(|previous| !previous.path.is_empty() && previous.id() != source.id())
        {
            self.remember(previous);
        }
        self.source = Some(source);
    }

    /// Clear the reviewed image; the images reviewed before it stay.
    #[action]
    pub fn clear_source(&mut self) {
        self.source = None;
    }
}

impl Imager {
    /// Remember `source` as the most recent image reviewed before.
    fn remember(&mut self, source: Source) {
        let rank = self
            .recent
            .iter()
            .map(|recent| recent.rank)
            .min()
            .unwrap_or(0)
            .saturating_sub(1);
        let id = Id::from_raw(source.id());
        match self.recent.find_mut(&id) {
            Some(recent) => {
                recent.rank = rank;
                recent.source = source;
            }
            None => self.recent.push(RecentSource { id, rank, source }),
        }
        self.recent.settle();
    }

    /// Previously reviewed images, most recent first.
    pub fn recent_sources(&self) -> Vec<Source> {
        let mut recent: Vec<&RecentSource> = self.recent.iter().collect();
        recent.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        recent
            .into_iter()
            .map(|recent| recent.source.clone())
            .collect()
    }

    /// Nothing to clear before an image is reviewed.
    fn available(&self, verb: imager::Verb) -> Availability {
        match verb {
            imager::Verb::ClearSource if self.source.is_none() => Availability::Unavailable {
                code: "no-source".into(),
                reason: "No image is reviewed.".into(),
            },
            _ => Availability::Available,
        }
    }
}

/// What the imager does on this machine, with no document delta. Writing
/// erases the target drive; it runs as a job whose progress and outcome
/// reach its caller, and read-back verification is part of it.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.write-plan")]
pub enum ImagerEffect {
    /// Erase drive and write image.
    #[effect(job, cancellable)]
    Write {
        #[param("Device path of the reviewed target drive, e.g. /dev/sdb")]
        device: String,
        #[param("The exact target device name shown by Imager, e.g. sdb")]
        confirmation: String,
    },
    /// Show image-write status.
    #[effect(returns, kind = "read")]
    Status,
    /// Export headless configuration.
    #[effect(returns, kind = "read")]
    ExportConfig,
    /// Set Wi-Fi password.
    SetWifiPsk {
        #[param(
            secret,
            "The Wi-Fi passphrase, kept on this machine for the next write"
        )]
        value: String,
    },
    /// Set the owner SSH public key for the next write on this machine.
    SetSshKey {
        #[param("OpenSSH public keys, one per line, without authorized_keys options")]
        value: String,
    },
    /// Customize a regular raw image file through the installed-root path.
    #[effect(job, returns)]
    CustomizeImage {
        #[param("Path to the regular raw disk image; devices and symlinks are refused")]
        image: String,
        #[param("Optional hostname for the image")]
        hostname: Option<String>,
        #[param("Optional Wi-Fi network name")]
        wifi_ssid: Option<String>,
        #[param("Path to a local file containing the Wi-Fi passphrase")]
        wifi_psk_file: Option<String>,
        #[param("Path to an owner public-key file, one OpenSSH key per line")]
        ssh_keys_file: Option<String>,
        #[param("Files to place in the image account's home")]
        home_files: Vec<String>,
    },
    /// Choose a disk image.
    #[effect(job)]
    ChooseImage,
    /// Cancel the installer download; what was verified is kept for resume.
    CancelDownload,
    /// Rescan drives.
    #[effect(kind = "read")]
    Rescan,
    /// Choose target drive.
    #[effect(kind = "read")]
    ChooseDrive {
        #[param("Kernel name of a removable drive, e.g. sdb")]
        drive: String,
    },
}

/// The imager: one binary, three flows over the one resource. A plain
/// launch is the write wizard; `--setup` the guided Tilo setup;
/// `--import-folders` the later folder imports.
pub struct ImagerShell;

/// Which flow a window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Wizard,
    Setup,
    Imports,
}

impl Flow {
    /// The flow a screenshot scenario shows.
    fn of_scenario(scenario: Option<&str>) -> Self {
        match scenario {
            Some(name) if name.starts_with("setup-") => Self::Setup,
            Some(name) if name.starts_with("import-") => Self::Imports,
            _ => Self::Wizard,
        }
    }
}

/// The write wizard's screenshot scenarios, one per meaningful station.
const WIZARD_SCENARIOS: &[&str] = &[
    "source",
    "source-empty",
    "drive",
    "drive-empty",
    "drive-internal",
    "customize",
    "customize-timezone-open",
    "export",
    "confirm",
    "progress",
    "done",
    "done-failed",
];

/// Every screenshot scenario: the wizard's, then the setup's and the
/// imports', so the gallery reads top to bottom.
static SCENARIOS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    WIZARD_SCENARIOS
        .iter()
        .chain(crate::setup::view::SCENARIOS)
        .chain(crate::setup::import_view::SCENARIOS)
        .copied()
        .collect()
});

/// The plain ISO the drive scenarios review.
fn sample_iso() -> Source {
    Source {
        kind: crate::model::SourceKind::IsoFile,
        path: "/home/user/Downloads/Fedora-Workstation-Live-42.iso".into(),
        name: "Fedora-Workstation-Live-42.iso".into(),
        size: 2_113_929_216,
        label: "Fedora-WS-Live-42-1-1".into(),
        installable: false,
        identity: None,
        expected_sha256: None,
    }
}

/// Review `sources` in order, each one remembering the one before it.
fn review(sources: &[Source]) -> Vec<Invoke<Imager>> {
    sources
        .iter()
        .map(|source| Imager::SELECT_SOURCE.with(source.clone()))
        .collect()
}

impl ResourceApp for ImagerShell {
    type Resource = Imager;
    const ID: &'static str = "so.tilo.imager";
    const TITLE: &'static str = "Tilo Installer";
    const ICON: Option<&'static str> = Some("imager");
    const FLAGS: &'static [&'static str] = &["--setup", "--import-folders"];
    // Only hosts with a local workspace runtime can attach the imager.
    const ATTACHES_RUNTIME: bool = ui::collaboration::SESSION_RUNTIME_SUPPORTED;

    /// The privileged and staging helpers the imager's binary also is, and
    /// its packaging probes: each is started with its own command line and
    /// runs before anything of the app does.
    fn helpers() -> &'static [Helper] {
        &crate::helpers::HELPERS
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        &SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1180, 820)
    }

    fn fixture(fixture: &mut Fixture<Imager>) {
        let flow = Flow::of_scenario(fixture.scenario());
        fixture.provide(flow);
    }

    /// The reviewed image (and the images reviewed before it) each wizard
    /// scenario shows; the drives, the station and the job are staged by
    /// the surface.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Imager>> {
        let mut history = crate::model::sample_recent_sources();
        history.reverse();
        match name {
            Some("source-empty") => Vec::new(),
            Some("drive-empty") => review(&[sample_iso()]),
            Some("drive-internal") => review(&[crate::model::sample_source()]),
            Some("drive") => {
                history.extend([crate::model::sample_source(), sample_iso()]);
                review(&history)
            }
            name if Flow::of_scenario(name) == Flow::Wizard => {
                history.push(crate::model::sample_source());
                review(&history)
            }
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Imager>) -> Element {
        let flow = cx.fixture::<Flow>().map_or_else(
            || {
                if cx.launch_flag("--import-folders") {
                    Flow::Imports
                } else if cx.launch_flag("--setup") {
                    Flow::Setup
                } else {
                    Flow::Wizard
                }
            },
            |flow| *flow,
        );
        match flow {
            Flow::Wizard => cx.surface(ImagerApp::new()),
            Flow::Setup => crate::setup::view::view(cx),
            Flow::Imports => crate::setup::import_view::view(cx),
        }
    }
}

impl ResourceSurface<Imager, ImagerEffect> for ImagerApp {
    fn present(&mut self, state: &Rc<Imager>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.attach_control_actions(cx.actions());
        self.attach_framework(cx.dispatcher(), cx.jobs(), cx.mode());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("source"));
    }

    fn effect(&mut self, effect: ImagerEffect, cx: &mut EffectCx<Imager, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// The removable drives the probe sees and the write job: a handful,
    /// so all of them are published.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Resource as _;

    /// The wizard over the sample drives and releases, hosted as the live
    /// app hosts it.
    struct SampleImager;

    impl ResourceApp for SampleImager {
        type Resource = Imager;
        const ID: &'static str = "so.tilo.imager";
        const TITLE: &'static str = "Disk Imager";

        // In a column, so a harness lookup by type meets the hosted
        // surface among the column's children.
        fn view(cx: &mut ViewCx<Imager>) -> Element {
            Box::new(
                ui::widget::Column::new()
                    .with_no_padding()
                    .push(cx.surface(ImagerApp::sample())),
            )
        }
    }

    /// A hosted wizard on the Customize step of the sample Tilo image.
    fn customizing() -> ui::framework::testing::TestApp<SampleImager> {
        let mut app = ui::framework::testing::TestApp::<SampleImager>::new().unwrap();
        app.invoke(Imager::SELECT_SOURCE.with(crate::model::sample_source()))
            .unwrap();
        app.harness()
            .with_widget_mut(|wizard: &mut ImagerApp| wizard.show_customize_for_test())
            .expect("the hosted wizard");
        app
    }

    #[test]
    fn assistive_profile_edits_reach_the_document() {
        use ui::a11y::Action as AccessAction;

        let mut app = customizing();
        let perform =
            |app: &mut ui::framework::testing::TestApp<SampleImager>, target: &str, value: &str| {
                let id = target.replace(':', ".");
                app.harness()
                    .perform_named(&id, &AccessAction::SetValue(value.into()))
                    .is_ok()
            };
        assert!(perform(
            &mut app,
            "imager:profile:hostname",
            "accessible-host"
        ));
        assert_eq!(app.state().profile.hostname, "accessible-host");
        for (target, chosen, expected) in [
            ("imager:profile:locale", "de_DE.UTF-8", "de_DE.UTF-8"),
            ("imager:profile:timezone", "Europe/Berlin", "Europe/Berlin"),
            ("imager:profile:keyboard", "German (QWERTZ)", "de"),
        ] {
            assert!(perform(&mut app, target, chosen));
            let profile = app.state().profile;
            let actual = match target {
                "imager:profile:locale" => profile.locale,
                "imager:profile:timezone" => profile.timezone,
                _ => profile.keyboard,
            };
            assert_eq!(actual, expected);
        }
        assert!(
            !perform(
                &mut app,
                "imager:profile:timezone",
                "Europe/Not_A_Real_Zone"
            ),
            "the shared chooser refuses a value outside its canonical catalog"
        );
        assert_eq!(app.state().profile.timezone, "Europe/Berlin");
    }

    #[test]
    fn typing_a_hostname_is_one_undo_step_through_the_panel() {
        let mut app = customizing();
        app.invoke(Invoke::edit(&Imager::paths().profile.hostname.replace("")).unwrap())
            .unwrap();
        app.harness()
            .perform_named("imager.profile.hostname", &ui::a11y::Action::Focus)
            .unwrap();
        app.harness().type_text("lab01");
        assert_eq!(app.state().profile.hostname, "lab01");
        assert!(app.undo());
        assert_eq!(
            app.state().profile.hostname,
            "",
            "the keystrokes of one field are one step"
        );
        assert!(app.redo(), "the grouped field edit can be redone");
        assert_eq!(app.state().profile.hostname, "lab01");

        let previous_name = app.state().profile.user.full_name;
        app.invoke(
            Invoke::edit(
                &Imager::paths()
                    .profile
                    .user
                    .full_name
                    .replace("Second field"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(app.state().profile.user.full_name, "Second field");
        assert!(app.undo(), "a different field is a separate step");
        assert_eq!(app.state().profile.user.full_name, previous_name);
        assert_eq!(
            app.state().profile.hostname,
            "lab01",
            "undoing another field keeps the prior typed word"
        );
    }

    #[test]
    fn the_keyboard_chooser_commits_through_the_real_event_path() {
        let mut app = customizing();
        app.harness()
            .perform_named("imager.profile.keyboard", &ui::a11y::Action::Focus)
            .unwrap();
        app.harness().key("Enter");
        app.harness().type_text("german");
        app.harness().key("Enter");
        assert_eq!(app.state().profile.keyboard, "de");
    }

    #[test]
    fn a_collaborator_clearing_the_source_or_editing_any_field_is_shown() {
        let mut app = customizing();
        app.merge_peer_edit(|doc| {
            let mut peer = Imager::read(doc).unwrap();
            peer.clear_source();
            ui::tilo_resource::Fields::write_fields(&peer, doc, &automerge::ROOT).unwrap();
            let profile = Imager::paths().profile;
            for edit in [
                profile.wifi_ssid.replace("lab"),
                profile.enroll_local_identity.set(true),
            ] {
                edit.apply_local(doc).unwrap();
            }
        });
        let shown = app
            .harness()
            .with_widget_mut(|wizard: &mut ImagerApp| {
                (
                    wizard.resource_source().cloned(),
                    wizard.profile_for_test().wifi_ssid.clone(),
                    wizard.profile_for_test().enroll_local_identity,
                )
            })
            .expect("the hosted wizard");
        assert_eq!(shown, (None, "lab".to_owned(), true));
    }

    fn find<'a>(
        resources: &'a [ui::tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    fn ids(resource: &ui::tilo_actions::Resource) -> Vec<&str> {
        resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    #[test]
    fn one_tree_holds_the_profile_actions_effects_drives_and_job() {
        let mut app = customizing();
        let resources = app.harness().resources();
        let root = find(&resources, Imager::KIND).expect("the imager resource");
        let root_ids = ids(root);
        for id in [
            "select-source",
            "clear-source",
            "field-edit",
            "write",
            "status",
            "export-config",
            "set-wifi-psk",
            "set-ssh-key",
            "customize-image",
            "rescan",
            "choose-drive",
            "choose-image",
            "cancel-download",
        ] {
            assert!(root_ids.contains(&id), "{id} in {root_ids:?}");
        }
        let write = root
            .actions
            .iter()
            .find(|action| action.id == "write")
            .unwrap();
        assert!(
            !write.contract.availability.is_available(),
            "no privileged writer in tests"
        );

        let drive = find(&resources, "imager.target:demo-sandisk-ultra").expect("the USB stick");
        assert_eq!(ids(drive), ["choose-drive", "write"]);
        let row_write = drive
            .actions
            .iter()
            .find(|action| action.id == "write")
            .unwrap();
        assert_eq!(
            row_write.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "sdb".into()
            },
            "writing a drive asks for its name"
        );
        assert!(
            find(&resources, "imager.target:demo-samsung-990").is_none(),
            "an internal disk is not a write target here"
        );
        let job = find(&resources, "imager.job").expect("the write job");
        assert_eq!(ids(job), ["status"]);
        for row in [drive, job] {
            assert!(
                row.actions
                    .iter()
                    .all(|action| action.kind != ui::tilo_actions::OperationKind::Document)
            );
        }
    }

    #[test]
    fn effects_choose_drives_report_status_and_export_the_plan() {
        let mut app = ui::framework::testing::TestApp::<SampleImager>::new().unwrap();
        app.invoke(Imager::SELECT_SOURCE.with(crate::model::sample_source()))
            .unwrap();
        assert!(
            app.effect(&ImagerEffect::ExportConfig).is_err(),
            "no plan without a drive"
        );
        assert!(
            app.effect(&ImagerEffect::ChooseDrive {
                drive: "nvme0n1".into()
            })
            .is_err(),
            "an internal disk is refused"
        );
        app.effect(&ImagerEffect::ChooseDrive {
            drive: "sdb".into(),
        })
        .unwrap();
        let exported = app.effect(&ImagerEffect::ExportConfig).unwrap();
        assert!(
            exported
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .is_some_and(|text| text.contains("\"schema\": 1"))
        );
        let status = app.effect(&ImagerEffect::Status).unwrap();
        assert_eq!(
            status
                .value
                .as_ref()
                .and_then(|value| value["phase"].as_str()),
            Some("idle")
        );
        app.effect(&ImagerEffect::SetWifiPsk {
            value: "hunter22".into(),
        })
        .unwrap();
        assert!(
            app.effect(&ImagerEffect::Write {
                device: "/dev/sdb".into(),
                confirmation: "sdb".into(),
            })
            .is_err(),
            "the write is refused without the privileged writer"
        );
        assert!(
            app.read_document(
                |doc| !String::from_utf8_lossy(&doc.clone().save()).contains("hunter22")
            ),
            "the passphrase never reaches the document"
        );
    }

    #[test]
    fn owner_ssh_key_effect_validates_and_stays_on_the_setup_machine() {
        let mut app = customizing();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA owner@example";
        app.effect(&ImagerEffect::SetSshKey { value: key.into() })
            .unwrap();
        app.harness()
            .with_widget_mut(|wizard: &mut ImagerApp| {
                assert_eq!(wizard.ssh_key_for_test(), key);
            })
            .expect("the hosted wizard");
        assert!(
            app.effect(&ImagerEffect::SetSshKey {
                value: "no-pty ssh-ed25519 invalid".into()
            })
            .is_err()
        );
        app.harness()
            .with_widget_mut(|wizard: &mut ImagerApp| {
                assert_eq!(
                    wizard.ssh_key_for_test(),
                    key,
                    "a rejected effect preserves the accepted key"
                );
            })
            .expect("the hosted wizard");
        assert!(app.read_document(|doc| {
            !String::from_utf8_lossy(&doc.clone().save()).contains("owner@example")
        }));
    }

    #[test]
    fn profile_field_edits_check_their_values() {
        let mut app = customizing();
        let profile = Imager::paths().profile;
        app.call(
            "field-edit",
            serde_json::json!({ "edit": profile.filesystem.set(crate::model::Filesystem::Xfs) }),
        )
        .unwrap();
        assert_eq!(
            app.state().profile.filesystem,
            crate::model::Filesystem::Xfs
        );
        let mut fat =
            serde_json::to_value(profile.filesystem.set(crate::model::Filesystem::Xfs)).unwrap();
        let text = fat.to_string().replace("\"xfs\"", "\"fat\"");
        fat = serde_json::from_str(&text).unwrap();
        assert!(
            app.call("field-edit", serde_json::json!({ "edit": fat }))
                .is_err(),
            "a filesystem the installer does not know is refused"
        );
        app.call(
            "field-edit",
            serde_json::json!({ "edit": profile.enroll_local_identity.set(true) }),
        )
        .unwrap();
        assert!(app.state().profile.enroll_local_identity);
        assert!(
            app.call(
                "field-edit",
                serde_json::json!({ "edit": Imager::paths().source.set(None) })
            )
            .is_err(),
            "only reviewing or clearing changes the image"
        );
        app.call("clear-source", serde_json::json!({})).unwrap();
        assert!(app.state().source.is_none());
        assert!(
            app.call("clear-source", serde_json::json!({})).is_err(),
            "nothing to clear"
        );
    }

    #[test]
    fn malformed_enrollment_calls_preserve_the_enabled_choice_and_document() {
        let mut app = customizing();
        let enroll = |on: bool| {
            serde_json::to_value(Imager::paths().profile.enroll_local_identity.set(on))
                .expect("an edit is JSON")
        };
        app.call("field-edit", serde_json::json!({ "edit": enroll(true) }))
            .unwrap();
        assert!(app.state().profile.enroll_local_identity);
        let enabled_document = app.read_document(|doc| doc.clone().save());
        // The edit turning enrollment off, with its value replaced.
        let with_value = |value: serde_json::Value| {
            fn replace(json: &mut serde_json::Value, value: &serde_json::Value) {
                match json {
                    serde_json::Value::Object(fields) => {
                        for (key, field) in fields.iter_mut() {
                            if key == "value" {
                                *field = value.clone();
                            } else {
                                replace(field, value);
                            }
                        }
                    }
                    serde_json::Value::Array(items) => {
                        for item in items {
                            replace(item, value);
                        }
                    }
                    _ => {}
                }
            }
            let mut edit = enroll(false);
            replace(&mut edit, &value);
            serde_json::json!({ "edit": edit })
        };
        for arguments in [
            serde_json::json!({}),
            with_value(serde_json::json!("maybe")),
            with_value(serde_json::json!("false")),
            with_value(serde_json::Value::Null),
            with_value(serde_json::json!(0)),
            with_value(serde_json::json!([])),
            with_value(serde_json::json!({})),
            serde_json::json!({ "edit": enroll(false), "unexpected": true }),
        ] {
            let Err(error) = app.call("field-edit", arguments.clone()) else {
                panic!("{arguments}: invalid arguments must not create an enrollment edit");
            };
            // A malformed call is invalid arguments; a value of the wrong
            // type is refused by the field it would set.
            assert!(
                matches!(
                    error,
                    ui::tilo_actions::DoorError::InvalidArguments(_)
                        | ui::tilo_actions::DoorError::Failed(_)
                ),
                "{arguments}: {error}"
            );
            assert!(
                app.state().profile.enroll_local_identity,
                "{arguments}: rejected input turned enrollment off"
            );
            assert_eq!(
                app.read_document(|doc| doc.clone().save()),
                enabled_document,
                "{arguments}: rejected input mutated the document"
            );
        }
        app.call("field-edit", serde_json::json!({ "edit": enroll(false) }))
            .unwrap();
        assert!(!app.state().profile.enroll_local_identity);
        assert_ne!(
            app.read_document(|doc| doc.clone().save()),
            enabled_document,
            "an explicit Boolean false must still edit the choice"
        );
    }

    #[test]
    fn every_scenario_captures_its_flow_without_touching_the_machine() {
        for scenario in
            std::iter::once(None).chain(ImagerShell::scenarios().iter().copied().map(Some))
        {
            let mut app = ui::framework::testing::TestApp::<ImagerShell>::capture(scenario)
                .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
            let inspection = app.harness().inspection_json().unwrap();
            assert!(!inspection.is_empty(), "{scenario:?}");
        }

        let mut drive =
            ui::framework::testing::TestApp::<ImagerShell>::capture(Some("drive")).unwrap();
        assert_eq!(
            drive.state().source.map(|source| source.name),
            Some("Fedora-Workstation-Live-42.iso".to_owned()),
            "the scenario reviewed the image through the resource's actions"
        );
        assert!(
            drive.harness().inspection_json().unwrap().contains("sdb"),
            "the sample drives are staged"
        );
        // The confirm and customize stations review the official Tilo image:
        // its media profile and on-write setup are offered.
        for (scenario, shown) in [
            (
                "confirm",
                ["Tilo 1.0", "Wi-Fi network", "Enroll as a new device"],
            ),
            (
                "customize",
                ["Tilo 1.0", "Hostname", "Preview embedded setup"],
            ),
        ] {
            let mut app =
                ui::framework::testing::TestApp::<ImagerShell>::capture(Some(scenario)).unwrap();
            assert_eq!(
                app.state().source.map(|source| source.name),
                Some("Tilo 1.0".to_owned()),
                "{scenario}"
            );
            let inspection = app.harness().inspection_json().unwrap();
            for text in shown {
                assert!(inspection.contains(text), "{scenario} shows {text}");
            }
            assert!(
                !inspection.contains("written unchanged"),
                "{scenario} recognises the Tilo image"
            );
        }
        let mut empty =
            ui::framework::testing::TestApp::<ImagerShell>::capture(Some("source-empty")).unwrap();
        assert!(empty.state().source.is_none());
        assert!(!empty.harness().inspection_json().unwrap().contains("sdb"));
    }

    #[test]
    fn launch_switches_choose_the_flow() {
        let mut setup =
            ui::framework::testing::TestApp::<ImagerShell>::launched(&["--setup"]).unwrap();
        let resources = setup.harness().resources();
        assert!(
            find(&resources, "imager.setup").is_some(),
            "the guided setup"
        );
        assert!(find(&resources, "imager.job").is_none());
        let mut imports =
            ui::framework::testing::TestApp::<ImagerShell>::launched(&["--import-folders"])
                .unwrap();
        let resources = imports.harness().resources();
        assert!(
            find(&resources, "imager.import").is_some(),
            "the folder imports"
        );
    }

    #[test]
    fn the_image_chooser_is_an_effect_whose_pick_is_reviewed() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("plain.iso");
        std::fs::write(&image, b"an image for the chooser test").unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = ui::framework::testing::TestApp::<SampleImager>::new().unwrap();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(image));
        app.effect(&ImagerEffect::ChooseImage).unwrap();
        app.settle();
        assert_eq!(recorder.pick_requests()[0].title(), "Choose a disk image");
        assert_eq!(
            app.state().source.map(|source| source.name),
            Some("plain.iso".to_owned())
        );
        assert!(
            app.effect(&ImagerEffect::CancelDownload).is_err(),
            "no download runs"
        );
    }

    #[test]
    fn a_drive_rows_write_chooses_that_drive_and_still_needs_its_name() {
        let mut app = customizing();
        let resources = app.harness().resources();
        let drive = find(&resources, "imager.target:demo-sandisk-ultra").unwrap();
        let write = drive
            .actions
            .iter()
            .find(|action| action.id == "write")
            .unwrap();
        assert_eq!(
            write.contract.availability.is_available(),
            find(&resources, Imager::KIND)
                .unwrap()
                .actions
                .iter()
                .find(|action| action.id == "write")
                .unwrap()
                .contract
                .availability
                .is_available(),
            "a row's write is unavailable only when every write is"
        );
        assert!(matches!(
            app.call_on(
                "imager.target:demo-sandisk-ultra",
                "write",
                serde_json::json!({})
            ),
            Err(ui::tilo_actions::DoorError::ConfirmationRequired)
        ));

        let chosen = app
            .harness()
            .with_widget_mut(|wizard: &mut ImagerApp| {
                wizard.forget_target_for_test();
                wizard.choose_for_write("/dev/sdb");
                let sdb = wizard.target().map(|target| target.name.clone());
                wizard.forget_target_for_test();
                wizard.choose_for_write("/dev/nvme0n1");
                (sdb, wizard.target().is_none())
            })
            .unwrap();
        assert_eq!(
            chosen,
            (Some("sdb".to_owned()), true),
            "the write chooses its removable drive, never an internal disk"
        );
    }
}

#[cfg(test)]
mod model_tests;
