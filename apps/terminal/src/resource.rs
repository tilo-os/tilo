//! Terminal as a record: where a session runs, the relay session it
//! re-attaches to and the pane collaborators' presence is scoped to.
//!
//! What the process prints (its screen, scrollback and title) is live
//! output of a process on one device, like the pixels of a legacy window,
//! and stays out of the document: recording it cost one Automerge
//! operation per character and one version per output batch, kept forever
//! (20,000 lines of a build log: 3.1 million operations and 403 versions,
//! 27 times the emulator's own work). Another device reaches a running
//! session by re-attaching to it on its relay; agents read the screen
//! through the view that runs it ([`TerminalEffect::ReadScreen`]).

use automerge::ObjId;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{
    self, ActionCx, Atom, Bytes, Collection, Doc, Effect, Fields, Id, JsonSchema, Model,
    ModelError, Outbox, Refusal, Resource, resource,
};
use ui::widget::Element;

use crate::TerminalWidget;
use crate::screen::Screen;
use crate::target::SessionTarget;

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// A terminal session, as its record holds it. Only the terminal running
/// the session records these, as facts about the world outside the
/// document that no undo takes back.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Terminal {
    /// Where the session runs
    ///
    /// An encoded [`SessionTarget`]. It outlives every process that runs
    /// in it, so reopening goes back to it.
    #[model(readonly)]
    pub target: Option<Atom>,
    /// Relay session
    ///
    /// The session on a relay this session re-attaches to.
    #[model(readonly)]
    pub relay_session: Option<Atom>,
    /// Pane
    ///
    /// The pane collaborators' presence is scoped to; claimed by each
    /// process that takes the session.
    #[model(readonly)]
    pub pane: Option<Atom>,
    /// Runs
    ///
    /// The commands the session ran, one item each, written when it ends.
    #[model(readonly)]
    pub runs: Collection<Run>,
    /// Commands asked to run
    ///
    /// The command lines `run` queued for the device that runs the session;
    /// only verbs queue them, and the terminal running it settles each with
    /// the run it became.
    #[model(readonly)]
    pub requests: Outbox<RunRequest, RunAnswer>,
}

/// What a part of a run's output is.
#[derive(
    Model,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    #[default]
    #[model(rename = "error")]
    Error,
    #[model(rename = "warning")]
    Warning,
    #[model(rename = "location")]
    Location,
    #[model(rename = "url")]
    Url,
    #[model(rename = "test-passed")]
    TestPassed,
    #[model(rename = "test-failed")]
    TestFailed,
    #[model(rename = "test-ignored")]
    TestIgnored,
    #[model(rename = "change")]
    Change,
}

/// One thing a parser found in a run's output: a diagnostic, a location, a
/// link, a test, a changed file.
#[derive(
    Model, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct RunItem {
    /// What it is.
    pub kind: ItemKind,
    /// What the output says about it.
    pub text: String,
    /// The file it is in, or the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// A diagnostic's code, a test's name, a change's status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// One command the session ran (docs/app-model/ush.md, "A run").
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Run {
    /// Command
    #[model(label)]
    pub command: Atom,
    /// Working directory
    pub cwd: Atom,
    /// Where it ran: the session's encoded target.
    pub environment: Option<Atom>,
    /// Who ran it: the person at the keyboard, or whoever asked with `run`.
    pub actor: Atom,
    /// The person an agent ran it for.
    pub for_person: Option<Atom>,
    /// Started, in milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// Ended, in milliseconds since the Unix epoch.
    pub ended_ms: i64,
    /// Exit status
    pub exit: i32,
    /// Everything it printed, escape sequences included.
    pub output: Bytes,
    /// It printed more than the output keeps.
    pub truncated: bool,
    /// The tool whose parser read it.
    pub tool: Atom,
    /// What its parsers found.
    pub items: Vec<RunItem>,
}

/// A command line `run` asked the session to run.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct RunRequest {
    /// Command
    pub command: Atom,
    /// Who asked.
    pub requested_by: Atom,
    /// The person they asked for.
    pub for_person: Option<Atom>,
    /// The pane of the process asked to run it: only that process does, so
    /// a request outlives no restart to run unseen in a later one.
    pub pane: Atom,
}

/// How a requested run went: the run it became.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunAnswer {
    pub run: Option<Id>,
    pub exit: Option<i32>,
}

/// What a finished run recorded, as the Terminal that ran it hands it in.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct Finished {
    pub command: String,
    pub cwd: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    /// The content hash of the raw output (lower-case hex), when it printed
    /// anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub output_len: u64,
    pub truncated: bool,
    pub tool: String,
    pub items: Vec<RunItem>,
    /// The request it carried out, if `run` asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Id>,
    /// The id the run takes: the request's when one asked for it, so its
    /// answer names the run.
    pub run: Id,
}

/// The longest command line `run` takes.
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

impl Terminal {
    /// Where the session runs, once a session recorded it.
    pub fn session_target(&self) -> Option<SessionTarget> {
        self.target
            .as_ref()
            .and_then(|target| SessionTarget::decode(&target.0))
    }

    /// The relay session to re-attach to.
    pub fn relay(&self) -> Option<&str> {
        self.relay_session
            .as_ref()
            .map(|session| session.0.as_str())
    }

    /// The pane presence is scoped to; empty until a process claims one.
    pub fn pane_id(&self) -> &str {
        self.pane.as_ref().map_or("", |pane| pane.0.as_str())
    }

    /// The session stored under `obj` of another record's document: a
    /// terminal nested in an editor's panel.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the object does not hold a session.
    pub fn read_under(doc: &Doc, obj: &ObjId) -> Result<Self, ModelError> {
        <Self as Fields>::read_fields(doc, obj)
    }

    /// Apply one of the session's actions to a session stored under `obj`
    /// of another record's document.
    ///
    /// # Errors
    /// The action's refusal; nothing is written then.
    pub fn apply_under(
        action: &<Self as Resource>::Action,
        doc: &mut Doc,
        obj: &ObjId,
    ) -> Result<(), ActionError> {
        let refused = |error: ModelError| ActionError::Refused(error.to_string());
        let payload = serde_json::to_vec(action).map_err(ActionError::Encode)?;
        let mut cx = ActionCx::for_apply(doc, &payload, action.now_ms);
        if let Some(edit) = tilo_resource::Operation::edit(&action.operation) {
            return <Self as Fields>::edit_fields(doc, obj, edit.path(), edit.op(), &mut cx)
                .map_err(refused);
        }
        let mut record = Self::read_under(doc, obj).map_err(refused)?;
        tilo_resource::Operation::perform(&action.operation, &mut record, &mut cx)
            .map_err(|refusal| ActionError::Refused(refusal.0))?;
        record.write_fields(doc, obj).map_err(refused)
    }
}

#[resource(
    kind = "terminal.session",
    label = "Terminal",
    params = session,
    effects = TerminalEffect
)]
impl Terminal {
    /// Run a command line in the session.
    ///
    /// Types it into the session's shell, on the device that runs the
    /// session, and answers with the run once it ends.
    #[action]
    pub fn run(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<RunRequest, RunAnswer>,
        #[param("The command line, as typed at the prompt")] command: String,
    ) -> Result<(), Refusal> {
        self.queue(cx, requests, &command)
    }

    /// Run a run's command line again.
    #[action(item = runs)]
    pub fn rerun(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<RunRequest, RunAnswer>,
        runs: &Id,
    ) -> Result<(), Refusal> {
        let command = self.runs.get(runs).ok_or("no such run")?.command.0.clone();
        self.queue(cx, requests, &command)
    }

    /// Run one test of a run again, the way its tool runs one test.
    #[action(item = runs)]
    pub fn rerun_test(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<RunRequest, RunAnswer>,
        runs: &Id,
        #[param("The test's name, as the run reported it")] test: String,
    ) -> Result<(), Refusal> {
        let run = self.runs.get(runs).ok_or("no such run")?;
        if !run
            .items
            .iter()
            .any(|item| item.code.as_deref() == Some(test.as_str()))
        {
            return Err(Refusal::new("the run reported no such test"));
        }
        let command = crate::parse::rerun_test(&run.command.0, &test)
            .ok_or("this run's tool cannot run one test")?;
        self.queue(cx, requests, &command)
    }

    /// Record a run that ended.
    #[action(external)]
    pub fn record_run(
        &mut self,
        cx: &mut ActionCx,
        #[param("The run, as the session marked it", flatten)] run: Finished,
    ) -> Result<(), Refusal> {
        let output = match &run.output {
            Some(hash) => Bytes::Blob {
                hash: content_hash(hash).ok_or("the output is not a content hash")?,
                len: run.output_len,
            },
            None => Bytes::Empty,
        };
        let asked = run
            .request
            .as_ref()
            .and_then(|request| self.requests.get(request))
            .map(|entry| entry.value.clone());
        let (actor, for_person) = match asked {
            Some(asked) => (asked.requested_by, asked.for_person),
            None => (Atom(cx.author().unwrap_or_default().to_owned()), None),
        };
        let mut items = run.items;
        items.truncate(crate::parse::MAX_ITEMS);
        if run.run.as_str().is_empty() || self.runs.contains(&run.run) {
            return Err(Refusal::new("a run takes an id of its own"));
        }
        let id = run.run;
        self.runs.push(
            id,
            Run {
                command: Atom(run.command),
                cwd: Atom(run.cwd),
                environment: self.target.clone(),
                actor,
                for_person,
                started_ms: run.started_ms,
                ended_ms: run.ended_ms,
                exit: run.exit,
                output,
                truncated: run.truncated,
                tool: Atom(run.tool),
                items,
            },
        );
        Ok(())
    }

    /// Record where the session runs.
    #[action(external)]
    pub fn record_target(
        &mut self,
        #[param("An encoded session target")] target: String,
    ) -> Result<(), Refusal> {
        if SessionTarget::decode(&target).is_none() {
            return Err(Refusal::new("session target is invalid"));
        }
        self.target = Some(Atom(target));
        Ok(())
    }

    /// Record the relay session this session re-attaches to, or forget it.
    #[action(external)]
    pub fn record_relay_session(
        &mut self,
        #[param("The relay session's id; none to forget it")] session: Option<String>,
    ) -> Result<(), Refusal> {
        self.relay_session = match session {
            Some(session)
                if session.len() == 32 && session.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                Some(Atom(session))
            }
            Some(_) => return Err(Refusal::new("relay session id is invalid")),
            None => None,
        };
        Ok(())
    }

    /// Scope presence to a new process's pane.
    #[action(external)]
    pub fn claim_pane(&mut self, #[param("The pane's id")] pane: String) -> Result<(), Refusal> {
        if uuid::Uuid::parse_str(&pane).is_err() {
            return Err(Refusal::new("pane id is invalid"));
        }
        self.pane = Some(Atom(pane));
        Ok(())
    }
}

impl Terminal {
    /// Queue `command` for the process that runs the session.
    fn queue(
        &self,
        cx: &mut ActionCx,
        requests: &mut Outbox<RunRequest, RunAnswer>,
        command: &str,
    ) -> Result<(), Refusal> {
        let command = command.trim();
        if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
            return Err(Refusal::new("a command line of 1 to 4096 bytes"));
        }
        if command.chars().any(char::is_control) {
            return Err(Refusal::new("one command line, without control characters"));
        }
        let pane = self
            .pane
            .clone()
            .ok_or("the session is not running anywhere yet")?;
        let request = RunRequest {
            command: Atom(command.to_owned()),
            requested_by: Atom(cx.author().unwrap_or_default().to_owned()),
            for_person: cx.for_person().map(Atom),
            pane,
        };
        requests.push(cx.id(), request);
        Ok(())
    }
}

/// The 32 bytes a lower-case hex content hash names.
fn content_hash(hex: &str) -> Option<[u8; 32]> {
    if !tilo_content::valid_hash(hex) {
        return None;
    }
    let mut out = [0_u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Keep explicit reads bounded even when retained output contains very long
/// rows. The suffix ends at a Unicode scalar boundary; no terminal content is
/// logged or copied until the caller asks for it.
pub(crate) fn readable_output(screen: &Screen) -> (String, bool) {
    const LIMIT: usize = 64 * 1024;
    let mut reversed = String::new();
    let mut rows = screen.active().iter().rev().peekable();
    while let Some(row) = rows.next() {
        for character in row.text.trim_end().chars().rev() {
            if reversed.len() + character.len_utf8() > LIMIT {
                return (reversed.chars().rev().collect(), true);
            }
            reversed.push(character);
        }
        if rows.peek().is_some() {
            if reversed.len() == LIMIT {
                return (reversed.chars().rev().collect(), true);
            }
            reversed.push('\n');
        }
    }
    (reversed.chars().rev().collect(), false)
}

/// What a terminal does that changes no document: drive its process, move
/// the session to another environment, manage environments, copy and scroll
/// its output, open a link it printed. Every effect that reaches the
/// process or the clipboard asks for confirmation when the palette, an agent
/// or a script invokes it; a person's own keystrokes and gestures reach the
/// process directly.
#[derive(Clone, PartialEq, Eq, Effect)]
#[effect(operation = "terminal.effects")]
pub enum TerminalEffect {
    /// Inspect session
    ///
    /// Read this participant's actual process lifecycle and startup cause.
    /// What the session shows is read with [`Self::ReadScreen`].
    #[effect(returns, kind = "read")]
    InspectSession,
    /// Read output
    ///
    /// Read what the session shows: its output (the newest 64 KiB of the
    /// screen and scrollback), title, cursor and where it runs.
    #[effect(returns, kind = "read")]
    ReadScreen,
    /// Inspect editable field
    ///
    /// Participant-private native editor metadata; never the surrounding buffer.
    #[effect(returns, kind = "read")]
    InspectEditor,
    /// Replace editor selection
    ///
    /// Literally replace the original qualified editor selection without execution.
    #[effect(job, destructive)]
    ReplaceEditorSelection {
        #[param("Original local terminal session")]
        session: String,
        #[param("Exact lease returned by Inspect editable field")]
        lease: String,
        #[param(secret, "Literal replacement, without control characters")]
        text: String,
    },
    /// Send text
    ///
    /// Writes reviewed text without pressing Enter.
    #[effect(destructive)]
    Send {
        #[param("Text to send")]
        text: String,
    },
    /// Interrupt process
    #[effect(destructive)]
    Interrupt,
    /// Clear screen
    #[effect(destructive)]
    Clear,
    /// Paste
    ///
    /// Pastes the exact supplied text; clipboard contents are never re-read
    /// after confirmation.
    #[effect(destructive)]
    Paste {
        #[param("Exact text to paste after confirmation")]
        text: String,
    },
    /// Paste primary selection
    ///
    /// Pastes the exact supplied text; clipboard contents are never re-read
    /// after confirmation.
    #[effect(destructive)]
    PastePrimary {
        #[param("Exact text to paste after confirmation")]
        text: String,
    },
    /// Open in…
    ///
    /// Restarts this session in another environment and records it as the
    /// session's target.
    #[effect(destructive)]
    OpenIn {
        #[param(
            "Where the session runs: `environment` (the default environment), \
             `environment:<record>`, or `legacy:<vm>`; append `@<relay endpoint id>` to an \
             environment to run it on that relay. The device's own shell (`host`) can only \
             be chosen by the person."
        )]
        target: String,
    },
    /// New environment
    ///
    /// Creates an environment on the default runtime with nothing added.
    #[effect(destructive)]
    CreateEnvironment {
        #[param("What to call the new environment")]
        name: String,
    },
    /// Grant folder to environment
    ///
    /// Binds a host folder into the environment, read-write.
    #[effect(destructive)]
    GrantFolder {
        #[param("The environment's record")]
        record: String,
        #[param("An absolute folder path; it appears at the same path in the environment")]
        path: String,
    },
    /// Rename environment
    #[effect(destructive)]
    RenameEnvironment {
        #[param("The environment's record")]
        record: String,
        #[param("The new name")]
        name: String,
    },
    /// Copy visible output
    #[effect(destructive, kind = "read")]
    CopyVisible,
    /// Copy selected output
    #[effect(destructive, kind = "read")]
    CopySelection,
    /// Select all output
    #[effect(destructive, kind = "read")]
    SelectAll,
    /// Show oldest output
    #[effect(kind = "read")]
    HistoryTop,
    /// Return to live output
    #[effect(kind = "read")]
    HistoryLive,
    /// Show item
    ///
    /// Shows one thing a run's parsers found: a link opens in the browser,
    /// a location or a diagnostic opens its file.
    #[effect(row, item = runs)]
    OpenItem {
        #[param("The run")]
        run: String,
        #[param("Which of the run's items, counting from 0")]
        position: u32,
    },
    /// Open link
    ///
    /// Opens a public web link the process printed.
    #[effect(destructive, kind = "read")]
    OpenLink {
        #[param("The link")]
        url: String,
    },
}

impl std::fmt::Debug for TerminalEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Effect arguments may be participant-private transcripts or commands.
        formatter.write_str(<Self as ui::tilo_resource::Effect>::id(self))
    }
}

/// Framework app: the terminal widget is the surface over [`Terminal`].
pub struct TerminalShell;

impl ResourceApp for TerminalShell {
    type Resource = Terminal;
    const ID: &'static str = "so.tilo.terminal";
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Processes];
    // Each window is a session of its own, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Terminal";
    const ICON: Option<&'static str> = Some("terminal");

    fn label(_state: &Terminal) -> String {
        "Terminal session".to_owned()
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn records(records: &mut ui::framework::Records<'_>) {
        records.person::<crate::history::CommandHistory>();
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 720)
    }

    #[allow(
        clippy::expect_used,
        reason = "a framework surface without an emulator cannot present a terminal"
    )]
    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        // A window someone may type in runs its session; a past version, a
        // screenshot, a participant who may only view, and this crate's own
        // tests present a terminal with no process behind it.
        // Mounted in the shell, sessions open through the supervisor
        // service permission or connection the shell lent; in its own process, locally.
        let lent = cx.take_lent::<crate::HostedProcessServices>();
        let deferred = cx.take_lent::<crate::HostedSessionSource>().is_some();
        // The window the shell shows a tab of it in, in the shell's process.
        let hosted_window = cx.take_lent::<ui::hosted_window::HostedWindow>();
        let widget = if cx.read_only() {
            TerminalWidget::spawn_headless()
        } else if deferred {
            Ok(TerminalWidget::spawn_hosted_deferred())
        } else if let Some(services) = lent {
            TerminalWidget::spawn_hosted(services)
                .map_err(|error| anyhow::anyhow!(error.to_string()))
                .or_else(|error| {
                    if cx.mode().is_live() {
                        Ok(TerminalWidget::without_hosted_runtime(error.to_string()))
                    } else {
                        TerminalWidget::spawn_headless()
                    }
                })
        } else if cx.mode().is_live() && !cfg!(test) {
            TerminalWidget::spawn_default().or_else(|_| TerminalWidget::spawn_headless())
        } else {
            TerminalWidget::spawn_headless()
        }
        .expect("terminal surface requires an emulator");
        let mut widget = match cx.content() {
            Some(content) => widget.with_content(content),
            None => widget,
        };
        // A live terminal is the app of its window: the window's title says
        // where its shell runs, and the window closes when the shell exits.
        // A past version shows a terminal, not a window of one.
        if cx.mode().is_live() {
            widget.show_in_window(hosted_window);
        }
        let queue = crate::runs::RunQueue::default();
        widget.run_through(queue.clone());
        let owner = queue.clone();
        cx.outbox_awaited(
            Terminal::paths().requests,
            move |_, request: &RunRequest| owner.runs(&request.pane.0),
            move |id, request, cx: &mut EffectCx<Terminal>| {
                queue.ask(crate::runs::Asked {
                    request: id.clone(),
                    command: request.command.0,
                    requested_by: request.requested_by.0,
                    reply: cx.reply(),
                });
                Ok(Effected::Pending)
            },
        );
        // A run is shown by the session that ran it: its view is this one.
        let live = cx.live().clone();
        cx.on_open_item(move |run| {
            if live.get().runs.contains(run) {
                Ok(())
            } else {
                Err(Refusal::new("the session has no such run"))
            }
        });
        cx.surface(crate::TerminalView::new(widget))
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }
}

impl ResourceSurface<Terminal, TerminalEffect> for TerminalWidget {
    fn read_only_changed(&mut self, read_only: bool) {
        self.native_editor_read_only_changed(read_only);
    }
    fn supports_device_shell(&self) -> bool {
        true
    }
    fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        TerminalWidget::device_shell_operation(self, operation)
    }

    fn request_device_shell(&mut self) -> bool {
        TerminalWidget::request_device_shell(self)
    }

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.take_session_intent()
    }

    fn present(&mut self, state: &std::rc::Rc<Terminal>) {
        self.present_session(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Terminal>) {
        self.wake_through(cx.waker());
        self.bind(cx.dispatcher());
        if cx.mode().is_live()
            && let (Some(history), Some(record)) =
                (cx.person::<crate::history::CommandHistory>(), cx.record())
        {
            self.remember_in(
                history,
                record.uri(),
                cx.me().unwrap_or_default().to_owned(),
            );
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(name) = scenario {
            self.stage_scenario(name);
        }
    }

    fn effect(
        &mut self,
        effect: TerminalEffect,
        cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        match effect {
            TerminalEffect::InspectSession => Ok(Effected::value(self.inspect_session())),
            TerminalEffect::ReadScreen => Ok(Effected::value(self.read_session())),
            TerminalEffect::InspectEditor => Ok(Effected::value(self.inspect_editor())),
            TerminalEffect::ReplaceEditorSelection {
                session,
                lease,
                text,
            } => {
                self.replace_editor_selection(&session, &lease, text, cx.reply())?;
                Ok(Effected::Pending)
            }
            effect => self.perform(effect).map(|()| Effected::done()),
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.link_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.link_row(id)
    }
}

impl ResourceSurface<Terminal, TerminalEffect> for crate::TerminalView {
    fn read_only_changed(&mut self, read_only: bool) {
        self.terminal_mut()
            .native_editor_read_only_changed(read_only);
    }
    fn supports_device_shell(&self) -> bool {
        true
    }

    fn device_shell_operation(
        &mut self,
        operation: &ui::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        crate::TerminalWidget::device_shell_operation(self.terminal_mut(), operation)
    }

    fn request_device_shell(&mut self) -> bool {
        crate::TerminalWidget::request_device_shell(self.terminal_mut())
    }

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.take_session_intent()
    }
    fn present(&mut self, state: &std::rc::Rc<Terminal>) {
        self.present_session(state);
    }
    fn attach(&mut self, cx: SurfaceCx<Terminal>) {
        self.wake_through(cx.waker());
        self.bind(cx.dispatcher());
        if cx.mode().is_live()
            && let (Some(history), Some(record)) =
                (cx.person::<crate::history::CommandHistory>(), cx.record())
        {
            self.remember_in(
                history,
                record.uri(),
                cx.me().unwrap_or_default().to_owned(),
            );
        }
    }
    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(name) = scenario {
            self.stage_scenario(name);
        }
        if scenario == Some("find") {
            self.set_find_query("build step 042");
        }
    }
    fn effect(
        &mut self,
        effect: TerminalEffect,
        cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        match effect {
            TerminalEffect::InspectSession => Ok(Effected::value(self.inspect_session())),
            TerminalEffect::ReadScreen => Ok(Effected::value(self.read_session())),
            TerminalEffect::InspectEditor => {
                Ok(Effected::value(self.terminal_mut().inspect_editor()))
            }
            TerminalEffect::ReplaceEditorSelection {
                session,
                lease,
                text,
            } => {
                self.terminal_mut()
                    .replace_editor_selection(&session, &lease, text, cx.reply())?;
                Ok(Effected::Pending)
            }
            effect => self.perform(effect).map(|()| Effected::done()),
        }
    }
    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        self.link_rows()
    }
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.link_row(id)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::TerminalShell::scenarios() {
            let mut app = TestApp::<super::TerminalShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
