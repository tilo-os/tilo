//! A window's tag names it to the session only when the session handed its client the connection
//! itself, as it hands one to an app keeper: a tag is the client's own text.

use super::*;

/// The facts of the window of `app_id`, once it is mapped.
fn facts(compositor: &Compositor, app_id: &str) -> Option<WindowInfo> {
    compositor
        .windows()
        .into_iter()
        .map(|(info, _)| info)
        .find(|info| info.app_id.as_deref() == Some(app_id))
}

/// Open a window of `app_id` on `client` that names itself `tag` before it first commits.
fn open_tagged(compositor: &mut Compositor, client: &mut TestClient, app_id: &str, tag: &str) {
    pump(compositor, &mut [&mut *client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.tag_manager.is_some()
    });
    client.create_window(app_id);
    let tags = client.seen.tag_manager.clone().expect("the tag manager");
    tags.set_toplevel_tag(client.toplevel.as_ref().expect("toplevel"), tag.into());
    client.commit();
    pump(compositor, &mut [&mut *client], |_, clients| {
        clients[0].seen.configured
    });
    client.show(120, 90, [40, 80, 120, 255]);
    pump(compositor, &mut [&mut *client], |compositor, _| {
        facts(compositor, app_id).is_some()
    });
}

#[test]
fn only_a_connection_the_session_handed_over_names_its_windows_by_tag() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut kept = TestClient::connect_for(&compositor, std::process::id());
    open_tagged(&mut compositor, &mut kept, "org.example.Kept", "w1");
    assert_eq!(
        facts(&compositor, "org.example.Kept").and_then(|info| info.tag),
        Some("w1".to_owned()),
        "a keeper's window is known by its tag"
    );
    let mut other = TestClient::connect(&mut compositor);
    open_tagged(&mut compositor, &mut other, "org.example.Other", "w1");
    assert_eq!(
        facts(&compositor, "org.example.Other").map(|info| info.tag),
        Some(None),
        "any other client's tag names nothing"
    );
}
