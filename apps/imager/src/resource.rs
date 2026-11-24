//! The imager's resource: the image a person reviews, the images reviewed before
//! it, and the write.

use tilo_resource::{self, Availability, Effect, Id, Invoke, Keyed, Model, resource};

use crate::model::{InstallProfile, RecentSource, Source};

ui::embedded_catalog!(pub(crate) static CATALOG, ["de-DE"]);

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

impl Imager {
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "imager.sources",
    initialize = Imager::default,
    available = Imager::available,
    read_content = Imager::read_document
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
    /// Erase drive and write image. Offered on each drive it may write,
    #[effect(kind = "outside")]
    #[effect(
        job,
        cancellable,
        row,
        destructive,
        on = TargetDriveRow,
        phrase = "Self::drive_name"
    )]
    Write {
        #[param("Device path of the reviewed target drive, e.g. /dev/sdb")]
        device: String,
        #[param("The exact target device name shown by Imager, e.g. sdb")]
        confirmation: String,
    },
    /// Show image-write status.
    #[effect(returns, kind = "read", on = ImageWrite)]
    Status,
    /// Export headless configuration.
    #[effect(returns, kind = "read")]
    ExportConfig,
    /// Set Wi-Fi password.
    #[effect(kind = "outside")]
    SetWifiPsk {
        #[param(
            secret,
            "The Wi-Fi passphrase, kept on this machine for the next write"
        )]
        value: String,
    },
    /// Set the owner SSH public key for the next write on this machine.
    #[effect(kind = "outside")]
    SetSshKey {
        #[param("OpenSSH public keys, one per line, without authorized_keys options")]
        value: String,
    },
    /// Customize a regular raw image file through the installed-root path.
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    CustomizeImage {
        #[param("Absolute path of the regular raw disk image; devices and symlinks are refused")]
        image: tilo_resource::AbsolutePath,
        #[param("Optional hostname for the image")]
        hostname: Option<String>,
        #[param("Optional Wi-Fi network name")]
        wifi_ssid: Option<String>,
        #[param("Absolute path of a local file containing the Wi-Fi passphrase")]
        wifi_psk_file: Option<tilo_resource::AbsolutePath>,
        #[param("Absolute path of an owner public-key file, one OpenSSH key per line")]
        ssh_keys_file: Option<tilo_resource::AbsolutePath>,
        #[param("Absolute paths of files to place in the image account's home")]
        home_files: Vec<tilo_resource::AbsolutePath>,
    },
    #[effect(kind = "outside")]
    #[effect(job)]
    #[effect(label = "imager-drop-title")]
    ChooseImage,
    /// Cancel the installer download; what was verified is kept for resume.
    #[effect(kind = "outside", on = InstallerDownload)]
    CancelDownload,
    #[effect(own_device, kind = "read")]
    #[effect(label = "imager-rescan-drives")]
    Rescan,
    /// Choose target drive.
    #[effect(screen, on = TargetDriveRow)]
    ChooseDrive {
        #[param("Kernel name of a removable drive, e.g. sdb")]
        drive: String,
    },
}

impl ImagerEffect {
    fn drive_name(&self) -> String {
        match self {
            Self::Write { confirmation, .. } => confirmation.clone(),
            _ => String::new(),
        }
    }
}

/// The write of an image to a drive, as Imager publishes it: a row of the
/// `imager.job` type, offering its status.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.job", label = "Image write", effects = ImagerEffect)]
pub struct ImageWrite;

/// Tilo's installer download, as Imager publishes it: a row offering to
/// cancel what was verified for resume.
#[derive(tilo_resource::Row)]
#[row(
    kind = "imager.download",
    label = "Installer download",
    effects = ImagerEffect
)]
pub struct InstallerDownload;

/// One removable drive an image may be written to, as Imager publishes it:
/// a row offering to choose it and to write to it.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.target", label = "Drive", effects = ImagerEffect)]
pub struct TargetDriveRow;

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

fn review(sources: &[Source]) -> Vec<Invoke<Imager>> {
    sources
        .iter()
        .map(|source| Imager::SELECT_SOURCE.with(source.clone()))
        .collect()
}

/// The installer's id for its own window and resource. It runs before Tilo exists, so it is no
/// first-party app of a workspace and has no entry in the app catalogue.
pub const APP_ID: &str = "so.tilo.imager";

/// What a scenario's shot shows in the resource: the images reviewed before, and the one
/// reviewed now.
pub(crate) fn scenario(name: Option<&str>) -> Vec<Invoke<Imager>> {
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
        name if crate::hosting::Flow::of_scenario(name) == crate::hosting::Flow::Wizard => {
            history.push(crate::model::sample_source());
            review(&history)
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Resource as _;

    use crate::app::ImagerApp;
    use crate::hosting::testing::TestHost;

    fn customizing() -> TestHost {
        let mut app = crate::app::sample_host();
        app.invoke(Imager::SELECT_SOURCE.with(crate::model::sample_source()))
            .unwrap();
        app.with_widget(|wizard: &mut ImagerApp| wizard.show_customize_for_test())
            .expect("the hosted wizard");
        app
    }

    tilo_test_env::scenario! {
        fn the_customize_panel_edits_the_profile_through_checked_effects;

        phase customize_image_refuses_relative_paths;
        {
            let arguments = |image: &str, psk: &str| {
                serde_json::Map::from_iter([
                    ("image".to_owned(), serde_json::json!(image)),
                    ("wifi_ssid".to_owned(), serde_json::json!("studio")),
                    ("wifi_psk_file".to_owned(), serde_json::json!(psk)),
                    ("home_files".to_owned(), serde_json::json!([])),
                ])
            };
            // An absolute path is rooted for the host's path syntax: a drive on
            // Windows, the filesystem root elsewhere.
            let (image, psk) = if cfg!(windows) {
                (r"C:\a\disk.raw", r"C:\a\psk.txt")
            } else {
                ("/a/disk.raw", "/a/psk.txt")
            };
            for (image, psk) in [(image, "psk.txt"), ("disk.raw", psk)] {
                let error =
                    ImagerEffect::decode("customize-image", &arguments(image, psk)).unwrap_err();
                assert!(
                    matches!(
                        &error,
                        tilo_resource::EffectDecodeError::Argument { source, .. }
                            if source.to_string().contains("absolute")
                    ),
                    "{error}"
                );
            }
            assert!(
                ImagerEffect::decode("customize-image", &arguments(image, psk)).is_ok()
            );
        }

        phase assistive_profile_edits_reach_the_document;
        {
            use ui::a11y::Action as AccessAction;

            let mut app = customizing();
            let perform =
                |app: &mut TestHost, target: &str, value: &str| {
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

        phase a_member_clearing_the_source_or_editing_any_field_is_shown;
        {
            let mut app = customizing();
            app.merge_peer_edit(|doc| {
                let mut peer = Imager::read(doc).unwrap();
                peer.clear_source();
                tilo_resource::Fields::write_fields(&peer, doc, &automerge::ROOT).unwrap();
                let profile = Imager::paths().profile;
                for edit in [
                    profile.wifi_ssid.replace("lab"),
                    profile.enroll_local_identity.set(true),
                ] {
                    edit.apply_local(doc).unwrap();
                }
            });
            let shown = app.with_widget(|wizard: &mut ImagerApp| {
                    (
                        wizard.resource_source().cloned(),
                        wizard.profile_for_test().wifi_ssid.clone(),
                        wizard.profile_for_test().enroll_local_identity,
                    )
                })
                .expect("the hosted wizard");
            assert_eq!(shown, (None, "lab".to_owned(), true));
        }

        phase owner_ssh_key_effect_validates_and_stays_on_the_setup_machine;
        {
            let mut app = customizing();
            let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA owner@example";
            app.effect(&ImagerEffect::SetSshKey { value: key.into() })
                .unwrap();
            app.with_widget(|wizard: &mut ImagerApp| {
                    assert_eq!(wizard.ssh_key_for_test(), key);
                })
                .expect("the hosted wizard");
            assert!(
                app.effect(&ImagerEffect::SetSshKey {
                    value: "no-pty ssh-ed25519 invalid".into()
                })
                .is_err()
            );
            app.with_widget(|wizard: &mut ImagerApp| {
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

        phase profile_field_edits_check_their_values;
        {
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

        phase malformed_enrollment_calls_preserve_the_enabled_choice_and_document;
        {
            let mut app = customizing();
            let enroll = |on: bool| {
                serde_json::to_value(Imager::paths().profile.enroll_local_identity.set(on))
                    .expect("an edit is JSON")
            };
            app.call("field-edit", serde_json::json!({ "edit": enroll(true) }))
                .unwrap();
            assert!(app.state().profile.enroll_local_identity);
            let enabled_document = app.read_document(|doc| doc.clone().save());
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
                assert!(
                    matches!(
                        error,
                        ui::tilo_actions::ApiError::InvalidArguments(_)
                            | ui::tilo_actions::ApiError::Failed(_)
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

    fn find<'a>(
        resources: &'a [ui::tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    fn ids(resource: &ui::tilo_actions::ResourceSpec) -> Vec<&str> {
        resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    #[test]
    fn one_tree_holds_the_profile_actions_effects_drives_and_job() {
        let mut app = customizing();
        let resources = vec![app.published()];
        let root = find(&resources, Imager::KIND).expect("the imager resource");
        let root_ids = ids(root);
        for id in [
            "select-source",
            "clear-source",
            "field-edit",
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
        assert!(
            !root_ids.contains(&"write"),
            "a write is offered only on the drive it erases, behind its name"
        );

        let drive = find(&resources, "imager.target:demo-sandisk-ultra").expect("the USB stick");
        let mut offered: Vec<&str> = ids(drive);
        offered.sort_unstable();
        assert_eq!(offered, ["choose-drive", "write"]);
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

    tilo_test_env::scenario! {
        fn the_wizards_effects_choose_images_and_drives;

        phase effects_choose_drives_report_status_and_export_the_plan;
        {
            let mut app = crate::app::sample_host();
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
                app.effect_on_confirmed(
                    "imager.target:demo-sandisk-ultra",
                    &ImagerEffect::Write {
                        device: "/dev/sdb".into(),
                        confirmation: "sdb".into(),
                    }
                )
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

        phase the_image_chooser_is_an_effect_whose_pick_is_reviewed;
        {
            let directory = tempfile::tempdir().unwrap();
            let image = directory.path().join("plain.iso");
            std::fs::write(&image, b"an image for the chooser test").unwrap();
            let (services, recorder) = ui::process::ProcessServices::recording();
            let _host = ui::process::enter(&services);
            let mut app = crate::app::sample_host();
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

        phase a_drive_rows_write_chooses_that_drive_and_still_needs_its_name;
        {
            let mut app = customizing();
            let resources = vec![app.published()];
            let drive = find(&resources, "imager.target:demo-sandisk-ultra").unwrap();
            let write = drive
                .actions
                .iter()
                .find(|action| action.id == "write")
                .unwrap();
            if cfg!(feature = "host-media-writer") {
                assert!(
                    write.contract.availability.is_available(),
                    "this build ships its host's media writer"
                );
            } else {
                assert!(
                    !write.contract.availability.is_available(),
                    "no privileged writer in tests"
                );
            }
            let unconfirmed = app.call_on(
                "imager.target:demo-sandisk-ultra",
                "write",
                serde_json::json!({}),
            );
            if cfg!(feature = "host-media-writer") {
                assert!(
                    matches!(
                        unconfirmed,
                        Err(ui::tilo_actions::ApiError::ConfirmationRequired)
                    ),
                    "a write erases the drive only once its name is typed: {unconfirmed:?}"
                );
            } else {
                assert!(
                    matches!(
                        unconfirmed,
                        Err(ui::tilo_actions::ApiError::Unavailable { .. })
                    ),
                    "a build without the writer refuses the write: {unconfirmed:?}"
                );
            }

            let device = |name: &str| {
                crate::model::sample_targets()
                    .into_iter()
                    .find(|target| target.name == name)
                    .unwrap()
                    .dev_path()
            };
            let chosen = app.with_widget(|wizard: &mut ImagerApp| {
                    wizard.forget_target_for_test();
                    wizard.choose_for_write(&device("sdb"));
                    let sdb = wizard.target().map(|target| target.name.clone());
                    wizard.forget_target_for_test();
                    wizard.choose_for_write(&device("nvme0n1"));
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

    #[test]
    fn every_scenario_captures_its_flow_without_touching_the_machine() {
        for scenario in
            std::iter::once(None).chain(crate::hosting::scenarios().into_iter().map(Some))
        {
            let mut app = TestHost::capture(scenario);
            let inspection = app.harness().inspection_json().unwrap();
            assert!(!inspection.is_empty(), "{scenario:?}");
        }

        let mut drive = TestHost::capture(Some("drive"));
        assert_eq!(
            drive.state().source.map(|source| source.name),
            Some("Fedora-Workstation-Live-42.iso".to_owned()),
            "the scenario reviewed the image through the resource's actions"
        );
        assert!(
            drive.harness().inspection_json().unwrap().contains("sdb"),
            "the sample drives are staged"
        );
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
            let mut app = TestHost::capture(Some(scenario));
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
        let mut empty = TestHost::capture(Some("source-empty"));
        assert!(empty.state().source.is_none());
        assert!(!empty.harness().inspection_json().unwrap().contains("sdb"));
    }

    #[test]
    #[cfg(feature = "installation")]
    fn launch_switches_choose_the_flow() {
        let mut setup = TestHost::launched(&["--setup"]);
        let read = crate::setup::journey::InstallerPreviewEffect::ReadPreview;
        let page = setup.effect(&read).unwrap().value.unwrap()["page"].clone();
        assert_eq!(
            page, "destination",
            "the guided setup starts at its destination"
        );
        assert!(find(&[setup.published()], "imager.job").is_none());
    }

    #[test]
    #[cfg(not(any(feature = "installation", feature = "destination")))]
    fn without_the_installation_the_imager_is_the_write_wizard() {
        assert_eq!(crate::hosting::scenarios(), crate::app::SCENARIOS);
        assert_eq!(
            crate::hosting::Flow::of_scenario(Some("installer-welcome")),
            crate::hosting::Flow::Wizard
        );
    }
}

#[cfg(test)]
mod model_tests;
