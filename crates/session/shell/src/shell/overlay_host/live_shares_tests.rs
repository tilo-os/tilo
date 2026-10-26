//! Windows shared live into a conversation, as the shell serves them to apps
//! in processes of their own: a window on a record gets "Share to channel…"
//! in its menu, what the person chooses there reaches the sharing app (once
//! it connects, when it has not yet), and the views the sharing app embeds
//! report how they are coming along.

use automerge::{AutoCommit, ROOT};

use crate::shell::frame::FrameBuilder;
use crate::shell::state::TileContent;
use ui::floating_surface::{
    FloatingSurfaceClientMessage as Message, FloatingSurfaceHostMessage as HostMessage,
};
use ui::window_share::host::{SHARE_ITEM, STOP_ITEM};
use ui::window_share::{Ask, DeclaredEmbed, EmbedState, ResourceView, SHARING_APP, SharedWindow};
use ui::window_toolbar::ToolbarEvent;

const APP: &str = "org.example.Notebook";
const APP_CONNECTION: u64 = 3;
const CHAT_CONNECTION: u64 = 7;

/// The focused tab shows `app`'s window, surface `surface_id`, as an app in
/// its own process maps it there. Returns the tab.
fn focus_window_of(frame: &mut FrameBuilder, app: &str, surface_id: u64) -> u64 {
    let focused =
        crate::shell::window_actions::active_tab_id(&frame.chrome()).expect("a focused tab");
    frame.with_chrome(|st| {
        let mut tree = st.tree_mut();
        for tab in tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            if tab.id == focused {
                tab.tile = TileContent::Foreign {
                    app_id: app.into(),
                    surface_id,
                };
            }
        }
        drop(tree);
        st.bump();
    });
    focused
}

fn settle(frame: &mut FrameBuilder) {
    for _ in 0..3 {
        let mut doc = AutoCommit::new();
        frame.pump_apps(&mut doc, &ROOT);
        frame.relayout();
        let _ = frame.paint();
    }
}

/// A notebook window in focus that said which record it shows.
fn notebook_on_a_record() -> (FrameBuilder, u64) {
    let mut frame = FrameBuilder::new((1600, 900));
    let tab = focus_window_of(&mut frame, APP, 9_101);
    frame.host_mut().connect_window_share(APP_CONNECTION, APP);
    let named = frame.host_mut().receive_window_share_message(
        APP_CONNECTION,
        APP,
        &Message::WindowRecord {
            record: "rec-notes".into(),
            kind: "note".into(),
            instance: None,
        },
    );
    assert_eq!(named, Some(true), "the window is anchored to its tab");
    settle(&mut frame);
    (frame, tab)
}

fn asks_to(outbox: &[(u64, HostMessage)], connection: u64) -> Vec<Ask> {
    outbox
        .iter()
        .filter(|(to, _)| *to == connection)
        .filter_map(|(_, message)| match message {
            HostMessage::WindowShareAsk { ask } => Some(ask.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn sharing_a_window_from_its_menu_asks_the_sharing_app_once_it_connects() {
    let (mut frame, tab) = notebook_on_a_record();
    let outbox = frame.host_mut().take_window_share_outbox();
    assert!(
        outbox
            .iter()
            .any(|(to, message)| *to == APP_CONNECTION
                && matches!(message, HostMessage::DrawsEmbeds)),
        "every app hears this shell draws embeds: {outbox:?}"
    );

    // The person chooses "Share to channel…" in the window's menu, before
    // the sharing app runs.
    assert!(frame.host_mut().window_toolbars.route(
        tab,
        ToolbarEvent::Action {
            id: SHARE_ITEM.into()
        }
    ));
    settle(&mut frame);
    assert!(
        asks_to(&frame.host_mut().take_window_share_outbox(), APP_CONNECTION).is_empty(),
        "the ask never goes to the shared window's own app"
    );

    // The sharing app connects: it hears the windows and the waiting ask.
    frame
        .host_mut()
        .connect_window_share(CHAT_CONNECTION, SHARING_APP);
    let outbox = frame.host_mut().take_window_share_outbox();
    let windows = outbox
        .iter()
        .find_map(|(to, message)| match message {
            HostMessage::OpenWindows { windows } if *to == CHAT_CONNECTION => Some(windows),
            _ => None,
        })
        .expect("the sharing app hears the windows");
    assert_eq!(windows.len(), 1, "{windows:?}");
    assert_eq!((windows[0].tab, windows[0].app.as_str()), (tab, APP));
    assert_eq!(windows[0].record, "rec-notes");
    let asks = asks_to(&outbox, CHAT_CONNECTION);
    assert!(
        matches!(asks.as_slice(), [Ask::Share(window)] if window.tab == tab && window.record == "rec-notes"),
        "{asks:?}"
    );
    drop(frame);
}

#[test]
fn a_shared_window_offers_to_stop_sharing_it() {
    let (mut frame, tab) = notebook_on_a_record();
    frame
        .host_mut()
        .connect_window_share(CHAT_CONNECTION, SHARING_APP);
    // Only the sharing app says what is shared.
    let shared = vec![SharedWindow {
        message: "msg-1".into(),
        app: APP.into(),
        record: "rec-notes".into(),
        place: "#general".into(),
    }];
    assert_eq!(
        frame.host_mut().receive_window_share_message(
            APP_CONNECTION,
            APP,
            &Message::SharedWindows {
                shared: shared.clone()
            },
        ),
        Some(false)
    );
    assert_eq!(
        frame.host_mut().receive_window_share_message(
            CHAT_CONNECTION,
            SHARING_APP,
            &Message::SharedWindows { shared },
        ),
        Some(true)
    );
    settle(&mut frame);
    let _ = frame.host_mut().take_window_share_outbox();

    assert!(frame.host_mut().window_toolbars.route(
        tab,
        ToolbarEvent::Action {
            id: STOP_ITEM.into()
        }
    ));
    settle(&mut frame);
    assert_eq!(
        asks_to(
            &frame.host_mut().take_window_share_outbox(),
            CHAT_CONNECTION
        ),
        vec![Ask::Stop {
            message: "msg-1".into()
        }]
    );
    drop(frame);
}

#[test]
fn a_closed_connection_takes_its_window_out_of_the_list() {
    let (mut frame, _) = notebook_on_a_record();
    frame
        .host_mut()
        .connect_window_share(CHAT_CONNECTION, SHARING_APP);
    let _ = frame.host_mut().take_window_share_outbox();
    frame.host_mut().forget_window_share(APP_CONNECTION);
    settle(&mut frame);
    let outbox = frame.host_mut().take_window_share_outbox();
    assert!(
        outbox.iter().any(|(to, message)| *to == CHAT_CONNECTION
            && matches!(message, HostMessage::OpenWindows { windows } if windows.is_empty())),
        "{outbox:?}"
    );
    drop(frame);
}

#[test]
fn only_the_sharing_app_embeds_views_and_hears_how_they_come_along() {
    let mut frame = FrameBuilder::new((1600, 900));
    focus_window_of(&mut frame, SHARING_APP, 9_102);
    frame
        .host_mut()
        .connect_window_share(CHAT_CONNECTION, SHARING_APP);
    let embeds = Message::Embeds {
        embeds: vec![DeclaredEmbed {
            key: "msg-1".into(),
            view: ResourceView {
                app: "org.example.NotInstalled".into(),
                record: "rec-notes".into(),
                resource: "note-1".into(),
            },
            rect: [10.0, 10.0, 410.0, 260.0],
            clip: [0.0, 0.0, 800.0, 600.0],
        }],
        instance: None,
    };
    assert_eq!(
        frame
            .host_mut()
            .receive_window_share_message(APP_CONNECTION, APP, &embeds),
        Some(false),
        "another app embeds nothing"
    );
    assert_eq!(
        frame
            .host_mut()
            .receive_window_share_message(CHAT_CONNECTION, SHARING_APP, &embeds),
        Some(true)
    );
    settle(&mut frame);
    let states: Vec<_> = frame
        .host_mut()
        .take_window_share_outbox()
        .into_iter()
        .filter_map(|(to, message)| match message {
            HostMessage::EmbedStates { states } if to == CHAT_CONNECTION => Some(states),
            _ => None,
        })
        .flatten()
        .collect();
    // An app this computer does not have shows no view, and says so.
    assert!(
        matches!(
            states.as_slice(),
            [state] if state.key == "msg-1" && matches!(state.state, EmbedState::Unavailable(_))
        ),
        "{states:?}"
    );
    drop(frame);
}
