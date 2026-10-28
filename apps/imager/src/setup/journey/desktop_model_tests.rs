use super::{
    draft,
    model::{Destination, Journey, Page},
};

#[test]
fn desktop_draft_and_edits_never_restore_os_questions() {
    let mut journey = Journey::new("Old profile".into());
    journey.destination = Some(Destination::Desktop);
    journey.migrate = Some(true);
    journey.password = "old password".into();
    journey.device_name = "old device".into();
    let choices = journey.draft_choices();
    journey.restore_draft(&choices, draft::IdentityKind::New, "Old profile".into());
    assert_eq!(journey.page, Page::Review);
    assert_eq!(journey.history, vec![Page::Destination]);
    assert!(journey.name.is_empty());
    assert!(journey.password.is_empty());
    assert!(journey.device_name.is_empty());
    assert!(journey.identity.is_none());
    assert!(journey.migrate.is_none());
    for section in ["profile", "identity", "password", "devices", "belongings"] {
        assert!(journey.edit(section).is_err());
        assert_eq!(journey.page, Page::Review);
    }
    journey.edit("destination").unwrap();
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
    journey.back().unwrap();
    assert_eq!(journey.page, Page::Destination);
    journey.choose("computer", true).unwrap();
    assert_eq!(journey.categories, [true; 3]);
    assert_eq!(journey.purpose, install_profile_model::default_purpose());
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Migration);
    journey.back().unwrap();
    journey.choose("desktop", true).unwrap();
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
    assert_eq!(journey.history, vec![Page::Destination]);
}
