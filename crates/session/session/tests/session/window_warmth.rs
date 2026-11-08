//! Every app window takes the temperature of the workspace it stands in and the colour of the
//! person at this machine, and is told again when either changes.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_app_launch::Connects as _;
use tilo_desktop_model::ids::WorkspaceSlot;
use ui::WindowAppearance;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};

use super::*;

/// The person asks the workspace `workspace` to wear `degrees` whatever its picture gives it.
fn warmed(workspace: WorkspaceSlot, degrees: f64) {
    settings_store::update_section(
        "appearance",
        serde_json::Map::from_iter([(
            "workspace_temperatures".to_owned(),
            serde_json::Value::Object(serde_json::Map::from_iter([(
                workspace.0.to_string(),
                serde_json::json!(degrees),
            )])),
        )]),
    )
    .expect("the temperature is kept");
}

/// A window's own shell-ui connection: the end it writes its frames on, and the end it reads the
/// session's from, with what of a line it read so far.
pub(super) struct Window {
    to_session: UnixStream,
    from_session: UnixStream,
    unread: Vec<u8>,
}

impl Window {
    /// The connection of a window of `app` , run by the launch `pid` .
    pub(super) fn open(
        launches: &Arc<tilo_session::Launches>,
        app: &str,
        workspace: &str,
        pid: u32,
    ) -> Self {
        let stream: UnixStream =
            tilo_session::mount::app_tabs::ServiceLinks::new(Arc::clone(launches))
                .window(app, workspace, 7, Some(pid))
                .shell_ui
                .expect("the app's shell-ui connection")
                .into();
        let from_session = stream.try_clone().expect("a reading end");
        from_session
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("a timeout on reading");
        Self {
            to_session: stream,
            from_session,
            unread: Vec::new(),
        }
    }

    /// Say the version this window speaks, as every app does once.
    pub(super) fn hello(&mut self) {
        self.say(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        });
    }

    /// Write `message` to the session, as this window says something.
    pub(super) fn say(&mut self, message: &FloatingSurfaceClientMessage) {
        let mut frame = serde_json::to_vec(message).expect("a message");
        frame.push(b'\n');
        self.to_session.write_all(&frame).expect("the message");
        self.to_session.flush().expect("the message");
    }

    /// The next message the session wrote, or `None` while it wrote none.
    pub(super) fn next(&mut self) -> Option<FloatingSurfaceHostMessage> {
        loop {
            if let Some(at) = self.unread.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.unread.drain(..=at).collect();
                let text = std::str::from_utf8(&line[..line.len() - 1]).expect("a message");
                return Some(serde_json::from_str(text.trim()).expect("a shell-ui message"));
            }
            let mut frame = [0_u8; 4096];
            match self.from_session.read(&mut frame) {
                Ok(0) => panic!("the session closed the window's connection"),
                Ok(read) => self.unread.extend_from_slice(&frame[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return None;
                }
                Err(error) => panic!("reading the session's message: {error}"),
            }
        }
    }
}

/// Step the session until the window is told something, or it stays silent for `steps` frames.
pub(super) fn told(
    window: &mut Window,
    session: &mut Session,
    steps: usize,
) -> Option<FloatingSurfaceHostMessage> {
    for _ in 0..steps {
        session.step(Duration::from_millis(2)).expect("step");
        if let Some(message) = window.next() {
            return Some(message);
        }
    }
    None
}

/// What an appearance message says.
#[track_caller]
fn appearance(message: &FloatingSurfaceHostMessage) -> WindowAppearance {
    message
        .window_appearance()
        .unwrap_or_else(|| panic!("an appearance, not {message:?}"))
}

/// A window opens to the temperature of the workspace it stands in and the colour of the person at
/// this machine, takes another temperature when the workspace's changes, and is told nothing while
/// neither does.
#[test]
fn a_window_takes_the_temperature_of_its_workspace_and_the_person_s_colour() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let launches = Arc::new(tilo_session::Launches::default());
    let mut session = crate::common::headless(Options {
        collab: false,
        launches: Arc::clone(&launches),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");

    // The app runs in its own workspace, and its window stands in the one in front.
    let mut app_process = std::process::Command::new("sleep")
        .arg("120")
        .spawn_locked()
        .expect("a launched app's process");
    let app = "org.example.Notes";
    launches.started(app_process.id(), app, "private", false);
    let _client = mapped(&mut session, app);
    let workspace = session.handle().read(|tree| tree.active);
    // The person gives the workspace a warm light, and goes to another one,
    // where every workspace's temperature is looked at again.
    warmed(workspace, 40.0);
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");

    let mut window = Window::open(&launches, app, "private", app_process.id());
    window.hello();
    let opened = appearance(
        &told(&mut window, &mut session, 200).expect("the window opens to its appearance"),
    );
    let warm = opened
        .workspace_hue
        .expect("the window wears a temperature");
    assert!(
        (warm - 40.0).abs() < 0.01,
        "the window wears the temperature of the workspace it stands in: {warm}"
    );
    assert_eq!(
        opened.your_colour,
        Some(ui::tokens::PresenceHue::of_identity("person")),
        "and the colour of the person at this machine"
    );
    assert!(
        opened.corner_radius.is_some_and(|radius| radius > 0.0),
        "a window framed among others is drawn with the theme's corners: {:?}",
        opened.corner_radius
    );

    // The workspace is given another light: the window is told, on the pass that looks at the
    // temperatures again.
    warmed(workspace, 200.0);
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    let changed = appearance(
        &told(&mut window, &mut session, 200)
            .expect("the window hears the workspace's new temperature"),
    );
    let cool = changed
        .workspace_hue
        .expect("the window wears a temperature");
    assert!(
        (cool - 200.0).abs() < 0.01,
        "the window takes the workspace's temperature when it changes: {cool}"
    );
    assert_eq!(
        changed.your_colour, opened.your_colour,
        "while the person's colour stays theirs"
    );

    // Nothing changed since: the window is told nothing more.
    assert!(
        told(&mut window, &mut session, 100).is_none(),
        "a window is told only what changed"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}
