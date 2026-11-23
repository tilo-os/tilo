//! Chat in a hosted window: the bar's New channel, run by the window as its own action — an
//! address on the app's root and no argument — makes a channel the app's own read holds.

use super::*;

const CHAT: &str = "so.tilo.lang.chat";

/// Chat's instance in a workspace of its own.
fn chatting() -> (TilolangProcess, Window, Sent, Inbox, RouteSpec) {
    running_app("ada", CHAT, shipped(&shipped_dir().join("chat")).1)
}

/// The runtime the hosted instance runs over, as a handle of its own.
fn runtime(host: &TilolangProcess) -> tilolang_tilo::host::Host {
    host.instances()
        .next()
        .expect("the instance runs")
        .instance()
        .host()
        .expect("the runtime")
}

/// The channels the app's own read holds.
fn channels_of(runtime: &tilolang_tilo::host::Host) -> Vec<Value> {
    let root = runtime.root_of(CHAT).expect("the chat root");
    match runtime
        .runtime()
        .read(&root, "channels")
        .expect("the channels")
    {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

#[test]
fn the_bars_new_channel_makes_one() {
    let (mut host, mut window, _sent, _inbox, _route) = chatting();
    settle(&mut host, &mut window);

    // What the window does on the bar's New channel: the app's address and no argument. Every
    // argument the action takes has a default, so nothing is asked and the channel is made.
    let address = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New channel")
        .expect("the app publishes the action that makes a channel")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle(&mut host, &mut window);
    assert!(
        window.form.is_none(),
        "nothing is asked of the person: {:?}",
        window.form.as_ref().map(|form| form.title.clone())
    );
    let chat = runtime(&host);
    let channels = channels_of(&chat);
    assert_eq!(
        channels.len(),
        1,
        "the bar made a channel: {channels:?}; the app said {:?}",
        window.notices
    );
    let Value::Ref(channel) = &channels[0] else {
        panic!("a channel is a resource: {:?}", channels[0]);
    };
    assert_eq!(
        chat.runtime()
            .read(&channel.target, "name")
            .expect("its name"),
        Value::text("new-channel"),
        "the channel is named by the words the action defaults to"
    );
}
