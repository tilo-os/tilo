//! A headless session linked to a real device daemon, with a real tilolang app installed and
//! started through the session's own launcher: the fixture the trigger, focus and window tests
//! read their flows through.
//!
//! The daemon is `tilo-runtime`'s, started in this process over a profile and a home of its own
//! ([`tilo_runtime::daemon::Running`]); the session is this process's, and takes the shell link the
//! daemon made for it, as a computer's session does. The apps the session starts connect to its app
//! socket, which attests each and hands it to the daemon over that link: the daemon is the one
//! broker, and what an app publishes is what the device's dispatch lists.
//!
//! What a test reads is what a person's own program reads: the fixture's second shell link, made by
//! the same daemon and used as the person ([`DaemonShell`]), and the state and windows the session
//! publishes. Nothing here reaches into the session's or the daemon's internals.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{RemoteActionTarget, workspace_provider};
use tilo_app_launch::{Launch, SessionEnvironment};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::apps::Apps;
use tilo_session::{AppActions, AppLists, AppSocket, Launches, Lists, Options, Session};
use tilo_session_client::daemon_documents::{DaemonShell, inherited_link, whole};

use crate::common::{HEIGHT, WIDTH, wait_for};

/// The tilolang app a fixture starts unless its builder names another: a real app of this tree,
/// built and installed as a device has it, whose window publishes its actions.
pub const DEFAULT_APP: &str = "so.tilo.chat";

/// How the fixture is made: which app it installs, who the person is, how many of the
/// person's devices it holds, and the services its session hosts.
pub struct Builder {
    app: Option<String>,
    start_it: bool,
    person: String,
    devices: usize,
    services: Vec<Box<dyn tilo_service::Service>>,
}

impl Builder {
    /// A fixture installing and starting the tilolang app `app`.
    #[must_use]
    pub fn of(app: impl Into<String>) -> Self {
        Self {
            app: Some(app.into()),
            start_it: true,
            person: "Ada".to_owned(),
            devices: 1,
            services: Vec::new(),
        }
    }

    /// A fixture installing the tilolang app `app`, which the test starts itself (an app-opened
    /// trigger arms before the app opens).
    #[must_use]
    pub fn installing(app: impl Into<String>) -> Self {
        Self {
            app: Some(app.into()),
            start_it: false,
            person: "Ada".to_owned(),
            devices: 1,
            services: Vec::new(),
        }
    }

    /// A fixture that installs and starts no app: the flows that run none need none.
    #[must_use]
    pub fn without_an_app() -> Self {
        Self {
            app: None,
            start_it: false,
            person: "Ada".to_owned(),
            devices: 1,
            services: Vec::new(),
        }
    }

    /// Have the session host `service` beside the desktop's own, as a development one does (the
    /// person's settings service among them).
    #[must_use]
    pub fn service(mut self, service: Box<dyn tilo_service::Service>) -> Self {
        self.services.push(service);
        self
    }

    /// The person whose session this is, by the name the fixture keeps of them.
    #[must_use]
    pub fn person(mut self, person: impl Into<String>) -> Self {
        self.person = person.into();
        self
    }

    /// The devices of the person's the fixture starts: one per computer, each with its own daemon
    /// and session.
    ///
    /// # Panics
    /// More than one: this fixture starts one computer's device in this process, and another
    /// computer is another process's.
    #[must_use]
    pub fn devices(mut self, devices: usize) -> Self {
        self.devices = devices;
        self
    }

    /// The fixture, started: the device's daemon, the session on its link, and the app installed.
    ///
    /// # Panics
    /// The daemon or the session does not start, or a package cannot be installed.
    #[track_caller]
    pub fn start(self) -> Fixture {
        // What the daemon says of the calls it serves, at `RUST_LOG`'s word (warnings by default):
        // a link it closes and a call it refuses are explained there.
        crate::common::people::install_log();
        assert_eq!(
            self.devices, 1,
            "this fixture starts one computer's device in this process; a second device is \
             another process's"
        );
        let within = tilo_test_env::hang_bound();
        let (home, launcher) = home_with_apps(self.app.is_some());
        // The store the fixture's apps are installed in: a test that installs a package of its own
        // (a theme's bundle) reads the same store the launcher runs its apps from.
        let store = home.data().join("tilo-apps");
        // The environment a launch starts apps in, read once the home's variables are set.
        let environment = SessionEnvironment::from_process();
        let wake = tilo_service::LoopWake::default();
        let apps = Rc::new(Apps::with_launcher(
            environment,
            Some(wake.handle()),
            launcher,
        ));
        let launch_record = Arc::new(Launches::default());
        let lists = Lists::fixed(AppLists::read());
        // This computer's person is founded by its daemon, as the first set-up on a machine does,
        // by the name the fixture's builder gave them.
        let crate::common::Device {
            daemon,
            person,
            home,
            profile,
        } = crate::common::device_daemon(home, Some(&self.person));
        let session_link = daemon
            .link_shell(within)
            .expect("the daemon makes the session's link");
        // The fixture's own link, as the person: what a program of theirs reads and runs over.
        let fixture_link = daemon
            .link_shell(within)
            .expect("the daemon makes the fixture's link");
        // Where the compositor's socket names itself, as `main` resolves the display it binds.
        let runtime_dir = home.runtime().to_path_buf();
        // The dispatch the session's services reach the daemon on is carried by this process's
        // runtime, and the session asks for one as it starts: the runtime is entered here, as the
        // session's own services enter it where they ask.
        let runtime = tilo_rt::app().handle().clone();
        let mut session = {
            let _inside = runtime.enter();
            Session::headless(
                Options {
                    presentation: ui::headless::fixture_runner_inputs,
                    person: person.clone(),
                    collab: false,
                    app_socket: AppSocket::Launcher,
                    apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
                    launch: Some(Rc::clone(&apps)),
                    launches: Arc::clone(&launch_record),
                    lists,
                    services: self.services,
                    shell_link: Some(session_link),
                    shell_daemon: Some(Box::new((daemon, profile, home))),
                    wake,
                    ..Options::default()
                },
                &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
            )
            .expect("session")
        };
        // The compositor binds its Wayland socket: the windows the session starts draw there.
        let socket = session.compositor().listen().expect("the display binds");
        apps.set_displays(runtime_dir.join(socket), None, None);
        // The workspace the shell shows is the person's own, founded by their daemon: what the
        // fixture's programs act in, read once it stands.
        let workspace = wait_for(
            &mut session,
            "the workspace the person's daemon holds",
            |session| {
                session.step(Duration::from_millis(1)).expect("step");
                session.handle().read(|tree| {
                    tree.active_workspace()
                        .map(|workspace| workspace.id().to_string())
                })
            },
        );
        let shell = DaemonShell::new(
            inherited_link(Some(fixture_link), &runtime).map(|(link, _)| link),
            runtime,
        );
        let mut fixture = Fixture {
            session,
            apps,
            launches: launch_record,
            shell,
            workspace,
            app: self.app,
            person: self.person,
            person_id: person,
            store,
        };
        if self.start_it
            && let Some(app) = fixture.app.clone()
        {
            fixture.open(&app);
        }
        fixture
    }
}

/// This computer's home, as a device that installed this tree's apps holds it: the identity its
/// daemon keeps for the person and certifies this device with, and, when `installs`, the store of
/// apps and the runtimes their windows run on, with the publisher its launcher trusts to run them.
/// Answers the launcher that starts them, or `None` for a computer that installs nothing: the
/// flows that run no app need neither, and start nothing.
///
/// # Panics
/// The home cannot be made, a package cannot be installed, or no run built the launcher.
fn home_with_apps(installs: bool) -> (tilo_test_env::TestHome, Option<std::path::PathBuf>) {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let identity = home.root().join("identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", &identity)
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let store = home.data().join("tilo-apps");
    home.env().set("TILO_APPS_DATA_DIR", &store);
    if !installs {
        return (home, None);
    }
    crate::common::dev_apps::install(&store, &home.state().join("tilolang"));
    crate::common::dev_apps::install_runtimes(&home.data().join("tilo-runtime"));
    home.env().set(
        "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
        &crate::common::dev_apps::runtimes().trusted,
    );
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    (home, Some(launcher))
}

/// A session on a real device daemon, with the app `app` installed and its window open.
pub struct Fixture {
    /// The session, as this process runs it: what a test steps and reads.
    pub session: Session,
    /// The apps the session starts, as search and the desktop reach them.
    pub apps: Rc<Apps>,
    /// What the session started, which an app-opened trigger follows.
    pub launches: Arc<Launches>,
    /// The person's own link to the daemon: the fixture reads and runs as them.
    pub shell: DaemonShell,
    /// The workspace all of it stands in, by id.
    pub workspace: String,
    /// The app installed and started, when the fixture names one.
    pub app: Option<String>,
    /// The person, by the name the fixture's builder gave them.
    pub person: String,
    /// The person's root, as the workspace and its apps name them.
    pub person_id: String,
    /// The store this computer installed its packages in, as the launcher reads it.
    pub store: PathBuf,
}

impl Fixture {
    /// `ask` the device's daemon, answered while this session steps: the daemon asks the session
    /// for what its apps publish as it decides a call, so a caller that waited on this thread
    /// without stepping would wait on itself.
    ///
    /// # Panics
    /// The daemon stopped, or did not answer within the test's bound.
    #[track_caller]
    fn answered<T: Send + 'static>(
        &mut self,
        ask: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, String> {
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let _task = self.shell.runtime().spawn(async move {
            let _ = answered.send(ask.await);
        });
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.session.step(Duration::from_millis(2)).expect("step");
            match answer.try_recv() {
                Ok(answered) => return answered,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("the device's daemon stopped before it answered".to_owned());
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err("the device's daemon did not answer".to_owned());
            }
        }
    }

    /// Save `shortcut` in the workspace, as the person's own save does: the workspace's shortcut
    /// library signs it by this device and commits it into the workspace's settings.
    ///
    /// # Panics
    /// The workspace refused the save.
    #[track_caller]
    pub fn save_shortcut(&mut self, shortcut: Value) {
        let target = RemoteActionTarget {
            provider: workspace_provider(&self.workspace),
            address: ActionAddress::new("shortcuts", "create"),
        };
        let arguments = Map::from_iter([("shortcut".to_owned(), shortcut)]);
        let request = crate::common::invocation(&target, &Value::Object(arguments), None, None);
        let (shell, workspace) = (self.shell.clone(), self.workspace.clone());
        // The save is followed until it ends, as the person's own save is: it may be accepted
        // first and committed a moment later.
        self.answered(async move {
            shell
                .link()?
                .dispatch()
                .settled(Some(workspace), request)
                .await
        })
        .unwrap_or_else(|why| panic!("the person saves their shortcut: {why}"));
    }

    /// Step the session until the shell shows the workspace's saved shortcut `id`, where the
    /// session arms it for its triggers.
    #[track_caller]
    pub fn wait_armed(&mut self, id: &str) {
        let id = id.to_owned();
        wait_for(
            &mut self.session,
            format!("{id} to reach the shell"),
            |session| {
                session.step(Duration::from_millis(1)).expect("step");
                session
                    .handle()
                    .read(|tree| {
                        tree.workspaces.iter().any(|workspace| {
                            workspace.shortcuts.iter().any(|saved| saved.id() == id)
                        })
                    })
                    .then_some(())
            },
        );
        // The pass that showed it is the one that arms it: the shell's revision moved. The look
        // that arms it is also the first that learns which apps run and which focus mode is on,
        // which an app-opened or focus trigger fires on the change from: step past that look, so
        // what the test starts next is the change it reads.
        self.pass(Duration::from_millis(700));
    }

    /// Step the session past `span` of its passing time.
    pub fn pass(&mut self, span: Duration) {
        let started = std::time::Instant::now();
        while started.elapsed() < span {
            self.session.step(Duration::from_millis(10)).expect("step");
        }
    }

    /// Press `keys` together and let go of them in reverse, as a person does.
    pub fn chord(&mut self, keys: &[u32]) {
        for keycode in keys {
            self.session
                .compositor()
                .input(InputEvent::Key {
                    keycode: *keycode,
                    pressed: true,
                })
                .expect("the key");
            self.session.step(Duration::from_millis(1)).expect("step");
        }
        for keycode in keys.iter().rev() {
            self.session
                .compositor()
                .input(InputEvent::Key {
                    keycode: *keycode,
                    pressed: false,
                })
                .expect("the key");
            self.session.step(Duration::from_millis(1)).expect("step");
        }
    }

    /// The invocations the device's daemon keeps of the workspace, newest first: what its record
    /// of calls answers the person.
    ///
    /// # Panics
    /// The daemon did not answer its record of invocations.
    #[track_caller]
    pub fn invocations(&mut self) -> Vec<tilo_actions::remote::workspace_invocations::Invocation> {
        let (shell, workspace) = (self.shell.clone(), self.workspace.clone());
        let reading = self
            .answered(async move { shell.read(whole(&workspace, "tilo.invocations", "")).await })
            .unwrap_or_else(|why| panic!("the device's invocations: {why}"));
        let value = reading["records"]
            .as_array()
            .and_then(|records| records.first())
            .and_then(|record| record.get("value"))
            .cloned()
            .unwrap_or_else(|| panic!("the invocations read answers a value: {reading}"));
        serde_json::from_value(value["items"].clone()).unwrap_or_else(|error| {
            panic!("the invocations read as invocations: {error}; it says {reading}")
        })
    }

    /// The runs of the shortcut `id` the daemon has finished, as its invocation records say.
    #[track_caller]
    pub fn runs(
        &mut self,
        id: &str,
    ) -> Vec<tilo_actions::remote::workspace_invocations::Invocation> {
        let wanted = format!("shortcuts.shortcut:{id}#run");
        self.invocations()
            .into_iter()
            .filter(|invocation| {
                invocation.action.as_deref() == Some(wanted.as_str())
                    && invocation.status
                        == tilo_actions::remote::workspace_invocations::InvocationStatus::Ended
            })
            .collect()
    }

    /// Step the session until the daemon records `wanted` finished runs of the shortcut `id`, and
    /// answer them.
    #[track_caller]
    pub fn until_runs(
        &mut self,
        id: &str,
        wanted: usize,
    ) -> Vec<tilo_actions::remote::workspace_invocations::Invocation> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.session.step(Duration::from_millis(2)).expect("step");
            let runs = self.runs(id);
            if runs.len() >= wanted {
                return runs;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon never recorded {wanted} runs of {id}; it recorded {}",
                runs.len()
            );
        }
    }

    /// Start `app` in the workspace, as the person opening it does: the session's own launcher
    /// starts its window, and the app connects to the session's app socket.
    ///
    /// # Panics
    /// The launch did not start, or the app never published its actions.
    #[track_caller]
    pub fn open(&mut self, app: &str) {
        let workspace = self.apps.workspace_named(&self.workspace);
        self.apps
            .start(
                &Launch::Package {
                    app: app.to_owned(),
                    open: None,
                },
                &workspace,
            )
            .unwrap_or_else(|why| panic!("{app} starts: {why}"));
        self.until_published(app);
    }

    /// Step the session until `app` publishes actions on the device's dispatch, as a running app
    /// does.
    #[track_caller]
    pub fn until_published(&mut self, app: &str) {
        let app = app.to_owned();
        let host = Rc::clone(&self.apps);
        let workspace = self.workspace.clone();
        wait_for(
            &mut self.session,
            format!("{app} to publish its actions"),
            move |session| {
                session.step(Duration::from_millis(2)).expect("step");
                host.published(&workspace)
                    .into_iter()
                    .any(|published| published.app == app && !published.actions.is_empty())
                    .then_some(())
            },
        );
    }

    /// What `app` publishes in the workspace: each action, by the resource it stands on.
    #[must_use]
    pub fn published(&self, app: &str) -> Vec<tilo_actions::ActionDescriptor> {
        self.apps
            .published(&self.workspace)
            .into_iter()
            .filter(|published| published.app == app)
            .flat_map(|published| published.actions)
            .collect()
    }

    /// The target of the action `action` `app` publishes on resource `resource`, as the person's
    /// own row names it.
    ///
    /// # Panics
    /// The app publishes no such action.
    #[track_caller]
    pub fn action_target(&mut self, app: &str, resource: &str, action: &str) -> RemoteActionTarget {
        let app = app.to_owned();
        let app_wanted = app.clone();
        let resource = resource.to_owned();
        let action = action.to_owned();
        let host = Rc::clone(&self.apps);
        let workspace = self.workspace.clone();
        let (wanted_resource, wanted_action) = (resource.clone(), action.clone());
        wait_for(
            &mut self.session,
            format!("{app}'s {action} on {resource}"),
            move |session| {
                session.step(Duration::from_millis(2)).expect("step");
                host.published(&workspace)
                    .into_iter()
                    .filter(|published| published.app == app_wanted)
                    .flat_map(|published| published.actions)
                    .find(|descriptor| {
                        descriptor.address.resource == wanted_resource
                            && descriptor.address.action == wanted_action
                    })
            },
        );
        tilo_actions::remote::application_action_target(&self.workspace, &app, resource, action)
    }

    /// Run the app's action `action` on `resource` as the person's own row does: through the
    /// session, where an action that takes them into the app brings its window forward.
    ///
    /// # Errors
    /// The session refused to carry it out.
    pub fn invoke(
        &mut self,
        app: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), tilo_actions::ApiError> {
        self.invoke_with(app, resource, action, Map::new())
    }

    /// [`Self::invoke`] with `arguments`, for an action that needs them.
    ///
    /// # Errors
    /// The session refused to carry it out.
    pub fn invoke_with(
        &mut self,
        app: &str,
        resource: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        let target = self.action_target(app, resource, action);
        self.apps.invoke(target, arguments)
    }

    /// The resource `app` publishes under the kind ending in `kind` (`Conversations`, `Direct`),
    /// once it does; `None` while it publishes none.
    #[must_use]
    pub fn resource_of_kind(&self, app: &str, kind: &str) -> Option<String> {
        self.published(app)
            .into_iter()
            .find(|descriptor| descriptor.resource_kind.ends_with(kind))
            .map(|descriptor| descriptor.address.resource)
    }

    /// Step the session until `app` publishes the action `action` on the resource the kind
    /// `kind` names, and answer the resource it stands on.
    ///
    /// # Panics
    /// It never does.
    #[track_caller]
    pub fn until_publishing(&mut self, app: &str, kind: &str, action: &str) -> String {
        let app_wanted = app.to_owned();
        let kind = kind.to_owned();
        let action = action.to_owned();
        let host = Rc::clone(&self.apps);
        let workspace = self.workspace.clone();
        wait_for(
            &mut self.session,
            format!("{app} to publish {action} on a {kind}"),
            move |session| {
                session.step(Duration::from_millis(2)).expect("step");
                host.published(&workspace)
                    .into_iter()
                    .filter(|published| published.app == app_wanted)
                    .flat_map(|published| published.actions)
                    .find(|descriptor| {
                        descriptor.resource_kind.ends_with(&kind)
                            && descriptor.address.action == action
                    })
                    .map(|descriptor| descriptor.address.resource)
            },
        )
    }

    /// The app whose window is in front, as the session publishes its windows; `None` while no
    /// window is.
    #[must_use]
    pub fn in_front(&self) -> Option<String> {
        crate::common::windows(&self.session)
            .into_iter()
            .find(|window| window.focused)
            .map(|window| window.app_key)
    }

    /// Step the session until the app `app`'s window is in front.
    ///
    /// # Panics
    /// It never is.
    #[track_caller]
    pub fn until_in_front(&mut self, app: &str) {
        let app = app.to_owned();
        wait_for(
            &mut self.session,
            format!("{app}'s window in front"),
            |session| {
                session.step(Duration::from_millis(2)).expect("step");
                crate::common::windows(session)
                    .into_iter()
                    .find(|window| window.focused)
                    .filter(|window| window.app_key == app)
                    .map(|_| ())
            },
        );
    }
}

/// `shortcut` of `id` named `name`, with the triggers `triggers`.
#[must_use]
pub fn shortcut(id: &str, name: &str, triggers: Value) -> Value {
    json!({
        "id": id,
        "name": name,
        "steps": [],
        "triggers": triggers,
    })
}
