use super::model::{Destination, Journey, Page};

#[test]
fn desktop_edits_never_restore_os_questions() {
    let mut journey = Journey::new();
    journey.choose("desktop", true).unwrap();
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
    assert_eq!(journey.history, vec![Page::Destination]);
    assert_eq!(journey.destination, Some(Destination::Desktop));
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
