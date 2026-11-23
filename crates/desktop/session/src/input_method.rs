//! The composing input method: the engine of the active input source. Chinese, Japanese and Korean
//! are composed, not mapped: a program holds the seat's `input-method-v2` and turns typed keys into
//! candidate characters. That program is Fcitx5 (Rime, Anthy, Hangul and XIM), started by the
//! session on a display of its own — as a surface app gets one — so only it and what it starts can
//! reach the seat's input method on the person's display. The session owns which engine is active,
//! so the engine's configuration is generated on every start: its own switching chords are cleared
//! (Super+Space is the session's search, Ctrl+Space belongs to the focused app) and the active
//! source's engine is the one it starts with. An engine of the active source means the on-screen
//! keyboard does not run: one input method owns the seat. It also serves X11 clients, one workspace
//! display at a time.
//!
//! A kept launch's X11 programs show on a display of the launch's own, which admits only the
//! keeper's cookie, so the session writes an authority file the engine reads: every kept display's
//! cookie in one file, which `xcb_connect` picks from by the display it is asked for.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use settings_store::hardware::InputMethodEngine;
use tilo_compositor::Compositor;

use crate::config::Sources;

/// Where the composing input method is installed. Its private display admits only this program and
/// what it starts.
const PROGRAM: &str = "/usr/bin/fcitx5";

/// Addons of the engine that the session does not use. The tray and panel addons need services the
/// session does not run, the clipboard addon claims a global chord, and the IBus/Fcitx4 frontends
/// would claim bus names nobody asked for. XIM stays: it serves X11 apps on the display the session
/// starts.
const DISABLED_ADDONS: &str =
    "clipboard,kimpanel,notificationitem,ibusfrontend,fcitx4frontend,virtualkeyboard";

/// What the engine needs from the session: its user, locale, session bus, and the directories its
/// dictionaries and engines live in. Everything else — the session's own display, the sockets it
/// bound, the X display around a nested session — stays out of the engine's reach.
const INHERITED_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// How often the engine is started before it is given up on: one that will not stay up leaves the
/// person typing their layout plainly, rather than starting for ever.
const TRIES: u32 = 3;

/// How long the engine has to exit when it is stopped, before it is killed with everything it
/// started.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// The name the engine takes on the session bus, and the controller object X11 displays are handed
/// to.
const BUS_NAME: &str = "org.fcitx.Fcitx5";
const CONTROLLER_PATH: &str = "/controller";
const CONTROLLER_INTERFACE: &str = "org.fcitx.Fcitx.Controller1";

/// How often, and how far apart, the engine is asked to open an X11 display: it takes its bus name
/// once its addons are loaded, and the display may start before that.
const XIM_TRIES: u32 = 100;
const XIM_RETRY: Duration = Duration::from_millis(100);

/// How often the engine is asked again for a display it refused: it still holds the connection of
/// a server that just stopped, and a new one on the same number starts at once.
const XIM_REFUSED_TRIES: u32 = 10;

/// The authority file the engine reads: the cookie of each kept display, so it can connect to a
/// display that admits only the keeper's own.
const AUTHORITY: &str = "Xauthority";

/// Whether the active source is an engine this image can run, so the seat is an input method's and
/// not the on-screen keyboard's.
pub(crate) fn composes(source: &str) -> bool {
    engine(source).is_some()
}

/// The engine the source `id` names, when this image has both it and the program that runs it.
fn engine(id: &str) -> Option<InputMethodEngine> {
    InputMethodEngine::from_source_id(id).filter(|engine| installed(*engine))
}

/// Whether the engine's addon and the program that runs it are installed.
fn installed(engine: InputMethodEngine) -> bool {
    Path::new(PROGRAM).is_file() && engine.is_installed()
}

/// An X11 display X11 programs show on, as the engine is asked to serve it: the display's name
/// and, for a kept launch's own display, the cookie file that lets a client onto it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
    pub(crate) display: String,
    pub(crate) authority: Option<PathBuf>,
}

/// The engine's run for one person, as the session's loop keeps it.
#[derive(Default)]
pub(crate) struct InputMethod {
    run: Run,
    /// The engine the active source last asked for, so a source switch is told from an engine that
    /// will not stay up.
    aimed: Option<InputMethodEngine>,
    /// How often the engine was started since the source last wanted it, so one that keeps exiting
    /// is given up on rather than started for ever.
    starts: u32,
    /// The private display the engine's connections are accepted on, while it runs: dropping it
    /// stops accepting them.
    display: Option<tilo_compositor::SurfaceDisplay>,
    /// The displays whose Xwayland servers run, which the engine serves.
    x11: Vec<Served>,
    /// The displays the engine was already asked to serve, with the cookie each was served with.
    opened: Vec<Served>,
}

#[derive(Default)]
enum Run {
    /// No engine runs: none is active, or none is installed.
    #[default]
    Down,
    /// The engine runs as this process.
    Running {
        engine: InputMethodEngine,
        child: Child,
    },
    /// The engine would not stay up, and is not started again this session.
    Failed,
}

impl InputMethod {
    /// The displays whose Xwayland servers run, which the engine serves once it runs.
    pub(crate) fn serve_x11(&mut self, displays: Vec<Served>) {
        if self.x11 == displays {
            return;
        }
        // A display whose server stopped is served again when the next client starts one there,
        // even though the number is the same: the cookie that admits it may be another's.
        self.opened.retain(|open| displays.contains(open));
        self.x11 = displays;
        // The engine reads this file when it connects to a display, so it is written before the
        // engine is asked to serve the display.
        self.write_authority();
    }

    /// Write the engine's authority file: one cookie for each kept display served now.
    fn write_authority(&self) {
        let Some(path) = authority_file() else {
            return;
        };
        let mut contents = Vec::new();
        for served in &self.x11 {
            let Some(authority) = &served.authority else {
                continue;
            };
            match std::fs::read(authority) {
                Ok(cookie) => contents.extend_from_slice(&cookie),
                Err(error) => tracing::warn!(
                    display = %served.display,
                    path = %authority.display(),
                    %error,
                    "a kept display's cookie was not read: the input method cannot serve it"
                ),
            }
        }
        if let Err(error) =
            tilo_files::native::atomic::write_creating_parent_with_mode(&path, &contents, 0o600)
        {
            tracing::warn!(path = %path.display(), %error, "the input method's X authority was not written");
        }
    }

    /// Follow the active input source: run the engine it names, on a display of the engine's own,
    /// and stop it when the person types in a layout again. An engine that exits is started again,
    /// a bounded number of times.
    pub(crate) fn follow(
        &mut self,
        compositor: &mut Compositor,
        sources: &Sources,
        runtime: &tokio::runtime::Runtime,
    ) {
        let wanted = engine(&sources.active);
        if self.aimed != wanted {
            // The person types in another source now: what ran is stopped,
            // and an engine given up on is given its tries back.
            self.stop();
            self.aimed = wanted;
            self.starts = 0;
        }
        let Some(engine) = wanted else {
            return;
        };
        // What exited is stopped; the pass below asks for another run, unless the engine used its
        // tries.
        self.exited();
        if self.running() {
            self.open_x11(runtime);
            return;
        }
        if matches!(self.run, Run::Failed) {
            return;
        }
        self.start(compositor, engine, sources);
        if self.running() {
            self.open_x11(runtime);
        }
    }

    /// Whether the engine runs now.
    fn running(&self) -> bool {
        matches!(self.run, Run::Running { .. })
    }

    /// Take in whether the engine's process exited, and stop what it left.
    fn exited(&mut self) -> bool {
        let Run::Running { engine, child } = &mut self.run else {
            return false;
        };
        let engine = *engine;
        match child.try_wait() {
            Ok(None) => return false,
            Ok(Some(status)) => {
                tracing::warn!(engine = engine.name(), ?status, "the input method exited");
            }
            Err(error) => {
                tracing::warn!(
                    engine = engine.name(),
                    %error,
                    "could not tell whether the input method runs"
                );
            }
        }
        self.stop();
        true
    }

    /// Stop the engine, and stop accepting connections on its display.
    fn stop(&mut self) {
        if let Run::Running { mut child, .. } = std::mem::take(&mut self.run) {
            // Its own group: what the engine started goes with it, and the display it composed on
            // is nobody's once it is gone.
            bounded_command::terminate_tree(&mut child, STOP_GRACE);
        }
        self.run = Run::Down;
        self.display = None;
        // A started engine serves no display yet: what ran before it is its own, and XIM goes with
        // it.
        self.opened.clear();
    }

    /// Start `engine` on a display of its own, typing on the source list's layout. A start that
    /// cannot be made is what the next pass tries again.
    fn start(&mut self, compositor: &mut Compositor, engine: InputMethodEngine, sources: &Sources) {
        if self.starts >= TRIES {
            // A start that cannot be made leaves nothing running, so the next pass would ask again:
            // this is where asking stops.
            tracing::error!(
                engine = engine.name(),
                starts = self.starts,
                "the input method would not stay up: it is not started again this session"
            );
            self.run = Run::Failed;
            return;
        }
        self.starts = self.starts.saturating_add(1);
        let Some(directory) = config_directory() else {
            tracing::warn!(
                engine = engine.name(),
                "the session has no runtime directory: no input method"
            );
            return;
        };
        let display = match compositor.bind_surface_display() {
            Ok(display) => display,
            Err(error) => {
                tracing::warn!(engine = engine.name(), %error, "no display for the input method");
                return;
            }
        };
        let launch = launch(engine, &layout(sources), display.path(), &directory);
        for (path, body) in &launch.files {
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                tracing::warn!(engine = engine.name(), path = %parent.display(), %error, "the input method's configuration was not made");
                return;
            }
            if let Err(error) = std::fs::write(path, body) {
                tracing::warn!(engine = engine.name(), path = %path.display(), %error, "the input method's configuration was not written");
                return;
            }
        }
        // The engine may be asked for a display at any moment after this: the file it reads for
        // the kept displays' cookies is there before it starts.
        self.write_authority();
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for name in &launch.inherit {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.envs(launch.env.iter().map(|(name, value)| (name, value)));
        bounded_command::own_process_tree(&mut command);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(
                    engine = engine.name(),
                    program = %launch.program.display(),
                    %error,
                    "the input method did not start"
                );
                return;
            }
        };
        // What connects to the engine's display from here on is the engine and what it starts: the
        // seat's input method is theirs to bind.
        match compositor.accept_surface_clients(display, child.id(), None) {
            Ok(display) => self.display = Some(display),
            Err(error) => {
                tracing::warn!(engine = engine.name(), %error, "the input method's display did not listen");
            }
        }
        tracing::info!(
            engine = engine.name(),
            program = %launch.program.display(),
            "the input method composes for this session"
        );
        self.run = Run::Running { engine, child };
    }

    /// Ask the engine, over the session bus, to serve each X11 display the session's X11 apps show
    /// on and it does not serve yet. Runs off the loop: the engine answers after its X handshake.
    fn open_x11(&mut self, runtime: &tokio::runtime::Runtime) {
        let pending: Vec<Served> = self
            .x11
            .iter()
            .filter(|served| !self.opened.contains(served))
            .cloned()
            .collect();
        for served in pending {
            let number = served.display.clone();
            self.opened.push(served);
            runtime.spawn(async move {
                match ask_x11(number.clone()).await {
                    Ok(()) => tracing::info!(x11_display = %number, "the input method serves the X11 display"),
                    Err(error) => {
                        tracing::warn!(x11_display = %number, %error, "the input method could not serve the X11 display");
                    }
                }
            });
        }
    }
}

impl crate::Session {
    /// Follow the composing input method of the active input source: run its engine while a source
    /// names one, and serve every display whose Xwayland server runs from it.
    pub(crate) fn follow_input_method(&mut self) {
        let mut running: Vec<Served> = self
            .compositor
            .x11_displays()
            .map(|displays| {
                displays
                    .running()
                    .into_iter()
                    .map(|number| Served {
                        display: format!(":{number}"),
                        authority: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A kept launch shows its X11 programs on a display of its own, which admits only the
        // keeper's cookie: the engine serves those too.
        if let Some(apps) = &self.shared.launch {
            running.extend(
                apps.kept_x11()
                    .into_iter()
                    .map(|(display, authority)| Served {
                        display,
                        authority: Some(authority),
                    }),
            );
        }
        self.input_method.serve_x11(running);
        self.input_method.follow(
            &mut self.compositor,
            &self.sources,
            &self.automation_runtime,
        );
    }
}

impl Drop for InputMethod {
    fn drop(&mut self) {
        // The engine outlives no session: it owns the seat's input method,
        // which the next session's engine takes with `--replace` anyway.
        self.stop();
    }
}

/// `$XDG_RUNTIME_DIR/tilo-session/input-method`: the engine's configuration,
/// regenerated on every start.
fn config_directory() -> Option<PathBuf> {
    Some(tilo_paths::login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))?.join("input-method"))
}

/// The authority file the engine reads, in the session's directory.
fn authority_file() -> Option<PathBuf> {
    Some(config_directory()?.join(AUTHORITY))
}

/// The XKB layout the engine's own key symbols stand on: the first layout of the source list, as
/// engines type on the primary layout.
fn layout(sources: &Sources) -> String {
    sources
        .compiled()
        .layout
        .split(',')
        .next()
        .filter(|layout| !layout.is_empty())
        .unwrap_or("us")
        .to_owned()
}

/// The engine's process description for one start.
struct Launch {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// The only variables inherited from the session; see [`INHERITED_ENV`].
    inherit: Vec<&'static str>,
    /// The configuration files to write before the engine starts.
    files: Vec<(PathBuf, String)>,
}

/// Describe the engine's run: `engine` typing on `layout` , connected to `display` (its own),
/// configured from `directory` .
fn launch(engine: InputMethodEngine, layout: &str, display: &Path, directory: &Path) -> Launch {
    let profile = format!(
        "[Groups/0]\nName=Tilo\nDefault Layout={layout}\nDefaultIM={engine}\n\n\
         [Groups/0/Items/0]\nName=keyboard-{layout}\nLayout=\n\n\
         [Groups/0/Items/1]\nName={engine}\nLayout=\n\n\
         [GroupOrder]\n0=Tilo\n",
        engine = engine.name(),
    );
    // Shift toggles the engine's own Latin mode and the dedicated Hangul/Zenkaku keys switch it;
    // every enumerate chord is empty.
    let config = "[Hotkey]\nEnumerateWithTriggerKeys=False\n\n\
         [Hotkey/TriggerKeys]\n0=Zenkaku_Hankaku\n1=Hangul\n\n\
         [Hotkey/AltTriggerKeys]\n0=Shift_L\n\n\
         [Hotkey/EnumerateForwardKeys]\n\n[Hotkey/EnumerateBackwardKeys]\n\n\
         [Hotkey/EnumerateGroupForwardKeys]\n\n[Hotkey/EnumerateGroupBackwardKeys]\n\n\
         [Hotkey/ActivateKeys]\n\n[Hotkey/DeactivateKeys]\n\n\
         [Behavior]\nActiveByDefault=True\nShareInputState=All\n\
         ShowInputMethodInformation=False\nshowInputMethodInformationWhenFocusIn=False\n";
    let config_home = directory.display().to_string();
    Launch {
        program: PathBuf::from(PROGRAM),
        args: vec![
            "--replace".into(),
            "--disable".into(),
            DISABLED_ADDONS.into(),
        ],
        env: vec![
            ("WAYLAND_DISPLAY".into(), display.display().to_string()),
            ("XDG_CONFIG_HOME".into(), config_home),
            ("XDG_SESSION_TYPE".into(), "wayland".into()),
            // The cookies of the kept displays it serves: it reads this file when it connects.
            (
                "XAUTHORITY".into(),
                directory.join(AUTHORITY).display().to_string(),
            ),
        ],
        inherit: INHERITED_ENV.to_vec(),
        files: vec![
            (directory.join("fcitx5").join("profile"), profile),
            (directory.join("fcitx5").join("config"), config.to_owned()),
        ],
    }
}

/// Ask the engine, once it has taken its name on the session bus, to become the input method of
/// `display` .
async fn ask_x11(display: String) -> Result<(), zbus::Error> {
    let bus = zbus::Connection::session().await?;
    let mut tries = 0;
    let mut refused = 0;
    loop {
        match bus
            .call_method(
                Some(BUS_NAME),
                CONTROLLER_PATH,
                Some(CONTROLLER_INTERFACE),
                "OpenX11Connection",
                &(display.as_str(),),
            )
            .await
            .map(drop)
        {
            Ok(()) => return Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                    && tries < XIM_TRIES =>
            {
                tries = tries.saturating_add(1);
                tokio::time::sleep(XIM_RETRY).await;
            }
            // A display it still holds the connection of, whose server just stopped: the next one
            // on the same number is served once it let go.
            Err(zbus::Error::MethodError(..)) if refused < XIM_REFUSED_TRIES => {
                refused = refused.saturating_add(1);
                tokio::time::sleep(XIM_RETRY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The controller a stand-in engine answers: the displays it was asked to serve.
    struct Controller {
        served: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[zbus::interface(name = "org.fcitx.Fcitx.Controller1")]
    impl Controller {
        fn open_x11_connection(&self, name: &str) {
            self.served.lock().expect("the calls").push(name.to_owned());
        }
    }

    /// A kept launch's display is served to the engine with the cookie the keeper wrote, a session
    /// that follows an in-place restart serves the adopted keeper's display again, and the
    /// display leaves the engine's file once the keeper ends. The engine is a stand-in on a bus of
    /// the test's own: nothing here reaches this machine's session bus.
    #[test]
    fn a_kept_displays_cookie_reaches_the_engine_and_a_surviving_keeper_is_served_again() {
        use std::sync::{Arc, Mutex};

        let mut env = tilo_test_env::ScopedEnv::new();
        let home = tempfile::tempdir().expect("a directory");
        env.set("XDG_RUNTIME_DIR", home.path());
        let bus = tilo_test_env::PrivateBus::start();
        env.set("DBUS_SESSION_BUS_ADDRESS", bus.address());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-input-method-test")
            .enable_all()
            .build()
            .expect("runtime");
        let served = Arc::new(Mutex::new(Vec::new()));
        let engine = runtime.block_on(async {
            let connection = zbus::Connection::session().await.expect("the test's bus");
            connection
                .object_server()
                .at(
                    "/controller",
                    Controller {
                        served: Arc::clone(&served),
                    },
                )
                .await
                .expect("the controller");
            connection
                .request_name(BUS_NAME)
                .await
                .expect("the engine's name");
            connection
        });
        let asked = || served.lock().expect("the calls").len();

        let keeper = home.path().join("keeper");
        std::fs::create_dir_all(&keeper).expect("the keeper's directory");
        let cookie = keeper.join("Xauthority");
        tilo_files::native::x11_authority::write(&cookie, 91, &[7; 16])
            .expect("the keeper's cookie");
        let merged = home
            .path()
            .join("tilo-session")
            .join("input-method")
            .join(AUTHORITY);
        let display = Served {
            display: ":91".into(),
            authority: Some(cookie.clone()),
        };

        let mut first = InputMethod::default();
        first.serve_x11(vec![display.clone()]);
        first.open_x11(&runtime);
        tilo_test_env::wait_for("the engine is asked for the display", || {
            (asked() >= 1).then_some(())
        });
        assert_eq!(
            std::fs::read(&merged).expect("the engine's authority file"),
            std::fs::read(&cookie).expect("the keeper's cookie"),
            "the engine reads the cookie of the display it was asked for"
        );

        // The keeper ends: the display leaves what the session serves, and the engine's file.
        first.serve_x11(Vec::new());
        assert_eq!(
            std::fs::read(&merged).expect("the engine's authority file"),
            Vec::<u8>::new()
        );

        // The session that follows a restart adopts the keeper and serves its display again.
        let mut second = InputMethod::default();
        second.serve_x11(vec![display]);
        second.open_x11(&runtime);
        tilo_test_env::wait_for("the engine is asked for the display again", || {
            (asked() >= 2).then_some(())
        });
        assert_eq!(asked(), 2, "each session asked once");
        drop(engine);
    }

    fn profile_of(files: &[(PathBuf, String)]) -> &str {
        let (path, body) = files
            .iter()
            .find(|(path, _)| path.ends_with("fcitx5/profile"))
            .expect("a profile");
        assert!(path.is_absolute());
        body
    }

    fn config_of(files: &[(PathBuf, String)]) -> &str {
        let (_, body) = files
            .iter()
            .find(|(path, _)| path.ends_with("fcitx5/config"))
            .expect("a config");
        body
    }

    tilo_test_env::scenario! {
        fn the_input_method_engine_is_set_up_from_the_persons_sources;

        phase the_engine_runs_on_its_own_display_with_the_active_engine;
        {
            let launch = launch(
                InputMethodEngine::Hangul,
                "de",
                Path::new("/run/user/1000/tilo-surface-3"),
                Path::new("/run/user/1000/tilo-session/input-method"),
            );
            assert_eq!(launch.program, Path::new(PROGRAM));
            assert_eq!(launch.args.first().map(String::as_str), Some("--replace"));
            let disabled = launch
                .args
                .iter()
                .skip_while(|arg| *arg != "--disable")
                .nth(1)
                .expect("disabled addons");
            assert!(disabled.split(',').any(|addon| addon == "clipboard"));
            assert!(
                !disabled.split(',').any(|addon| addon == "xim"),
                "X11 apps compose through XIM"
            );
            assert!(launch.env.contains(&(
                "WAYLAND_DISPLAY".into(),
                "/run/user/1000/tilo-surface-3".into()
            )));
            assert!(launch.env.contains(&(
                "XDG_CONFIG_HOME".into(),
                "/run/user/1000/tilo-session/input-method".into()
            )));
            assert!(
                !launch.env.iter().any(|(name, _)| name == "DISPLAY"),
                "the engine composes on the display it was given, not the one around a nested session"
            );
            assert!(
                launch.inherit.contains(&"DBUS_SESSION_BUS_ADDRESS"),
                "the engine serves X11 displays over the session bus"
            );
            assert!(
                launch.env.iter().any(|(name, value)| name == "XAUTHORITY"
                    && value.ends_with("tilo-session/input-method/Xauthority")),
                "the engine reads the cookies of the kept displays it serves"
            );

            let profile = profile_of(&launch.files);
            assert!(profile.contains("DefaultIM=hangul"), "{profile}");
            assert!(profile.contains("Default Layout=de"), "{profile}");
            assert!(profile.contains("Name=keyboard-de"), "{profile}");
            assert!(profile.contains("Name=hangul"), "{profile}");
            let config = config_of(&launch.files);
            assert!(
                config.contains("[Hotkey/AltTriggerKeys]\n0=Shift_L"),
                "{config}"
            );
            assert!(
                config.contains("[Hotkey/EnumerateForwardKeys]\n\n"),
                "Super+Space stays the session's search"
            );
        }

        phase the_primary_layout_of_the_source_list_is_the_engine_s_layout;
        {
            let sources = Sources {
                ids: vec!["ime/rime".into(), "us:intl".into(), "de".into()],
                active: "ime/rime".into(),
                model: "pc105".into(),
                options: String::new(),
            };
            assert_eq!(layout(&sources), "us");
            let sources = Sources {
                ids: vec!["de".into()],
                active: "de".into(),
                model: "pc105".into(),
                options: String::new(),
            };
            assert_eq!(layout(&sources), "de");
        }

        phase a_layout_is_not_an_engine_and_an_unknown_source_composes_nothing;
        {
            assert!(!composes("de"));
            assert!(!composes("us:intl"));
            assert!(!composes("ime/unknown"));
            assert!(!composes(""));
        }
    }
}
