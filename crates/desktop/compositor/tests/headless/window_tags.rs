//! A window's tag names it to the session only when the session handed its client the connection
//! itself, as it hands one to an app keeper: a tag is the client's own text.

use super::*;

/// The facts of the window `client` mapped last.
fn facts_of_last(compositor: &Compositor) -> Option<WindowInfo> {
    compositor
        .windows()
        .into_iter()
        .map(|(info, _)| info)
        .max_by_key(|info| info.id)
}

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
    open_tagged_until(compositor, client, app_id, tag, |compositor| {
        facts(compositor, app_id).is_some()
    });
}

/// [`open_tagged`], until `mapped` holds of the compositor.
fn open_tagged_until(
    compositor: &mut Compositor,
    client: &mut TestClient,
    app_id: &str,
    tag: &str,
    mapped: impl Fn(&Compositor) -> bool,
) {
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
        mapped(compositor)
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

/// A spare window process's connection names its windows' app by what the session bound it to,
/// never by what the client says: before it is bound its windows are no app's, and once it is,
/// a window calling itself another app is still the bound app's, its tag the client's own word
/// for its instance.
#[test]
fn a_spare_connections_windows_are_the_app_the_session_bound_it_to() {
    let mut compositor = headless(Box::new(StackPolicy));
    let (mut unbound, _held) = TestClient::connect_spare(&compositor);
    open_tagged_until(
        &mut compositor,
        &mut unbound,
        "so.tilo.settings",
        "7",
        |compositor| facts_of_last(compositor).is_some(),
    );
    let first = facts_of_last(&compositor).expect("the window is listed");
    assert_eq!(first.made_for, Some(MadeFor::Spare));
    assert_eq!(
        first.app_id, None,
        "an unbound spare's window is no app's, whatever it calls itself"
    );

    let (mut bound, spare) = TestClient::connect_spare(&compositor);
    spare.started(4242);
    assert!(spare.bind("so.tilo.tasks", "workspace-a"));
    assert!(
        !spare.bind("so.tilo.settings", "workspace-a"),
        "a connection is bound once"
    );
    open_tagged_until(
        &mut compositor,
        &mut bound,
        "so.tilo.settings",
        "9",
        |compositor| facts(compositor, "so.tilo.tasks").is_some(),
    );
    let window = facts(&compositor, "so.tilo.tasks").expect("the bound window");
    assert_eq!(
        window.made_for,
        Some(MadeFor::App {
            app: "so.tilo.tasks".to_owned(),
            workspace: "workspace-a".to_owned(),
        })
    );
    assert_eq!(window.tag.as_deref(), Some("9"));
    assert_eq!(
        window.pid,
        Some(4242),
        "the process the session handed it to"
    );
    assert!(
        facts(&compositor, "so.tilo.settings").is_none(),
        "no window is the app a client claimed"
    );
}
