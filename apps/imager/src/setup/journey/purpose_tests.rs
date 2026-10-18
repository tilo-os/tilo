//! Purpose stays independent of identity, migration and the selected OS.
use super::*;

#[test]
fn purpose_is_a_separate_semantic_decision_and_review_edit_is_reversible() {
    let mut preview = InstallerPreview::default();
    preview.journey.destination = Some(Destination::Computer);
    preview.journey.identity = Some(Identity::Existing);
    preview.journey.migrate = Some(false);
    preview.journey.device_name = "Home device".into();
    preview.journey.page = Page::Devices;
    preview.journey.next().unwrap();
    assert_eq!(preview.journey.page, Page::Purpose);
    preview
        .apply(InstallerPreviewEffect::ChoosePreview {
            choice: "server".into(),
        })
        .unwrap();
    assert_eq!(preview.journey.status()["purpose"], "server");
    assert_eq!(preview.journey.identity, Some(Identity::Existing));
    assert_eq!(preview.journey.migrate, Some(false));
    preview.journey.next().unwrap();
    assert_eq!(preview.journey.page, Page::Storage);
    preview.journey.next().unwrap();
    preview.journey.edit("purpose").unwrap();
    preview.journey.choose("computer", true).unwrap();
    preview.journey.back().unwrap();
    assert_eq!(preview.journey.page, Page::Review);
    assert_eq!(preview.journey.purpose, "server");
    preview.journey.edit("purpose").unwrap();
    preview.journey.choose("computer", true).unwrap();
    preview.journey.next().unwrap();
    assert_eq!(preview.journey.page, Page::Review);
    assert_eq!(preview.journey.purpose, "computer");
}

#[test]
fn purpose_survives_authenticated_profile_media_and_restart_choices() {
    let mut preview = InstallerPreview::default();
    preview.journey.purpose = "server".into();
    preview.journey.name = "Ada".into();
    let profile = preview.public_install_profile().unwrap();
    let encoded = serde_json::to_vec(&profile).unwrap();
    let decoded: crate::setup::prepared_setup::PublicInstallProfile =
        serde_json::from_slice(&encoded).unwrap();
    let draft = decoded.to_draft().unwrap();
    assert_eq!(draft.purpose, "server");
    let media = crate::setup::staging::handoff::profile_from_install(&draft).unwrap();
    assert_eq!(media["purpose"], "server");
    let staged = crate::handoff::stage_access_handoff(&draft, "", "", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(staged.path().join("etc/tilo/install-purpose")).unwrap(),
        "server\n"
    );
    assert!(!staged.path().join("var").exists());
    std::fs::remove_dir_all(staged.path()).unwrap();
    let mut legacy = serde_json::to_value(profile).unwrap();
    legacy.as_object_mut().unwrap().remove("purpose");
    let legacy: crate::setup::prepared_setup::PublicInstallProfile =
        serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.to_draft().unwrap().purpose, "computer");
    let choices = preview.journey.draft_choices();
    let choices = serde_json::from_slice(&serde_json::to_vec(&choices).unwrap()).unwrap();
    let mut restored = model::Journey::new(String::new());
    restored.restore_draft(&choices, draft::IdentityKind::New, "Ada".into());
    assert_eq!(restored.purpose, "server");
    preview.journey.purpose = "another-os".into();
    assert!(preview.public_install_profile().is_err());
}

#[test]
fn migration_still_follows_purpose_and_desktop_has_no_os_purpose_page() {
    let mut journey = model::Journey::new("Ada".into());
    journey.page = Page::Purpose;
    journey.migrate = Some(true);
    journey.choose("server", true).unwrap();
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Belongings);
    journey.page = Page::Devices;
    journey.destination = Some(Destination::Desktop);
    journey.device_name = "Existing Linux".into();
    journey.migrate = Some(false);
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Storage);
    journey.page = Page::Destination;
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
    assert_eq!(journey.history, vec![Page::Destination]);
}
