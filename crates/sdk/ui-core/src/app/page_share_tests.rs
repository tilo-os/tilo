//! A window's page on its way to the session: each window of a process sends its own page on its
//! own connection, a payload too large for one message goes in parts the connection has room
//! for, and what an older window sends is still read.

use super::*;
use crate::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
};
use crate::hosted_window::HostedWindow;
use crate::webview::paint::{self, SharedFrame};
use crate::widget::Column;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

/// One window of a process: its app, the window its events run in, and the shell's end of its
/// connection.
struct Opened {
    app: App,
    window: HostedWindow,
    shell: UnixStream,
    unread: Vec<u8>,
    /// The longest message the shell read, in bytes.
    longest: usize,
}

impl Opened {
    fn window() -> Self {
        let (app_side, shell) = UnixStream::pair().unwrap();
        shell.set_nonblocking(true).unwrap();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Column::new()));
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 600, 400, 1.0, false, None).unwrap();
        draw_frame_content(&mut app, &mut Scene::new(), 600, 400, 1.0, None);
        *app.floating = crate::app::FloatingRuntime::new(Some(
            FloatingSurfaceServiceClient::from_stream(app_side).unwrap(),
        ));
        Self {
            app,
            window: HostedWindow::new(),
            shell,
            unread: Vec::new(),
            longest: 0,
        }
    }

    /// Run `act` inside this window's events, as the process's loop runs each window.
    fn within<T>(&mut self, act: impl FnOnce(&mut App) -> T) -> T {
        let _events = crate::window_toolbar::enter(Some(&self.window));
        act(&mut self.app)
    }

    /// One turn of the window's loop: what the shell said is taken, and what the window has to
    /// say is sent as far as the connection takes it.
    fn turn(&mut self) {
        self.within(|app| {
            app.pump_floating_surface_service();
        });
    }

    /// The shell says `message` to the window.
    fn told(&mut self, message: &FloatingSurfaceHostMessage) {
        let mut bytes = serde_json::to_vec(message).unwrap();
        bytes.push(b'\n');
        self.shell.set_nonblocking(false).unwrap();
        self.shell.write_all(&bytes).unwrap();
        self.shell.set_nonblocking(true).unwrap();
        self.turn();
    }

    /// The frames of its page the window sent since the last call, as the shell reads them.
    fn frames(&mut self) -> Vec<FloatingSurfaceClientMessage> {
        let mut chunk = vec![0_u8; 256 * 1024];
        loop {
            match self.shell.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.unread.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("the window's connection failed: {error}"),
            }
        }
        let mut frames = Vec::new();
        while let Some(end) = self.unread.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.unread.drain(..=end).collect();
            self.longest = self.longest.max(line.len());
            let message: FloatingSurfaceClientMessage =
                serde_json::from_slice(&line[..end]).expect("a message the shell reads");
            if matches!(message, FloatingSurfaceClientMessage::PaintStream { .. }) {
                frames.push(message);
            }
        }
        frames
    }
}

fn frame(payload: &[u8]) -> SharedFrame {
    SharedFrame::whole(payload.to_vec(), [0.0, 0.0, 600.0, 400.0], 1.0)
}

fn decoded(text: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .expect("base64")
}

/// Two windows of one process. The session asks the first for its page: that window is watched
/// and the other is not, the first window's frames go out on its connection alone, and telling
/// the second that nobody watches it stops nothing of the first.
#[cfg(unix)]
#[test]
#[serial_test::serial(paint_share)]
fn each_window_sends_its_own_page_on_its_own_connection() {
    let (mut first, mut second) = (Opened::window(), Opened::window());
    first.told(&FloatingSurfaceHostMessage::PaintKeyframe);
    assert!(first.within(|_| paint::share_watched()));
    assert!(
        !second.within(|_| paint::share_watched()),
        "the session asked the first window, not the second"
    );
    assert!(!paint::share_watched());

    // Each window's view queues what its engine captured; only a watched window holds it.
    first.within(|_| paint::queue_share(frame(b"the first window's page")));
    second.within(|_| paint::queue_share(frame(b"the second window's page")));
    second.turn();
    first.turn();
    assert!(
        second.frames().is_empty(),
        "nothing of any page goes out on the second window's connection"
    );
    let sent = first.frames();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let FloatingSurfaceClientMessage::PaintStream { payload, part, .. } = &sent[0] else {
        unreachable!("only frames are kept");
    };
    assert_eq!(decoded(payload), b"the first window's page");
    assert_eq!(*part, None, "a payload one message carries goes whole");

    second.told(&FloatingSurfaceHostMessage::PaintStop);
    assert!(
        first.within(|_| paint::share_watched()),
        "the second window's stop stops nothing of the first"
    );
    first.told(&FloatingSurfaceHostMessage::PaintStop);
    assert!(!first.within(|_| paint::share_watched()));
}

/// A payload of megabytes goes in parts, in order, each within what one message carries, and
/// the window sends only what its connection has room for: a shell that reads slowly makes the
/// page wait, and never breaks the connection every other ask of the window travels. A payload
/// larger than a share hands over is said by its first part alone.
#[cfg(unix)]
#[test]
#[serial_test::serial(paint_share)]
fn a_page_of_megabytes_goes_in_parts_the_connection_has_room_for() {
    let mut window = Opened::window();
    window.told(&FloatingSurfaceHostMessage::PaintKeyframe);
    let large: Vec<u8> = (0..3 * 1024 * 1024_usize + 12_345)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    let larger: Vec<u8> = (0..3 * 1024 * 1024_usize)
        .map(|at| u8::try_from(at % 241).unwrap())
        .collect();
    window.within(|_| {
        paint::queue_share(frame(&large));
        paint::queue_share(frame(&larger));
        paint::queue_share(frame(b"a small change after them"));
    });

    // The shell reads nothing yet: the window sends what its connection queues, and keeps
    // the rest.
    for _ in 0..8 {
        window.turn();
    }
    assert!(
        window.app.floating.client.is_some(),
        "more than the connection queues is no reason to lose it"
    );
    assert!(
        window.app.share_backlog_at().is_some(),
        "the window comes back for what the connection had no room for"
    );

    // The shell reads on, and the loop turns when the window asked.
    let mut sent = Vec::new();
    tilo_test_env::wait_until("the whole page is sent", || {
        sent.extend(window.frames());
        window.turn();
        sent.extend(window.frames());
        window.app.share_backlog_at().is_none()
            && window.app.floating.share_sending.is_none()
            && sent.len() >= 11
    });
    assert!(
        window.longest <= 1024 * 1024,
        "every message is within what the connection carries: the longest had {} bytes",
        window.longest
    );
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    let mut joining: Option<(u64, Vec<u8>)> = None;
    for message in &sent {
        let FloatingSurfaceClientMessage::PaintStream { payload, part, .. } = message else {
            unreachable!("only frames are kept");
        };
        match part {
            None => {
                assert!(joining.is_none(), "a payload's parts follow each other");
                payloads.push(decoded(payload));
            }
            Some(part) => {
                assert!(payload.is_empty(), "a part travels in its own field");
                let bytes = decoded(&part.bytes);
                assert!(bytes.len() <= paint::MAX_SHARE_PART);
                let (of, mut held) = joining.take().unwrap_or((part.of, Vec::new()));
                assert_eq!(part.of, of);
                assert_eq!(part.at, held.len() as u64, "the parts come in order");
                held.extend_from_slice(&bytes);
                if held.len() as u64 == of {
                    payloads.push(held);
                } else {
                    joining = Some((of, held));
                }
            }
        }
    }
    assert!(joining.is_none(), "every payload came whole");
    assert_eq!(payloads.len(), 3);
    assert!(
        payloads[0] == large,
        "the first payload is joined as it was cut"
    );
    assert!(payloads[1] == larger);
    assert_eq!(payloads[2], b"a small change after them");

    // A whole frame larger than a share hands over: its size and its start, and no more.
    let of = paint::MAX_SHARE_PAYLOAD + 1;
    window.within(|_| {
        paint::queue_share(SharedFrame {
            payload: vec![0; paint::MAX_SHARE_PART],
            of,
            page: [0.0; 4],
            scale: 1.0,
        });
    });
    let mut said = Vec::new();
    tilo_test_env::wait_until("the page too large is said", || {
        window.turn();
        said.extend(window.frames());
        !said.is_empty() && window.app.floating.share_sending.is_none()
    });
    assert_eq!(said.len(), 1, "{} messages", said.len());
    let FloatingSurfaceClientMessage::PaintStream {
        part: Some(part), ..
    } = &said[0]
    else {
        panic!("a payload too large is said by a part");
    };
    assert_eq!((part.at, part.of), (0, of as u64));
    window.told(&FloatingSurfaceHostMessage::PaintStop);
}

/// A window withheld from captures hands no page over, also not the rest of a payload it was
/// sending.
#[cfg(unix)]
#[test]
#[serial_test::serial(paint_share)]
fn a_window_withheld_from_captures_sends_no_part_of_its_page() {
    let mut window = Opened::window();
    window.told(&FloatingSurfaceHostMessage::PaintKeyframe);
    let large = vec![7_u8; 6 * 1024 * 1024];
    window.within(|_| paint::queue_share(frame(&large)));
    for _ in 0..4 {
        window.turn();
    }
    assert!(
        window.app.floating.share_sending.is_some(),
        "the payload is half sent: the shell read nothing"
    );
    window.within(|app| app.set_capture_protected(true));
    for _ in 0..4 {
        window.turn();
    }
    assert!(window.app.floating.share_sending.is_none());
    assert!(window.app.share_backlog_at().is_none());
    assert!(window.within(|_| paint::take_share_frames()).is_empty());
    // What the connection had queued before goes out as the shell reads; nothing follows it.
    let mut parts = window.frames();
    tilo_test_env::wait_until("what was queued before is read", || {
        window.turn();
        let more = window.frames();
        let quiet = more.is_empty();
        parts.extend(more);
        quiet
    });
    assert!(
        parts.len() < large.len().div_ceil(paint::MAX_SHARE_PART),
        "the payload was never completed: {} of its parts went out",
        parts.len()
    );
    window.within(|_| paint::set_share_watched(false));
}

/// What a window built before frames named their window sends is still read, and a payload one
/// message carries is written as it always was, with no field an older shell does not know.
#[test]
fn a_window_built_before_frames_named_their_window_is_read_as_before() {
    let offer: FloatingSurfaceClientMessage =
        serde_json::from_str(r#"{"type":"paint_capable"}"#).unwrap();
    assert_eq!(offer, FloatingSurfaceClientMessage::PaintCapable);
    let older: FloatingSurfaceClientMessage = serde_json::from_str(
        r#"{"type":"paint_stream","payload":"AAEC","page":[0.0,0.0,600.0,400.0],"scale":1.0}"#,
    )
    .unwrap();
    let whole = FloatingSurfaceClientMessage::PaintStream {
        payload: "AAEC".into(),
        page: [0.0, 0.0, 600.0, 400.0],
        scale: 1.0,
        instance: None,
        part: None,
    };
    assert_eq!(older, whole);
    let written = serde_json::to_value(&whole).unwrap();
    let mut fields: Vec<&str> = written
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(fields, ["page", "payload", "scale", "type"]);
}
