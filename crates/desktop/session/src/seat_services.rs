//! The seat this session holds for its device, served to the device's daemon over the shell link
//! (`tilo_workspace_service::apps::seat`): the screen and its windows' notes, the sound apps play,
//! the calls they capture, the person's settings and the displays Settings changes, the board of
//! live activities, what the session's services publish, the roles that follow records, and the
//! asks about a workspace's nodes.
//!
//! The daemon admits every app and forwards each call of these services here with the facts it
//! admitted the app on; no app reaches this provider itself. Each call is answered by the same
//! service code the session ran when it admitted apps itself.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::outcome::{ActionFailure, ActionOutput};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_workspace_service::apps::collab_app_broker::{AudioOutputServices, NativeMediaServices};
use tilo_workspace_service::apps::seat::{self, MediaContext, SeatReading, SeatView, calls};
use tilo_workspace_service::apps::services::{
    BrowserServiceSessionServices, DisplayControlContext, DisplayControlServices,
    PublishedResources,
};

/// How long a wait for a change is held here before it is answered as it stands: the daemon asks
/// again, so a wait whose app went away ends too.
const HELD: Duration = Duration::from_secs(25);

/// What the session holds for its seat, each piece the service it ran for its apps.
pub(crate) struct SeatParts {
    /// The device's output, which apps' sound plays on.
    pub audio_output: Option<Arc<dyn AudioOutputServices>>,
    /// The calls this session's apps join.
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    /// What each window shows, as its app reports it.
    pub continuations: Arc<dyn tilo_broker_core::steps::ContinuationServices>,
    /// What notes the workspaces' records changing, for the roles that follow them.
    pub records: Option<Arc<crate::resource_index::RecordChanges>>,
    /// What the session's services publish (the sound, the network, the displays), as apps read
    /// and change it.
    pub published: Arc<dyn PublishedResources>,
    /// The asks about a workspace's nodes, as the person here answers them.
    pub workspace_nodes: Option<Arc<dyn tilo_broker_core::services::WorkspaceNodeActions>>,
    /// The person's settings, in their own home.
    pub settings: Option<Arc<dyn settings_store::SettingsSource>>,
    /// The displays Settings changes.
    pub display_control: Option<Arc<dyn DisplayControlServices>>,
    /// The board every app's live activity shows on.
    pub live_activities: Arc<runtime_collab::LiveActivities>,
    /// The launches the session's loop makes: where an app's window opens.
    pub launches: Option<Arc<crate::launches::Launches>>,
    /// The device's keyboard shortcuts, read from the keymap this session runs:
    /// `crate::keyboard_shortcuts::servable()`.
    pub keyboard_shortcuts: Arc<dyn tilo_broker_core::services::KeyboardShortcutServices>,
    /// The person's account credentials, in the vault this session opens:
    /// `crate::service_sessions::ServiceSessions::new(engine, item_lending, service)`, `None`
    /// for a session with no engine (no workspace of its own).
    pub service_sessions: Option<Arc<dyn BrowserServiceSessionServices>>,
    /// Files' clipboard, whose copies reach the compositor's loop: the same
    /// `crate::file_clipboard::NativeFileClipboard` the loop takes the published copies from.
    pub file_clipboard: Option<Arc<crate::file_clipboard::NativeFileClipboard>>,
    /// The workspaces' agents as the records this session reads name them, and the agent
    /// sessions it serves: whom the daemon's broker admits as an agent, and whether a turn's
    /// session goes.
    pub agents: Option<SeatAgents>,
}

/// What [`SeatParts::agents`] holds.
pub(crate) struct SeatAgents {
    pub roster: Arc<crate::agents::Roster>,
    pub sessions: Arc<crate::agent_sessions::AgentSessions>,
}

impl SeatAgents {
    fn table(&self) -> seat::AgentTable {
        seat::AgentTable::of(self.roster.seat_agents(), self.sessions.seat_sessions())
    }

    /// The table once its revision differs from `after`, or as it stands after [`HELD`].
    async fn read(&self, after: u64) -> seat::AgentTable {
        let waited = tokio::time::timeout(HELD, async {
            loop {
                let (listed, going) = (self.roster.moved(), self.sessions.moved());
                tokio::pin!(listed, going);
                listed.as_mut().enable();
                going.as_mut().enable();
                let table = self.table();
                if table.revision != after {
                    return table;
                }
                tokio::select! {
                    () = listed => {}
                    () = going => {}
                }
            }
        })
        .await;
        waited.unwrap_or_else(|_| self.table())
    }
}

/// Serve the seat on the device's daemon over `shell`, its shell link, for as long as the link
/// lasts.
pub(crate) fn serve_on(shell: runtime_collab::ShellClient, parts: SeatParts) {
    let runtime = match tilo_rt::try_app() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "the device's daemon reaches none of this session's seat");
            return;
        }
    };
    let provider = Arc::new(SeatProvider {
        parts: Arc::new(parts),
        runtime: runtime.handle().clone(),
        handed: shell.handed().clone(),
    });
    runtime.spawn(async move {
        if let Err(error) = shell
            .provide(seat::PROVIDER, &CataloguePage::default(), provider)
            .await
        {
            tracing::warn!(%error, "the device's daemon reaches none of this session's seat");
        }
    });
}

/// The seat's one provider, as the daemon's forwarders call it.
struct SeatProvider {
    parts: Arc<SeatParts>,
    runtime: tokio::runtime::Handle,
    /// The files the daemon hands over on the shell link, each taken by the call that names it.
    handed: runtime_portal::descriptors::ReceivedDescriptors,
}

type Answer = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

/// `value` read as a `T`, or why not.
fn read<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("unreadable call: {error}"))
}

/// The field `name` of `arguments`, read as a `T`.
fn field<T: serde::de::DeserializeOwned>(arguments: &Value, name: &str) -> Result<T, String> {
    read(&arguments[name]).map_err(|error| format!("{name}: {error}"))
}

/// `value` as an answer.
fn answered<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// `work` run where it may block, answered.
fn blocking<T: serde::Serialize + Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Answer {
    Box::pin(async move {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| error.to_string())?
            .and_then(answered)
    })
}

/// A settings view of `app`, with a revision that changes exactly when the view does.
fn settings_view(
    source: &dyn settings_store::SettingsSource,
    app: &str,
    grants: &[String],
) -> SeatView {
    use std::hash::{Hash as _, Hasher as _};
    let document = Value::Object(runtime_collab::settings::host::view(source, app, grants));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    document.to_string().hash(&mut hasher);
    SeatView {
        revision: hasher.finish().max(1),
        document,
    }
}

/// The device's keyboard shortcuts as `shortcuts` answers them from the keymap this session runs,
/// with a revision that changes exactly when the table does.
fn shortcuts_now(
    shortcuts: &dyn tilo_broker_core::services::KeyboardShortcutServices,
) -> seat::ShortcutTable {
    use std::hash::{Hash as _, Hasher as _};
    let keymap = settings_store::hardware::ShortcutConfig::load();
    let rows = shortcuts.catalogue_rows("", &keymap);
    let answers = rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .map(|action| seat::ShortcutAnswer {
            address: action.address.clone(),
            answer: shortcuts
                .apply(&keymap, &action.address, None)
                .map_err(|failure| *failure),
        })
        .collect::<Vec<_>>();
    let mut table = seat::ShortcutTable {
        revision: 0,
        app_id: shortcuts.app_id().to_owned(),
        rows,
        answers,
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&table)
        .unwrap_or_default()
        .hash(&mut hasher);
    table.revision = hasher.finish().max(1);
    table
}

/// The shortcut table once its revision differs from `after`, or as it stands after [`HELD`]: the
/// keymap is the person's settings, so a change of them is when it is read again, and a while
/// otherwise (the language its names are in).
async fn shortcut_table(
    shortcuts: Arc<dyn tilo_broker_core::services::KeyboardShortcutServices>,
    settings: Option<Arc<dyn settings_store::SettingsSource>>,
    after: u64,
) -> Result<seat::ShortcutTable, String> {
    const AGAIN: Duration = Duration::from_secs(5);
    let changed = Arc::new(tokio::sync::Notify::new());
    let wake = Arc::clone(&changed);
    let listener: Arc<settings_store::Listener> = Arc::new(move || wake.notify_waiters());
    if let Some(settings) = &settings {
        settings.changes().listen(&listener);
    }
    let read = || {
        let shortcuts = Arc::clone(&shortcuts);
        async move {
            tokio::task::spawn_blocking(move || shortcuts_now(shortcuts.as_ref()))
                .await
                .map_err(|error| error.to_string())
        }
    };
    let waited = tokio::time::timeout(HELD, async {
        loop {
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let table = read().await?;
            if table.revision != after {
                return Ok(table);
            }
            let _ = tokio::time::timeout(AGAIN, notified).await;
        }
    })
    .await;
    let table =
        match tilo_failure::ignored!("a wait that ran out reads the table as it stands", waited) {
            Ok(table) => table,
            Err(_elapsed) => read().await,
        };
    drop(listener);
    table
}

impl SeatProvider {
    #[expect(
        clippy::too_many_lines,
        reason = "one table: each call of the seat, and the service that answers it"
    )]
    fn answer(&self, service: &str, call: &str, arguments: Value) -> Answer {
        let parts = Arc::clone(&self.parts);
        let failed = |why: String| -> Answer { Box::pin(std::future::ready(Err(why))) };
        let unserved = |what: &str| -> Answer {
            Box::pin(std::future::ready(Err(format!(
                "this session serves no {what}"
            ))))
        };
        match (service, call) {
            (calls::AUDIO_OUTPUT, calls::COMMAND) => {
                let Some(output) = parts.audio_output.clone() else {
                    return unserved("app audio");
                };
                match (
                    field::<MediaContext>(&arguments, "context"),
                    field(&arguments, "command"),
                ) {
                    (Ok(context), Ok(command)) => {
                        let played = output.output(context.context(), command);
                        Box::pin(async move { answered(played.await?) })
                    }
                    (Err(why), _) | (_, Err(why)) => failed(why),
                }
            }
            (calls::AUDIO_OUTPUT, calls::DISCONNECT) => {
                if let (Some(output), Ok(peer)) =
                    (&parts.audio_output, field::<u64>(&arguments, "peer"))
                {
                    output.disconnect(peer);
                }
                Box::pin(std::future::ready(Ok(Value::Null)))
            }
            (calls::NATIVE_MEDIA, calls::COMMAND) => {
                let Some(media) = parts.native_media.clone() else {
                    return unserved("calls");
                };
                match (
                    field::<MediaContext>(&arguments, "context"),
                    field(&arguments, "request"),
                ) {
                    (Ok(context), Ok(request)) => {
                        let answer = media.command(context.context(), request);
                        Box::pin(async move { answered(answer.await?) })
                    }
                    (Err(why), _) | (_, Err(why)) => failed(why),
                }
            }
            (calls::NATIVE_MEDIA, calls::DISCONNECT) => {
                if let (Some(media), Ok(peer)) =
                    (&parts.native_media, field::<u64>(&arguments, "peer"))
                {
                    media.disconnect(peer);
                }
                Box::pin(std::future::ready(Ok(Value::Null)))
            }
            (calls::CONTINUATIONS, call) => {
                let workspace = match field::<String>(&arguments, "workspace").and_then(|id| {
                    tilo_desktop_model::WorkspaceId::parse(&id)
                        .ok_or_else(|| "the call names no workspace".to_owned())
                }) {
                    Ok(workspace) => workspace,
                    Err(why) => return failed(why),
                };
                let (app, instance) = match (
                    field::<String>(&arguments, "app"),
                    field::<u64>(&arguments, "instance"),
                ) {
                    (Ok(app), Ok(instance)) => (app, instance),
                    (Err(why), _) | (_, Err(why)) => return failed(why),
                };
                let notes = &parts.continuations;
                let done = match call {
                    calls::REPORT => field(&arguments, "schema").and_then(|schema| {
                        notes.report(
                            workspace,
                            app,
                            instance,
                            schema,
                            field(&arguments, "metadata")?,
                        )
                    }),
                    calls::CLEAR => notes.clear(workspace, &app, instance),
                    calls::HANDOFF => field(&arguments, "message")
                        .and_then(|message| notes.handoff(workspace, app, instance, message)),
                    other => Err(format!("windows' notes take no {other}")),
                };
                Box::pin(std::future::ready(done.map(|()| Value::Null)))
            }
            (calls::RECORDS, calls::CHANGED) => {
                let Some(records) = parts.records.clone() else {
                    return unserved("record changes");
                };
                let changed = (|| {
                    records.changed(
                        &field::<String>(&arguments, "workspace")?,
                        &field::<String>(&arguments, "app")?,
                        &field::<String>(&arguments, "record")?,
                    );
                    Ok(Value::Null)
                })();
                Box::pin(std::future::ready(changed))
            }
            (calls::PUBLISHED, call) => self.published(call, &arguments),
            (calls::WORKSPACE_NODES, calls::ASK) => {
                let Some(nodes) = parts.workspace_nodes.clone() else {
                    return unserved("workspaces' nodes");
                };
                let asked = field::<String>(&arguments, "workspace").and_then(|workspace| {
                    let node = field::<String>(&arguments, "node")?;
                    let ask = seat::workspace_node_ask(&arguments["ask"])?;
                    nodes.ask(&workspace, &node, ask)
                });
                match asked {
                    Ok(answer) => {
                        Box::pin(async move { answer.await.ok_or("the workspace did not answer")? })
                    }
                    Err(why) => failed(why),
                }
            }
            (calls::SETTINGS, call) => {
                let Some(source) = parts.settings.clone() else {
                    return unserved("settings");
                };
                let (app, grants, providers) = match (
                    field::<String>(&arguments, "app"),
                    field::<Vec<String>>(&arguments, "grants"),
                    field::<bool>(&arguments, "providers"),
                ) {
                    (Ok(app), Ok(grants), Ok(providers)) => (app, grants, providers),
                    (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => return failed(why),
                };
                match call {
                    calls::READ => Box::pin(async move {
                        let after = field::<u64>(&arguments, "after")?;
                        let changed = Arc::new(tokio::sync::Notify::new());
                        let wake = Arc::clone(&changed);
                        let listener: Arc<settings_store::Listener> =
                            Arc::new(move || wake.notify_waiters());
                        source.changes().listen(&listener);
                        let waited = tokio::time::timeout(HELD, async {
                            loop {
                                let notified = changed.notified();
                                tokio::pin!(notified);
                                notified.as_mut().enable();
                                let view = settings_view(source.as_ref(), &app, &grants);
                                if view.revision != after {
                                    return view;
                                }
                                notified.await;
                            }
                        })
                        .await;
                        let view = waited
                            .unwrap_or_else(|_| settings_view(source.as_ref(), &app, &grants));
                        drop(listener);
                        answered(view)
                    }),
                    calls::UPDATE => blocking(move || {
                        let values = serde_json::to_vec(&arguments["values"])
                            .map_err(|error| error.to_string())?;
                        let action = arguments["action"].as_str();
                        // Model Providers' writes are held to the providers' own rules.
                        let check: Option<runtime_collab::settings::host::Check> =
                            providers.then(|| {
                                Arc::new(agent_provider_config::management::check_write)
                                    as runtime_collab::settings::host::Check
                            });
                        runtime_collab::settings::host::update(
                            source.as_ref(),
                            &app,
                            (&grants, check.as_ref()),
                            (&field::<String>(&arguments, "section")?, &values, action),
                        )?;
                        Ok(settings_view(source.as_ref(), &app, &grants))
                    }),
                    other => failed(format!("settings take no {other}")),
                }
            }
            (calls::DISPLAY_CONTROL, calls::COMMAND) => {
                let Some(display) = parts.display_control.clone() else {
                    return unserved("display control");
                };
                blocking(move || {
                    let context = field::<DisplayControlContext>(&arguments, "context")?;
                    let command = field::<Vec<u8>>(&arguments, "command")?;
                    display.command(&context, &command)
                })
            }
            (calls::LIVE_ACTIVITIES, calls::SHOW) => {
                let board = Arc::clone(&parts.live_activities);
                Box::pin(std::future::ready(field::<String>(&arguments, "app").map(
                    |app| {
                        let activity = Some(arguments["activity"].clone()).filter(Value::is_object);
                        board.show(&app, activity);
                        Value::Null
                    },
                )))
            }
            (calls::LIVE_ACTIVITIES, calls::CONTROLS) => {
                let board = Arc::clone(&parts.live_activities);
                Box::pin(async move {
                    let app = field::<String>(&arguments, "app")?;
                    let after = field::<u64>(&arguments, "after")?;
                    match tilo_failure::ignored!(
                        "a wait for the board that ran out answers as it stands",
                        tokio::time::timeout(HELD, board.controls(&app, after)).await
                    ) {
                        Ok(taken) => answered(taken),
                        Err(_elapsed) => Ok(Value::Null),
                    }
                })
            }
            (calls::LAUNCHES, calls::LAUNCH) => {
                let Some(launches) = parts.launches.clone() else {
                    return unserved("launches");
                };
                blocking(move || {
                    let app = field::<String>(&arguments, "app")?;
                    let workspace = field::<String>(&arguments, "workspace")?;
                    let record = field::<Option<String>>(&arguments, "record")?;
                    let place = seat::launch_place(&arguments["place"])?;
                    let launched = launches.launch_in(&app, &workspace, record.as_deref(), &place);
                    if let Err(error) = &launched {
                        tracing::warn!(%app, %error, "an app's launch did not start");
                    }
                    launched
                })
            }
            (calls::KEYBOARD_SHORTCUTS, calls::READ) => {
                let shortcuts = Arc::clone(&parts.keyboard_shortcuts);
                let settings = parts.settings.clone();
                Box::pin(async move {
                    let after = field::<u64>(&arguments, "after")?;
                    answered(shortcut_table(shortcuts, settings, after).await?)
                })
            }
            (calls::BROWSER_SERVICE_SESSIONS, call) => self.service_sessions(call, arguments),
            (calls::FILE_CLIPBOARD, calls::COMMAND) => {
                // Every file the call names is taken now, so none outlives it.
                let selected = seat::handed_files(&self.handed, &arguments);
                let Some(clipboard) = parts.file_clipboard.clone() else {
                    return unserved("Files clipboard");
                };
                Box::pin(async move {
                    let context = field::<seat::ClipboardFacts>(&arguments, "context")?;
                    let command = field(&arguments, "command")?;
                    answered(
                        tilo_workspace_service::apps::services::FileClipboardServices::command(
                            clipboard,
                            &context.context(selected?),
                            command,
                        )
                        .await?,
                    )
                })
            }
            (calls::FILE_CLIPBOARD, calls::DISCONNECT) => {
                if let (Some(clipboard), Ok(peer)) =
                    (&parts.file_clipboard, field::<u64>(&arguments, "peer"))
                {
                    tilo_workspace_service::apps::services::FileClipboardServices::disconnect(
                        clipboard.as_ref(),
                        peer,
                    );
                }
                Box::pin(std::future::ready(Ok(Value::Null)))
            }
            (calls::AGENTS, call) => {
                let Some(agents) = parts.agents.as_ref() else {
                    return unserved("agents");
                };
                match call {
                    calls::READ => Box::pin(async move {
                        let after = field::<u64>(&arguments, "after")?;
                        let agents = parts
                            .agents
                            .as_ref()
                            .ok_or("this session serves no agents")?;
                        answered(agents.read(after).await)
                    }),
                    calls::KEEP_ANSWER => {
                        let kept = field::<String>(&arguments, "session").and_then(|session| {
                            tilo_broker_core::services::AgentSessions::keep_answer(
                                agents.sessions.as_ref(),
                                &session,
                                field(&arguments, "grant")?,
                            );
                            Ok(Value::Null)
                        });
                        Box::pin(std::future::ready(kept))
                    }
                    other => failed(format!("agents take no {other}")),
                }
            }
            (service, call) => failed(format!("the seat serves no {service}#{call}")),
        }
    }

    /// A call of the person's account credentials, for the connection the daemon's broker admitted
    /// and checked: this session decides the rest against the workspace it keeps, then opens the
    /// vault.
    fn service_sessions(&self, call: &str, arguments: Value) -> Answer {
        let failed = |why: String| -> Answer { Box::pin(std::future::ready(Err(why))) };
        let Some(sessions) = self.parts.service_sessions.clone() else {
            return failed("this session keeps no account credentials".to_owned());
        };
        if call == calls::DISCONNECT {
            if let Ok(peer) = field::<u64>(&arguments, "peer") {
                sessions.disconnect(peer);
            }
            return Box::pin(std::future::ready(Ok(Value::Null)));
        }
        let (context, command) = match (
            field::<seat::ServiceSessionFacts>(&arguments, "context"),
            field::<runtime_collab::BrowserServiceSessionCommand>(&arguments, "command"),
        ) {
            (Ok(facts), Ok(command)) => (facts.context(), command),
            (Err(why), _) | (_, Err(why)) => return failed(why),
        };
        match call {
            calls::COMMAND => {
                let answer = sessions.command(&context, command);
                Box::pin(async move { answered(answer.await?) })
            }
            calls::ITEM_USE => {
                let answer = sessions.item_use(&context, command);
                Box::pin(async move { answered(answer.await?) })
            }
            calls::ANSWERED_LATER => Box::pin(async move {
                // What the workspace says of the person (whether they are still in it, whether
                // they administer it) is decided here, before the vault opens.
                let (sessions, context, command) = tokio::task::spawn_blocking(move || {
                    sessions
                        .authorize_command(&context, &command)
                        .map(|()| (sessions, context, command))
                })
                .await
                .map_err(|error| error.to_string())??;
                answered(sessions.answered_later(&context, command).await?)
            }),
            other => failed(format!("account credentials take no {other}")),
        }
    }

    /// A call of what the session's services publish.
    fn published(&self, call: &str, arguments: &Value) -> Answer {
        let published = Arc::clone(&self.parts.published);
        let caller = match (
            arguments["caller"]["app_id"].as_str(),
            arguments["caller"]["workspace"].as_str(),
        ) {
            (Some(app_id), Some(workspace)) => tilo_workspace_service::apps::services::Caller {
                app_id: app_id.to_owned(),
                workspace: workspace.to_owned(),
            },
            _ => {
                return Box::pin(std::future::ready(Err(
                    "the call names no caller".to_owned()
                )));
            }
        };
        let Some(resource) = arguments["resource"].as_str().map(str::to_owned) else {
            return Box::pin(std::future::ready(Err(
                "the call names no resource".to_owned()
            )));
        };
        let reading = |(revision, reading): (u64, Option<Vec<u8>>)| SeatReading {
            revision,
            reading: reading.and_then(|reading| {
                tilo_failure::ignored!(
                    "a reading that does not read shows nothing",
                    serde_json::from_slice(&reading).ok()
                )
            }),
        };
        match call {
            calls::SNAPSHOT => {
                let snapshot = published.snapshot(&caller, &resource);
                Box::pin(async move {
                    let read = snapshot
                        .await
                        .map_err(|_| "the session dropped the reading".to_owned())??;
                    answered(reading(read))
                })
            }
            calls::READ => {
                let after = arguments["after"].as_u64().unwrap_or(0);
                Box::pin(async move {
                    let (sink, mut readings) = tokio::sync::mpsc::unbounded_channel();
                    let watch = published.watch(&caller, &resource, sink)?;
                    let waited = tokio::time::timeout(HELD, async {
                        while let Some(event) = readings.recv().await {
                            if let runtime_collab::wire::Event::Reading {
                                revision,
                                reading: read,
                                ..
                            } = event
                                && revision != after
                            {
                                return Some((revision, read));
                            }
                        }
                        None
                    })
                    .await;
                    published.unwatch(watch);
                    match waited {
                        Ok(Some(read)) => answered(reading(read)),
                        Ok(None) => Err(format!("the session publishes `{resource}` no more")),
                        Err(_) => answered(SeatReading {
                            revision: after,
                            reading: None,
                        }),
                    }
                })
            }
            calls::INVOKE => {
                let action = arguments["action"].as_str().unwrap_or_default().to_owned();
                let invoked = serde_json::to_vec(&arguments["arguments"])
                    .map(|arguments| published.invoke(&caller, &resource, &action, arguments));
                Box::pin(async move {
                    let receipt = invoked
                        .map_err(|error| error.to_string())?
                        .await
                        .map_err(|_| "the session dropped the call".to_owned())?;
                    answered(receipt)
                })
            }
            other => Box::pin(std::future::ready(Err(format!(
                "published resources take no {other}"
            )))),
        }
    }
}

impl ActionProvider for SeatProvider {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::from_rows(
            String::new(),
            Vec::new(),
            &query,
        )))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let address = request.target.address;
        let arguments = match request.argument {
            InvokeArgument::Json(arguments) => Value::Object(arguments),
            _ => Value::Null,
        };
        let answer = self.answer(&address.resource, &address.action, arguments);
        let (done, receipt) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let status = match answer.await {
                Ok(value) => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        ..ActionOutput::default()
                    },
                },
                Err(message) => ReceiptStatus::Failed {
                    error: ActionFailure {
                        code: "seat".to_owned(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            };
            done.complete(Ok(InvocationReceipt { invocation, status }));
        });
        receipt
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use tilo_broker_core::services::{AgentRoster as _, AgentSessions as _, ListedAgent};
    use tilo_mcp::session_api::Consent;

    use super::*;

    /// The seat of a session holding `agents` and nothing else it could serve.
    fn holding(agents: SeatAgents) -> SeatParts {
        SeatParts {
            agents: Some(agents),
            ..serving_nothing()
        }
    }

    /// The seat of a session that serves nothing.
    fn serving_nothing() -> SeatParts {
        SeatParts {
            audio_output: None,
            native_media: None,
            continuations: crate::launches::Launches::default().notes(),
            records: None,
            published: crate::app_wire::Link::new(tilo_service::LoopWake::default().handle()),
            workspace_nodes: None,
            settings: None,
            display_control: None,
            live_activities: Arc::new(runtime_collab::LiveActivities::default()),
            launches: None,
            keyboard_shortcuts: crate::keyboard_shortcuts::servable(),
            service_sessions: None,
            file_clipboard: None,
            agents: None,
        }
    }

    /// A record whose change the daemon's store takes reaches the session's roles that follow
    /// records (the person's mentions, the activity), named by the document they read it as.
    #[test]
    fn a_record_the_daemon_takes_a_change_of_reaches_the_sessions_roles() {
        use tilo_resource_store::wire::{Command, Edit};

        use crate::references::testing::{NOTES, command, groceries, stamp};

        const KITCHEN: &str = "6d1c1f36-0000-4000-8000-0000000000b1";
        let daemon = crate::daemon_fixture::TestDaemon::writing(&[KITCHEN]);
        let changes = daemon.changes();
        let shell = daemon.shell_client();
        serve_on(
            shell.clone(),
            SeatParts {
                records: Some(crate::resource_index::RecordChanges::start(
                    &daemon.reads(),
                    changes.clone(),
                )),
                ..serving_nothing()
            },
        );
        {
            let _inside = tilo_rt::app().handle().enter();
            tilo_workspace_service::apps::seat::tell_record_changes(
                daemon.seat(),
                daemon.workspaces().document_writes(),
            );
        }
        tilo_test_env::wait_until("the session serves the seat", || daemon.seat().served());
        let followed = changes.reader();

        let note = groceries(&daemon, KITCHEN);
        command(
            &daemon,
            KITCHEN,
            NOTES,
            Command::Edit {
                kind: note.kind.clone(),
                id: note.record.clone(),
                edits: vec![Edit::Set {
                    field: "title".to_owned(),
                    value: serde_json::json!("Groceries and more"),
                }],
                stamp: stamp("add"),
            },
        )
        .expect("the note is retitled");
        let changed = tilo_test_env::wait_for("the session hears of the note changing", || {
            followed.try_iter().find(|changed| {
                changed.app == NOTES
                    && tilo_resource_store::content_resource(&changed.record)
                        == Some((note.kind.as_str(), note.record.as_str()))
            })
        });
        assert_eq!(changed.workspace, KITCHEN);
        assert!(!changed.replayed, "a change made now is news");
        drop(shell);
    }

    /// The daemon admits an agent and the turn it works in on what the person's session
    /// holds, over the shell link: the agents the workspace's record names as the session read it,
    /// and the sessions the person admitted at its MCP server. The person's "allow" for the rest
    /// of a session reaches the session, and a session that ended goes at the daemon too.
    #[test]
    fn the_daemon_admits_agents_on_what_the_session_holds_over_its_link() {
        const WORKSPACE: &str = "5c07c075-0000-4000-8000-0000000000a1";
        let daemon = crate::daemon_fixture::TestDaemon::start();
        let roster = Arc::new(crate::agents::Roster::default());
        let sessions = crate::agent_sessions::AgentSessions::new();
        // The session's link stays open while it serves the seat, as a running session holds it.
        let shell = daemon.shell_client();
        serve_on(
            shell.clone(),
            holding(SeatAgents {
                roster: Arc::clone(&roster),
                sessions: Arc::clone(&sessions),
            }),
        );
        let agents = {
            let _inside = tilo_rt::app().handle().enter();
            tilo_workspace_service::apps::seat::SeatAgents::follow(daemon.seat())
        };

        roster.list(
            WORKSPACE,
            ListedAgent {
                id: "scribe".to_owned(),
                name: "Scribe".to_owned(),
                avatar: None,
                acts_for: "ada".to_owned(),
            },
        );
        let session = sessions.joining(None);
        sessions.admit(
            &session,
            &Consent::Admit {
                workspace: WORKSPACE.to_owned(),
                agent: "scribe".to_owned(),
                person: "ada".to_owned(),
                name: "Claude Code".to_owned(),
            },
            "claude-code",
            "Studio",
        );
        tilo_test_env::wait_for("the daemon to read the session's agents", || {
            (agents.acts_for(WORKSPACE, "scribe").as_deref() == Some("ada")
                && agents
                    .going(WORKSPACE, "scribe", session.as_str())
                    .is_some())
            .then_some(())
        });
        assert!(
            agents
                .listed("5c07c075-0000-4000-8000-0000000000a2")
                .is_empty()
        );
        assert!(
            agents
                .going(WORKSPACE, "another", session.as_str())
                .is_none(),
            "a session is its own agent's only"
        );

        let node = tilo_desktop_model::Workspace::new(
            tilo_desktop_model::WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
            "Studio",
        )
        .policy
        .node();
        let allowed = tilo_action_policy::Grant::new(
            "session-answer:scribe",
            tilo_action_policy::Grantee::agent("scribe"),
            node,
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person("ada".to_owned()),
        );
        agents.keep_answer(session.as_str(), allowed.clone());
        tilo_test_env::wait_for("the person's answer to reach the session", || {
            sessions
                .going(WORKSPACE, "scribe", session.as_str())
                .filter(|going| going.answers == vec![allowed.clone()])
                .map(drop)
        });

        sessions.left(&session);
        tilo_test_env::wait_for("the ended session to go at the daemon", || {
            agents
                .going(WORKSPACE, "scribe", session.as_str())
                .is_none()
                .then_some(())
        });
        drop(shell);
    }
}
