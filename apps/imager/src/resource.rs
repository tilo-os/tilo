//! Imager as a [`tilo_resource::Resource`].
//!
//! The document holds what a person reviews and authors: the image to write,
//! the images reviewed before it and the install profile. Its edits are
//! [`ImagerAction`]s, offered by name for the source and profile that apply
//! now ([`Resource::bound_declarations`]).
//!
//! Everything else belongs to this machine: the drives the probe sees are
//! [`LiveRow`]s beneath the resource, and writing one is the
//! [`ImagerEffect::Write`] job, whose progress, cancellation and outcome
//! reach whoever started it — the Write button, the palette or an agent.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, Helper, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, ActionDecl, Doc, Effect, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{IMAGER_ACTION_TYPE, ImagerAction, profile_declarations, register};
use crate::app::ImagerApp;
use crate::doc::ImagerDoc;
use crate::model::{InstallProfile, Source};

/// The reviewed image, the images reviewed before it and the install
/// profile: everything the imager keeps in its document.
#[derive(Clone, Debug, Default)]
pub struct ImagerState {
    pub source: Option<Source>,
    /// Previously reviewed images, most recent first.
    pub recent: Vec<Source>,
    /// The rank a newly remembered image takes: before every recent one.
    pub next_recent_rank: i64,
    pub profile: InstallProfile,
}

/// The disk-imager sources / write-plan resource.
pub struct Imager;

impl Resource for Imager {
    const KIND: &'static str = "imager.sources";
    const LABEL: &'static str = "Image sources";
    const ACTION_TYPE: &'static str = IMAGER_ACTION_TYPE;

    type State = ImagerState;
    type Action = ImagerAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ImagerState, ModelError> {
        Ok(ImagerState {
            source: ImagerDoc::source(doc),
            recent: ImagerDoc::recent(doc),
            next_recent_rank: ImagerDoc::next_recent_rank(doc),
            profile: ImagerDoc::profile(doc),
        })
    }

    /// An empty reviewed source, recent list and install profile.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        ImagerDoc::seed(doc);
        Ok(())
    }

    /// Typing into one profile field is one step until the field changes.
    fn coalesces(previous: &ImagerAction, next: &ImagerAction) -> bool {
        previous.same_text_field(next)
    }

    /// Clearing the reviewed image, and the profile fields that image
    /// carries: the media profile only for an authenticated Tilo image, the
    /// on-write Wi-Fi and enrollment for any image.
    fn bound_declarations(state: &ImagerState) -> Vec<ActionDecl<Self>> {
        profile_declarations(state.source.as_ref())
    }
}

/// What the imager does on this machine, with no document delta. Writing
/// erases the target drive; it runs as a job whose progress and outcome
/// reach its caller, and read-back verification is part of it.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.write-plan")]
pub enum ImagerEffect {
    /// Erase drive and write image.
    #[effect(job, cancellable, permission = "admin")]
    Write {
        #[param("Device path of the reviewed target drive, e.g. /dev/sdb")]
        device: String,
        #[param("The exact target device name shown by Imager, e.g. sdb")]
        confirmation: String,
    },
    /// Show image-write status.
    #[effect(returns, permission = "read")]
    Status,
    /// Export headless configuration.
    #[effect(returns, permission = "read")]
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
    #[effect(permission = "admin")]
    SetSshKey {
        #[param("OpenSSH public keys, one per line, without authorized_keys options")]
        value: String,
    },
    /// Customize a regular raw image file through the installed-root path.
    #[effect(job, returns, permission = "admin")]
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
    #[effect(permission = "read")]
    Rescan,
    /// Choose target drive.
    #[effect(permission = "read")]
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
    let mut previous: Option<&Source> = None;
    let mut invokes = Vec::new();
    for (rank, source) in sources.iter().enumerate() {
        invokes.extend(
            Invoke::apply(&ImagerAction::SelectSource {
                source: Box::new(source.clone()),
                previous: previous.cloned().map(Box::new),
                recent_rank: -(rank as i64),
            })
            .ok(),
        );
        previous = Some(source);
    }
    invokes
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
    fn present(&mut self, state: &Rc<ImagerState>) {
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
    use ui::tilo_actions::Action as _;

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
        app.invoke(
            ui::tilo_resource::Invoke::apply(&ImagerAction::SelectSource {
                source: Box::new(crate::model::sample_source()),
                previous: None,
                recent_rank: 0,
            })
            .unwrap(),
        )
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
        app.invoke(
            ui::tilo_resource::Invoke::apply(&ImagerAction::SetHostname {
                value: String::new(),
            })
            .unwrap(),
        )
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
            ui::tilo_resource::Invoke::apply(&ImagerAction::SetFullName {
                value: "Second field".into(),
            })
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
            ImagerAction::ClearSource.apply(doc).unwrap();
            ImagerAction::SetWifiSsid {
                value: "lab".into(),
            }
            .apply(doc)
            .unwrap();
            ImagerAction::SetEnrollLocalIdentity { on: true }
                .apply(doc)
                .unwrap();
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
            "clear-source",
            "hostname",
            "filesystem",
            "wifi-ssid",
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
        app.invoke(
            ui::tilo_resource::Invoke::apply(&ImagerAction::SelectSource {
                source: Box::new(crate::model::sample_source()),
                previous: None,
                recent_rank: 0,
            })
            .unwrap(),
        )
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
    fn declared_profile_actions_check_their_arguments() {
        let mut app = customizing();
        app.call("filesystem", serde_json::json!({ "filesystem": "xfs" }))
            .unwrap();
        assert_eq!(
            app.state().profile.filesystem,
            crate::model::Filesystem::Xfs
        );
        assert!(
            app.call("filesystem", serde_json::json!({ "filesystem": "fat" }))
                .is_err()
        );
        app.call("enroll-identity", serde_json::json!({ "on": true }))
            .unwrap();
        assert!(app.state().profile.enroll_local_identity);
        app.call("clear-source", serde_json::json!({})).unwrap();
        assert!(app.state().source.is_none());
    }

    #[test]
    fn malformed_enrollment_calls_preserve_the_enabled_choice_and_document() {
        let mut app = customizing();
        app.call("enroll-identity", serde_json::json!({ "on": true }))
            .unwrap();
        assert!(app.state().profile.enroll_local_identity);
        let enabled_document = app.read_document(|doc| doc.clone().save());
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({ "on": "maybe" }),
            serde_json::json!({ "on": "false" }),
            serde_json::json!({ "on": null }),
            serde_json::json!({ "on": 0 }),
            serde_json::json!({ "on": [] }),
            serde_json::json!({ "on": {} }),
            serde_json::json!({ "enabled": false }),
            serde_json::json!({ "on": false, "unexpected": true }),
        ] {
            let error = app
                .call("enroll-identity", arguments.clone())
                .expect_err("invalid arguments must not create an enrollment edit");
            assert!(
                matches!(error, ui::tilo_actions::DoorError::InvalidArguments(_)),
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
        app.call("enroll-identity", serde_json::json!({ "on": false }))
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
