//! Two windows of one app on one person's desktop, run by the one host of their session, each on
//! a connection of its own to the same workspace: a shared desktop where two people opened Chat
//! at once shows both people's windows on each desktop. What another person's device makes
//! reaches both windows, and each opens it.

use super::*;
use tilolang_process::testing::Bus;

const CHANNEL: &str = "new-channel";

impl Session {
    /// One more window of the session's app in the session's host, for its person, on a
    /// connection of its own on `bus` to `route`: what the host does when the session hands it a
    /// second window of an app it runs.
    fn another_window(&mut self, bus: &Rc<Bus>, route: &RouteSpec) -> Window {
        let spec = spec_of(route, &self.person);
        let broker = Broker::on(bus, route.clone(), &spec.person);
        let mut manager = CollaborationManager::with_peer_transport(broker);
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: None,
            resource_type: None,
            handle: Some(spec.person.clone()),
        }));
        let early = manager.poll();
        let connection = tilolang_process::connection::Connection::new(manager)
            .expect("the connection joins its route");
        let store = tilolang_tilo::resources::ResourceStore::new(Rc::new(connection.port(None)));
        let (ours, theirs) = Channel::pair().expect("a patch connection");
        let watched = std::os::fd::AsFd::as_fd(&ours)
            .try_clone_to_owned()
            .expect("the window's connection is watched");
        let bundle = package(package_of(&spec.app));
        let mut hosted = Hosted::new(
            spec,
            connection,
            (store, tilolang_process::hosted::memory_content()),
            bundle,
            &tilolang_process::hosted::Kept::default(),
            Box::new(ChannelTransport::new(ours).expect("the window's transport")),
            (&Rc::new(Phrases::new("en-US")), self.host.formats()),
            self.woken.waker(),
        );
        hosted.arrived_early(early);
        self.host.add(hosted, Some(watched));
        Window::in_harness(HostedWindow::over(theirs))
    }
    /// The host's frames and both windows', until the host owes nothing.
    fn settle_with(&mut self, other: &mut Window) {
        settle::settle(&mut self.host, &self.woken, || {
            let mine = self.window.frame();
            other.frame() | mine
        });
    }
}

/// The texts `window` shows: its overview, and the page the content level opens.
fn shown_in(window: &mut Window) -> Vec<String> {
    let hosted = window
        .root()
        .as_any()
        .and_then(|any| any.downcast_ref::<HostedWindow>())
        .expect("the window is the hosted window");
    let view = hosted.view().expect("the window is connected");
    let mut out = Vec::new();
    for tree in [view.root_tree(), view.detail_tree()].into_iter().flatten() {
        texts(&tree, &mut out);
    }
    out
}

/// Where `window` draws the control named `label`, if it does.
fn drawn_at(window: &mut Window, label: &str) -> Option<(f64, f64)> {
    let mut found = Vec::new();
    named(window.root(), label, &mut found);
    found.last().map(|rect| (rect.center().x, rect.center().y))
}

/// Bo's session runs his own Chat window and the one Ada opened on their shared desktop; Ada's
/// device makes a channel. Both of Bo's windows list it, and each opens it on a press.
#[test]
fn two_windows_of_chat_both_open_a_channel_another_device_made() {
    let bus = Rc::new(Bus::default());
    bus.member(ui::tilo_actions::MemberId::person("ada"), "Ada");
    bus.member(ui::tilo_actions::MemberId::person("bo"), "Bo");
    let route = RouteSpec::app(CHAT, uuid::Uuid::new_v4());
    let mut ada = Session::on_bus(&bus, &route, spec_of(&route, "ada"));
    let mut bo = Session::on_bus(&bus, &route, spec_of(&route, "bo"));
    let mut second = bo.another_window(&bus, &route);
    bo.settle_with(&mut second);
    assert_eq!(
        bo.host.instances().count(),
        2,
        "Bo's host runs both windows"
    );

    let new_channel = ada.identity_doing("Conversations", "new_channel");
    let made = ada.invoke_doing(&new_channel, &serde_json::json!({ "name": CHANNEL }));
    assert!(
        matches!(made, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the channel was made: {made:?}"
    );
    ada.settle();
    bo.settle_with(&mut second);

    // Listed in the sidebar, where a press opens it: the window's "since you looked" names it
    // too, which is not the list.
    for (which, window) in [("his own", &mut bo.window), ("Ada's", &mut second)] {
        assert!(
            drawn_at(window, CHANNEL).is_some(),
            "{which} window on Bo's desktop lists the channel Ada made: {:?}",
            shown_in(window)
        );
    }
    let composer = format!("Message #{CHANNEL}");
    for which in ["his own", "Ada's"] {
        let window = if which == "Ada's" {
            &mut second
        } else {
            &mut bo.window
        };
        let (x, y) = drawn_at(window, CHANNEL).expect("the window lists the channel");
        window.click_at(x, y);
        bo.settle_with(&mut second);
        let window = if which == "Ada's" {
            &mut second
        } else {
            &mut bo.window
        };
        assert!(
            shown_in(window).contains(&composer),
            "a press on the channel in {which} window opens it: {:?}",
            shown_in(window)
        );
    }
}
