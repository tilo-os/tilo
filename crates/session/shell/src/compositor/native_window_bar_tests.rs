//! An app running in its own process is searched and its History opened
//! from its window's bar over the real `shell-ui` connection: the app says
//! what its window offers, the shell's service reads it off the
//! launcher-authenticated socket and hands it to the window, and what the
//! person does in the bar goes back to the app on the same connection.

use std::io::Write as _;
use std::os::unix::net::UnixStream;

use crate::compositor::floating_service::{FloatingServiceEvent, FloatingSurfaceService};
use crate::compositor::frame::FrameBuilder;
use crate::shell::state::TileContent;
use ui::floating_surface::{
    FLOATING_SURFACE_SERVICE, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
    FloatingSurfaceServiceClient,
};
use ui::window_toolbar::{HistoryOffer, SearchCount, SearchOffer, ToolbarEvent, WindowOffer};

const APP: &str = "org.example.Notebook";

/// The focused tab shows `APP`, as the window an app in its own process
/// maps there does. Returns its strip and tab.
fn focus_app_window(frame: &mut FrameBuilder) -> (crate::shell::state::StripId, u64) {
    let focused =
        crate::shell::window_actions::active_tab_id(&frame.chrome()).expect("a focused tab");
    let mut strip_id = None;
    frame.with_chrome(|st| {
        let mut tree = st.tree_mut();
        for strip in tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
        {
            for tab in strip
                .window_groups
                .iter_mut()
                .flat_map(|group| &mut group.tabs)
            {
                if tab.id == focused {
                    tab.tile = TileContent::Foreign {
                        app_id: APP.into(),
                        surface_id: 9_002,
                    };
                    strip_id = Some(strip.id);
                }
            }
        }
    });
    (strip_id.expect("the focused tab's strip"), focused)
}

fn connect(service: &FloatingSurfaceService) -> (FloatingSurfaceServiceClient, u64) {
    // The launcher authenticates the app's connection before the app runs.
    let mut socket = UnixStream::connect(launcher::services::socket_for(FLOATING_SURFACE_SERVICE))
        .expect("connect");
    let mut handshake = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::SHELL_UI_LAUNCH_PROTOCOL,
        "app_id": APP,
    }))
    .unwrap();
    handshake.push(b'\n');
    socket.write_all(&handshake).unwrap();
    let app = FloatingSurfaceServiceClient::from_test_stream(socket).expect("app client");
    let mut connection = None;
    tilo_test_env::wait_until("the app connects", || {
        connection = service.drain().into_iter().find_map(|event| match event {
            FloatingServiceEvent::Connected { connection, .. } => Some(connection),
            _ => None,
        });
        connection.is_some()
    });
    (app, connection.expect("a connection"))
}

/// The window that shows tab `tab` of `strip`.
fn window_of(
    frame: &mut FrameBuilder,
    strip: crate::shell::state::StripId,
    tab: u64,
) -> &mut ui::widget::WindowGroup {
    let index = (0..64)
        .find(|index| {
            frame
                .host_mut()
                .group_at_for_tests(strip, *index)
                .is_some_and(|group| group.tabs().iter().any(|t| t.id == tab))
        })
        .expect("the app's window");
    frame
        .host_mut()
        .group_at_for_tests(strip, index)
        .expect("the app's window")
}

/// The next message the shell reads from the app, with the app id the
/// launcher vouched for.
fn receive(service: &FloatingSurfaceService) -> (String, FloatingSurfaceClientMessage) {
    let mut received = None;
    tilo_test_env::wait_until("the shell reads the app's message", || {
        received = service.drain().into_iter().find_map(|event| match event {
            FloatingServiceEvent::Message {
                app_id, message, ..
            } => Some((app_id, message)),
            _ => None,
        });
        received.is_some()
    });
    received.expect("a message")
}

/// What the app reads from the shell next.
fn app_reads(app: &mut FloatingSurfaceServiceClient) -> Vec<FloatingSurfaceHostMessage> {
    let mut read = Vec::new();
    tilo_test_env::wait_until("the app reads the bar's events", || {
        read.extend(app.pump().expect("the connection"));
        !read.is_empty()
    });
    read
}

// USER REQUIREMENT (2026-10-19): "search right next to the title (clicking it
// expands into an in-place search field)" and History as "a right-side panel
// per window or active tab" -- for an app that runs as its own window.
#[test]
#[serial_test::serial(process_env)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn an_app_in_its_own_process_offers_its_window_and_hears_the_bars_search_and_history() {
    let runtime = tempfile::tempdir().expect("temporary runtime");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let service = FloatingSurfaceService::start().expect("start the shell-ui service");
    let (mut app, connection) = connect(&service);

    let mut frame = FrameBuilder::new((1600, 900));
    let (strip, tab) = focus_app_window(&mut frame);
    frame.relayout();

    // The app says its window has a document to search and its History.
    let offer = WindowOffer {
        search: SearchOffer::Document,
        count: None,
        history: Some(HistoryOffer::Closed),
    };
    app.send(&FloatingSurfaceClientMessage::WindowOffer {
        offer,
        instance: None,
    })
    .expect("send the offer");
    let (app_id, message) = receive(&service);
    assert_eq!(app_id, APP, "the launcher's app id, not one the app claims");
    assert_eq!(
        frame
            .host_mut()
            .receive_window_bar_message(connection, &app_id, &message),
        Some(true),
        "the window shows what the app offers"
    );
    frame.relayout();

    // The bar's search opens on the app's document, and what it asks goes
    // to the app's connection.
    let group = window_of(&mut frame, strip, tab);
    assert_eq!(group.active_tab_id(), tab);
    group.open_search(ui::tokens::MotionSource::Pointer);
    assert!(group.window_search().is_open() && group.window_search().is_remote());
    let outbox = frame.host_mut().take_toolbar_outbox();
    assert_eq!(
        outbox,
        vec![(
            connection,
            ToolbarEvent::Search {
                query: String::new()
            }
        )]
    );
    for (to, event) in outbox {
        service.send(to, &FloatingSurfaceHostMessage::ToolbarEvent { event });
    }
    assert_eq!(
        app_reads(&mut app),
        vec![FloatingSurfaceHostMessage::ToolbarEvent {
            event: ToolbarEvent::Search {
                query: String::new()
            }
        }]
    );

    // The app's count shows in the bar.
    app.send(&FloatingSurfaceClientMessage::WindowOffer {
        offer: WindowOffer {
            count: Some(SearchCount { shown: 0, total: 0 }),
            ..offer
        },
        instance: None,
    })
    .expect("send the count");
    let (app_id, message) = receive(&service);
    frame
        .host_mut()
        .receive_window_bar_message(connection, &app_id, &message);

    // Another app's connection never reaches this window.
    assert_eq!(
        frame.host_mut().receive_window_bar_message(
            connection + 1,
            "org.example.Other",
            &FloatingSurfaceClientMessage::OpenSearch { instance: None },
        ),
        Some(false)
    );

    // The connection closing takes the offer with it: the window's search
    // ends and nothing more goes to the app.
    assert!(frame.host_mut().forget_app_toolbars(connection));
    frame.relayout();
    let group = window_of(&mut frame, strip, tab);
    assert!(!group.window_search().is_open());
    assert!(frame.host_mut().take_toolbar_outbox().is_empty());
}
