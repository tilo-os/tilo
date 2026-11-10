//! A host's channel state in its platform secret store: the store a page has.

use tilo_channels::{ChannelState as _, SecretStates};
use tilo_desktop_model::WorkspaceId;

fn states() -> (SecretStates, tilo_secrets::SecretsScope) {
    let secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    (SecretStates::new("so.tilo.collaboration.mls.test"), secrets)
}

#[test]
fn a_channel_s_peers_and_route_are_kept_in_the_secret_store() {
    let (states, _secrets) = states();
    let workspace = WorkspaceId::new();
    let channel = collab_types::ChannelId::new();
    let route = collab_types::ChannelRoute::new("notes", format!("note-{}", channel.0));
    let channel = route.channel(&workspace.to_string());

    let peers = states.peers(workspace);
    assert_eq!(peers.read(channel).unwrap(), None, "nothing kept yet");
    peers.write(channel, b"sealed peer").unwrap();
    assert_eq!(
        peers.read(channel).unwrap().as_deref(),
        Some(&b"sealed peer"[..]),
        "the state is read back from the same store"
    );
    assert_eq!(states.route(workspace, channel), None, "no route kept yet");
    states.keep_route(workspace, channel, &route);
    assert_eq!(states.route(workspace, channel), Some(route));

    peers.forget(channel).unwrap();
    assert_eq!(peers.read(channel).unwrap(), None, "the state is dropped");
    assert!(
        states.route(workspace, channel).is_some(),
        "and the route beside it stays"
    );
}

/// A route that names another channel is no route for this one: the store answers none, so the
/// channel is decided as one whose record is unknown, which withholds more, never less.
#[test]
fn a_route_kept_for_another_channel_names_none() {
    let (states, _secrets) = states();
    let workspace = WorkspaceId::new();
    let channel = collab_types::ChannelId::new();
    let other = collab_types::ChannelId::new();
    let route = collab_types::ChannelRoute::new("notes", format!("note-{}", other.0));
    states.keep_route(workspace, channel, &route);

    assert_eq!(states.route(workspace, channel), None);
    assert_eq!(
        states
            .peers(workspace)
            .read(channel)
            .expect("a store with no peers for it answers none"),
        None
    );
}

/// Each workspace's peers are its own, as they are in a directory.
#[test]
fn one_workspace_s_peers_are_not_another_s() {
    let (states, _secrets) = states();
    let (here, elsewhere) = (WorkspaceId::new(), WorkspaceId::new());
    let channel = collab_types::ChannelId::new();
    states.peers(here).write(channel, b"here").unwrap();

    assert_eq!(states.peers(elsewhere).read(channel).unwrap(), None);
}
