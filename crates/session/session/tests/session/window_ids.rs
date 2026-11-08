//! The ids the desk addresses its windows by, as a person's device mints
//! them: a tab's id is composed with the salt its own device's key folds to,
//! so on some devices it is far past what an action's integer holds. The desk
//! still sends such a window where the person sends it, and answers a call
//! that names something which is no window's id instead of moving the desk's
//! first window.

use super::*;

use iroh::SecretKey;
use tilo_desktop_model::ids::device_salt_of_key;

/// A session whose desk mints its ids under `salt`: the salt another device's
/// key folds to, as this one's does.
fn session_minting_under(salt: u16) -> Session {
    crate::common::headless(Options {
        collab: false,
        salt,
        ..Options::default()
    })
}

/// The salt a device's key folds to when the ids it mints do not fit an
/// action's integer: the top half of the id space, which is every salt from
/// 0x8000 up, so a device folding there names its windows in ids past
/// `i64::MAX`.
fn device_salt_past_an_integer() -> u16 {
    (0..=u8::MAX)
        .find_map(|seed| {
            let key = SecretKey::from_bytes(&[seed; 32]).public();
            let salt = device_salt_of_key(key.as_bytes());
            (salt >= 0x8000).then_some(salt)
        })
        .expect("a device whose ids do not fit an action's integer")
}

/// A window whose id is past an action's integer is sent to another strip and
/// another workspace all the same, and a call naming no window's id is
/// answered rather than taken for the desk's first window.
#[test]
fn the_desktop_sends_a_window_whose_id_is_past_an_integer() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_minting_under(device_salt_past_an_integer());
    let _client = mapped(&mut session, "org.example.Editor");
    let tab = tab_of(&session, "org.example.Editor");
    assert!(
        tab > u64::try_from(i64::MAX).expect("an i64 is a u64"),
        "the window's id is the device's own, past an action's integer: {tab}"
    );

    add_strip(&session, "Second");
    let second = strip_id(&session, "Second");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-tab",
        &serde_json::json!({"tab": tab.to_string(), "to": {"strip": second}}),
    );
    let holding = strips_holding(&session, tab);
    assert_eq!(
        holding.len(),
        1,
        "the window stands on one strip: {holding:?}"
    );
    assert!(
        holding[0].starts_with("Second/"),
        "the window went to the strip the person chose: {holding:?}"
    );

    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let other = active_workspace(&session).expect("the new workspace is open");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-tab",
        &serde_json::json!({"tab": tab.to_string(), "to": {"workspace": other.to_string()}}),
    );
    assert_eq!(
        tabs_in(&session, other),
        [tab],
        "the same window is in the workspace it was sent to"
    );

    // What no window answers to is refused: neither a tab's id spelled as
    // something else, nor a call that names none.
    for arguments in [
        serde_json::json!({"tab": "not a tab", "to": {"workspace": other.to_string()}}),
        serde_json::json!({"to": {"workspace": other.to_string()}}),
        serde_json::json!({"tab": serde_json::json!(tab), "to": {"workspace": other.to_string()}}),
    ] {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let refused = session.invoke(CHROME_RESOURCE, "move-tab", &arguments);
        assert!(
            matches!(refused, Err(tilo_actions::DoorError::InvalidArguments(_))),
            "a call naming no window's id is answered, not taken for the first window: {arguments:?} {refused:?}"
        );
    }
}
