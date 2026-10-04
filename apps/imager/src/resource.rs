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
    EffectAnswer, EffectCx, EffectStatus, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, ActionDecl, Doc, Effect, ModelError, Resource};
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

/// Framework app.
pub struct ImagerShell;

impl ResourceApp for ImagerShell {
    type Resource = Imager;
    const ID: &'static str = "so.tilo.imager";
    const TITLE: &'static str = "Disk Imager";
    const ICON: Option<&'static str> = Some("imager");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1180, 820)
    }

    fn view(cx: &mut ViewCx<Imager>) -> Element {
        cx.surface(ImagerApp::new())
    }
}

impl ResourceSurface<Imager, ImagerEffect> for ImagerApp {
    fn present(&mut self, state: &Rc<ImagerState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.attach_framework(cx.dispatcher());
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
        use ui::widget::Widget as _;

        let mut app = customizing();
        let perform =
            |app: &mut ui::framework::testing::TestApp<SampleImager>, target: &str, value: &str| {
                app.harness()
                    .with_widget_mut(|wizard: &mut ImagerApp| {
                        wizard.perform_semantic_action(
                            target,
                            &AccessAction::SetValue(value.into()),
                            &mut automerge::AutoCommit::new(),
                            &automerge::ROOT,
                        )
                    })
                    .expect("the hosted wizard")
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
        assert!(perform(
            &mut app,
            "imager:profile:timezone",
            "Europe/Not_A_Real_Zone"
        ));
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
            .with_widget_mut(|wizard: &mut ImagerApp| {
                wizard.focus_profile_field_for_test(crate::customize::CzField::Hostname);
            })
            .expect("the hosted wizard");
        app.harness().type_text("lab01");
        assert_eq!(app.state().profile.hostname, "lab01");
        assert!(app.undo());
        assert_eq!(
            app.state().profile.hostname,
            "",
            "the keystrokes of one field are one step"
        );
    }

    #[test]
    fn the_keyboard_chooser_commits_through_the_real_event_path() {
        let mut app = customizing();
        app.harness()
            .with_widget_mut(|wizard: &mut ImagerApp| {
                wizard.focus_profile_field_for_test(crate::customize::CzField::Keyboard);
            })
            .expect("the hosted wizard");
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
            "rescan",
            "choose-drive",
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
}
