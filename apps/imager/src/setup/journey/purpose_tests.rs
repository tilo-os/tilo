//! The ordinary journey keeps the safe computer default without asking a
//! Purpose question.
use super::*;

#[test]
fn purpose_is_not_asked_and_the_safe_default_is_retained() {
    let mut journey = model::Journey::new("Ada".into());
    journey.destination = Some(Destination::Computer);
    journey.migrate = Some(false);
    journey.device_name = "Home device".into();
    journey.page = Page::Devices;
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Storage);
    assert_eq!(
        journey.purpose,
        install_profile_model::default_purpose(),
        "removing the question must not change the safe default"
    );
    assert!(journey.edit("purpose").is_err());

    let mut preview = InstallerPreview::default();
    preview.journey = journey;
    preview.journey.page = Page::Review;
    let profile = preview.public_install_profile().unwrap();
    assert_eq!(profile.purpose, install_profile_model::default_purpose());
}

#[test]
fn migration_still_follows_the_device_name_without_a_purpose_stop() {
    let mut journey = model::Journey::new("Ada".into());
    journey.destination = Some(Destination::Computer);
    journey.migrate = Some(true);
    journey.device_name = "Ada's computer".into();
    journey.page = Page::Devices;
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Belongings);
    journey.categories = [true, true, false];
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Storage);
    assert_eq!(
        journey.purpose,
        install_profile_model::default_purpose(),
        "the review and installed profile keep the same safe default"
    );
}

#[test]
fn desktop_has_no_os_purpose_page() {
    let mut journey = model::Journey::new("Ada".into());
    journey.destination = Some(Destination::Desktop);
    journey.migrate = Some(false);
    journey.device_name = "Existing Linux".into();
    journey.page = Page::Destination;
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
    assert_eq!(journey.history, vec![Page::Destination]);
    assert_eq!(journey.purpose, install_profile_model::default_purpose());
}

#[test]
fn legacy_purpose_values_remain_readable_but_are_not_asked() {
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
    let choices = preview.journey.draft_choices();
    let choices = serde_json::from_slice(&serde_json::to_vec(&choices).unwrap()).unwrap();
    let mut restored = model::Journey::new(String::new());
    restored.restore_draft(&choices, draft::IdentityKind::New, "Ada".into());
    assert_eq!(restored.purpose, "server");
}
