//! The ordinary journey defers sensitive handoff work to the destination.
use super::*;
use ui::theme::Theme;

fn review_preview() -> InstallerPreview {
    let mut preview = InstallerPreview::default();
    preview.journey.destination = Some(Destination::Computer);
    preview.journey.migrate = Some(false);
    preview.journey.identity = Some(Identity::New);
    preview.journey.name = "Ada".into();
    preview.journey.device_name = "Ada's computer".into();
    preview.journey.page = Page::Review;
    preview
}

#[test]
fn normal_flow_has_no_password_or_purpose_stop() {
    let mut journey = model::Journey::new("Ada".into());
    journey.destination = Some(Destination::Computer);
    journey.migrate = Some(false);
    journey.identity = Some(Identity::New);
    journey.name = "Ada".into();
    journey.device_name = "Ada's computer".into();
    journey.page = Page::Profile;
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Devices);
    assert_eq!(journey.status()["handoff"], "deferred");
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Storage);
    assert!(!journey.history.contains(&Page::Password));
    assert!(!journey.history.contains(&Page::Purpose));

    let mut migrating = model::Journey::new("Ada".into());
    migrating.destination = Some(Destination::Computer);
    migrating.migrate = Some(true);
    migrating.identity = Some(Identity::New);
    migrating.name = "Ada".into();
    migrating.device_name = "Ada's computer".into();
    migrating.page = Page::Profile;
    migrating.next().unwrap();
    assert_eq!(migrating.page, Page::Devices);
    migrating.next().unwrap();
    assert_eq!(migrating.page, Page::Belongings);
}

#[test]
fn deferred_review_names_the_destination_handoff_and_offers_no_secret_field() {
    let mut preview = review_preview();
    let root = preview.composition(&Theme::dark());
    let control = super::visual_tests::control;
    assert!(control(root.as_ref(), "installer.password").is_none());
    assert!(control(root.as_ref(), "installer.review.purpose").is_none());
    let handoff = control(root.as_ref(), "installer.review.handoff")
        .expect("deferred review must disclose where the remaining work happens");
    assert_eq!(
        handoff.accessibility().unwrap().name.as_deref(),
        Some(t("installer-preview-deferred-handoff").as_str())
    );
    assert!(preview.sensitive_handoff_deferred());
    preview.journey.password = "an explicit retained preparation password".into();
    assert!(!preview.sensitive_handoff_deferred());
}

#[test]
fn deferred_preparation_never_starts_a_ciphertext_package() {
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.destination = Some(Destination::Computer);
    preview.journey.migrate = Some(false);
    preview.journey.identity = Some(Identity::New);
    preview.journey.name = "Ada".into();
    preview.journey.device_name = "Ada's computer".into();
    preview.journey.page = Page::Belongings;
    preview.direct.context = None;
    preview.prepare_belongings().unwrap();
    assert!(!preview.media_preparation.busy());
    assert!(!preview.direct.belongings_pending);
    assert_eq!(preview.journey.status()["handoff"], "deferred");
}

#[test]
fn deferred_files_remain_selected_without_a_preparation_password() {
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.destination = Some(Destination::Computer);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [true, false, false];
    preview.journey.identity = Some(Identity::New);
    preview.journey.name = "Ada".into();
    preview.journey.device_name = "Ada's computer".into();
    preview.journey.page = Page::Belongings;
    preview.direct.context = None;
    preview.prepare_belongings().unwrap();
    assert!(preview.sensitive_handoff_deferred());
    assert!(!preview.media_preparation.busy());
    assert!(!preview.direct.belongings_pending);
    assert_eq!(preview.journey.status()["handoff"], "deferred");
    assert!(preview.journey.categories[0], "files stay selected");
}

#[test]
fn deferred_preparation_keeps_application_selection_without_a_package() {
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.destination = Some(Destination::Computer);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [true, true, false];
    preview.journey.identity = Some(Identity::New);
    preview.journey.name = "Ada".into();
    preview.journey.device_name = "Ada's computer".into();
    preview.journey.page = Page::Belongings;
    preview.direct.context = None;
    preview.prepare_belongings().unwrap();
    assert!(preview.sensitive_handoff_deferred());
    assert!(!preview.media_preparation.busy());
    assert!(preview.journey.categories[1], "applications stay selected");
    assert_eq!(preview.journey.status()["handoff"], "deferred");
}

#[test]
fn an_explicit_retained_password_keeps_the_legacy_encrypted_route() {
    let mut preview = InstallerPreview::default();
    preview.journey.password = "several unrelated installation words".into();
    preview.journey.confirmation = preview.journey.password.clone();
    preview.journey.page = Page::Password;
    assert!(!preview.sensitive_handoff_deferred());
    assert_eq!(preview.journey.status()["handoff"], "prepared");
    preview.journey.password = "short".into();
    preview.journey.confirmation = "short".into();
    assert_eq!(
        preview.journey.validation(),
        Some("installer-preview-password-short")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_source_computer_names_the_receiving_device_and_offers_approval() {
    let mut preview = InstallerPreview::default();
    preview.journey.destination = Some(Destination::Another);
    preview.journey.migrate = Some(true);
    preview.journey.identity = Some(Identity::New);
    preview.journey.name = "Ada".into();
    preview.journey.device_name = "Ada's new computer".into();
    preview.journey.page = Page::Review;
    let source = imager_backend::setup::other_device::Source::bind(
        uuid::Uuid::new_v4().to_string(),
        [4; 32],
    )
    .unwrap();
    preview.source_handoff = Some(imager_backend::setup::source_handoff::Handoff::from_source(
        source,
        zeroize::Zeroizing::new("test preparation key".into()),
    ));
    preview.source_state = Some(imager_backend::setup::source_handoff::SourceState::Waiting);
    let theme = Theme::dark();
    let root = preview.composition(&theme);
    let control = super::visual_tests::control;
    let waiting = control(root.as_ref(), "installer.review.source-handoff")
        .expect("the source must say to keep this computer on");
    assert_eq!(
        waiting.accessibility().unwrap().name.as_deref(),
        Some(t("installer-source-handoff-waiting").as_str())
    );
    assert!(control(root.as_ref(), "installer.source-handoff.approve").is_none());

    preview.source_state = Some(
        imager_backend::setup::source_handoff::SourceState::Requested {
            label: "Ada's new computer".into(),
            code: "123456".into(),
            approval: uuid::Uuid::new_v4().to_string(),
        },
    );
    let root = preview.composition(&theme);
    assert!(control(root.as_ref(), "installer.source-handoff.approve").is_some());
    assert!(
        control(root.as_ref(), "installer.review.source-handoff")
            .unwrap()
            .accessibility()
            .unwrap()
            .name
            .as_deref()
            .is_some_and(|text| text.contains("123456"))
    );
}
