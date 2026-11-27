//! The journey's own model: the safe computer default kept without asking a
//! Purpose question, and a desktop route that never asks an OS question.
use super::*;

tilo_test_env::scenario! {
    fn the_purpose_keeps_its_safe_default_without_being_asked;

    phase purpose_is_not_asked_and_the_safe_default_is_retained;
    {
        let mut journey = Journey::new();
        journey.destination = Some(Destination::Computer);
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

        let mut preview = InstallerPreview {
            journey,
            ..Default::default()
        };
        preview.journey.page = Page::Review;
        let profile = preview.install_profile().unwrap();
        assert_eq!(profile.purpose, install_profile_model::default_purpose());
    }

    phase desktop_edits_never_restore_os_questions;
    {
        let mut journey = Journey::new();
        journey.choose("desktop", true).unwrap();
        journey.next().unwrap();
        assert_eq!(journey.page, Page::Review);
        assert_eq!(journey.history, vec![Page::Destination]);
        assert_eq!(journey.destination, Some(Destination::Desktop));
        assert_eq!(journey.purpose, install_profile_model::default_purpose());
        assert!(journey.password.is_empty());
        assert!(journey.device_name.is_empty());
        for section in ["password", "devices", "purpose"] {
            assert!(journey.edit(section).is_err());
            assert_eq!(journey.page, Page::Review);
        }
        journey.edit("destination").unwrap();
        journey.next().unwrap();
        assert_eq!(journey.page, Page::Review);
        journey.back().unwrap();
        assert_eq!(journey.page, Page::Destination);
        journey.choose("computer", true).unwrap();
        assert_eq!(journey.purpose, install_profile_model::default_purpose());
        journey.next().unwrap();
        assert_eq!(journey.page, Page::Migrate);
        journey.back().unwrap();
        journey.choose("desktop", true).unwrap();
        journey.next().unwrap();
        assert_eq!(journey.page, Page::Review);
        assert_eq!(journey.history, vec![Page::Destination]);
    }

    phase legacy_purpose_values_remain_readable_but_are_not_asked;
    {
        let mut preview = InstallerPreview::default();
        preview.journey.purpose = "server".into();
        let draft = preview.install_profile().unwrap();
        assert_eq!(draft.purpose, "server");
        let media = crate::setup::staging::handoff::profile_from_install(&draft).unwrap();
        assert_eq!(media["purpose"], "server");
    }
}
