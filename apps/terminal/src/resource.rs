//! Terminal as a record: where a session runs, the relay session it re-attaches to and the pane
//! presence is scoped to. What the process prints stays on the device that runs it.

use automerge::ObjId;
#[cfg(feature = "desktop")]
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceSurface, SurfaceCx,
};
use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{
    self, AbsolutePath, ActionCx, Atom, Bytes, Collection, Doc, Effect, Fields, Id, JsonSchema,
    Model, ModelError, Outbox, Refusal, Resource, resource,
};
use ui::widget::Element;

#[cfg(feature = "desktop")]
use crate::TerminalWidget;
#[cfg(feature = "desktop")]
use crate::screen::Screen;
use crate::target::SessionTarget;

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// A terminal session, as its resource holds it. Only the terminal running the session keeps these:
/// facts about the world outside the document that no undo takes back.
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
    /// The pane members' presence is scoped to; claimed by each
    /// process that takes the session.
    #[model(readonly)]
    pub pane: Option<Atom>,
    /// Device
    ///
    /// The device of the window the session is set up in: the one that
    /// recorded where it runs, or whose process last took it.
    #[model(readonly)]
    pub device: Option<Atom>,
    /// Holder
    ///
    /// Who that window is: the member who set the session up there.
    #[model(readonly)]
    pub holder: Option<ui::tilo_resource::MemberId>,
    /// Runs
    ///
    /// The commands the session ran, one item each, written when it ends.
    #[model(readonly)]
    pub commands: Collection<Command>,
    /// Commands asked to run
    ///
    /// The command lines `run` queued for the device that runs the session;
    /// only verbs queue them, and the terminal running it settles each with
    /// the run it became.
    #[model(readonly)]
    pub requests: Outbox<CommandRequest, CommandAnswer>,
}

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

/// One thing a parser found in a run's output: a diagnostic, a location, a link, a test, a changed
/// file.
#[derive(
    Model, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct CommandItem {
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

impl From<tilo_terminal::ItemKind> for ItemKind {
    fn from(kind: tilo_terminal::ItemKind) -> Self {
        match kind {
            tilo_terminal::ItemKind::Error => Self::Error,
            tilo_terminal::ItemKind::Warning => Self::Warning,
            tilo_terminal::ItemKind::Location => Self::Location,
            tilo_terminal::ItemKind::Url => Self::Url,
            tilo_terminal::ItemKind::TestPassed => Self::TestPassed,
            tilo_terminal::ItemKind::TestFailed => Self::TestFailed,
            tilo_terminal::ItemKind::TestIgnored => Self::TestIgnored,
            tilo_terminal::ItemKind::Change => Self::Change,
        }
    }
}

impl From<ItemKind> for tilo_terminal::ItemKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Error => Self::Error,
            ItemKind::Warning => Self::Warning,
            ItemKind::Location => Self::Location,
            ItemKind::Url => Self::Url,
            ItemKind::TestPassed => Self::TestPassed,
            ItemKind::TestFailed => Self::TestFailed,
            ItemKind::TestIgnored => Self::TestIgnored,
            ItemKind::Change => Self::Change,
        }
    }
}

impl From<tilo_terminal::Item> for CommandItem {
    fn from(item: tilo_terminal::Item) -> Self {
        Self {
            kind: ItemKind::from(item.kind),
            text: item.text,
            path: item.path,
            line: item.line,
            column: item.column,
            code: item.code,
        }
    }
}

impl From<CommandItem> for tilo_terminal::Item {
    fn from(item: CommandItem) -> Self {
        Self {
            kind: tilo_terminal::ItemKind::from(item.kind),
            text: item.text,
            path: item.path,
            line: item.line,
            column: item.column,
            code: item.code,
        }
    }
}

/// One command the session ran (docs/app-model/ush.md, "Commands").
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Command {
    /// Command
    #[model(label)]
    pub command: Atom,
    /// Working directory
    pub cwd: Atom,
    /// Where it ran: the session's encoded target.
    pub environment: Option<Atom>,
    /// Who ran it: the person at the keyboard, or whoever asked with `run`.
    pub actor: Option<ui::tilo_resource::MemberId>,
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
    /// It changed something a later action can put back, so the history
    /// can undo it ([`crate::undoability`]).
    pub undoable: bool,
    /// What its parsers found.
    pub items: Vec<CommandItem>,
}

/// A command line `run` asked the session to run.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct CommandRequest {
    /// Command
    pub command: Atom,
    /// Who asked.
    pub requested_by: Option<ui::tilo_resource::MemberId>,
    /// The person they asked for.
    pub for_person: Option<Atom>,
    /// The pane of the process asked to run it: only that process does, so
    /// a request outlives no restart to run unseen in a later one.
    pub pane: Atom,
    /// An agent asked: its run gets the shell's agent rendering.
    pub agent: bool,
}

/// How a requested command went: the id of the command it became.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandAnswer {
    pub id: Option<Id>,
    pub exit: Option<i32>,
    /// Why the command's host refused it, with the steps that would let it through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<ui::tilo_actions::remote::server_account::Refusal>,
}

/// What a finished command recorded, as the Terminal that ran it hands it in.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct FinishedCommand {
    pub command: String,
    pub cwd: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    /// The raw output, kept in the window's content store; the reference is what the resource names it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Bytes>,
    pub truncated: bool,
    pub tool: String,
    /// Whether what it changed can be undone later, read from the command when it ended
    /// ([`crate::undoability`]).
    pub undoable: bool,
    pub items: Vec<CommandItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Id>,
    /// The command's id: the request's, when `run` asked for it, so the answer names it.
    pub id: Id,
}

/// The longest command line `run` takes.
pub const MAX_COMMAND_BYTES: usize = 4 * 1024;

impl Terminal {
    pub fn session_target(&self) -> Option<SessionTarget> {
        self.target
            .as_ref()
            .and_then(|target| SessionTarget::decode(&target.0))
    }

    pub fn relay(&self) -> Option<&str> {
        self.relay_session
            .as_ref()
            .map(|session| session.0.as_str())
    }

    /// The pane presence is scoped to; empty until a process claims one.
    pub fn pane_id(&self) -> &str {
        self.pane.as_ref().map_or("", |pane| pane.0.as_str())
    }

    /// Whether the session belongs to a window other than one `me` opens on `device`: a window opened on
    /// it joins the session instead of starting a shell, and one `me` reopens there takes it back.
    pub fn runs_elsewhere(&self, device: &str, me: &ui::tilo_resource::MemberId) -> bool {
        let is =
            |field: &Option<Atom>, value: &str| field.as_ref().is_some_and(|atom| atom.0 == value);
        (self.pane.is_some() || self.target.is_some())
            && !(is(&self.device, device) && self.holder.as_ref() == Some(me))
    }

    fn set_up_on(&mut self, cx: &ActionCx, device: Option<String>) {
        self.device = device.filter(|device| !device.is_empty()).map(Atom);
        self.holder = cx.author().cloned();
    }

    /// The session stored under `obj` of another resource's document: a terminal nested in an editor's
    /// panel.
    ///
    /// # Errors
    /// [`ModelError`] when the object does not hold a session.
    pub fn read_under(doc: &Doc, obj: &ObjId) -> Result<Self, ModelError> {
        <Self as Fields>::read_fields(doc, obj)
    }

    /// Apply one of the session's actions to a session stored under `obj` of another resource's document.
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
            .map_err(ActionError::from)?;
        record.write_fields(doc, obj).map_err(refused)
    }
}

impl Terminal {
    /// What the resource holds, as JSON: an app with no window open reads a resource through this.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "terminal.session",
    params = session,
    effects = TerminalEffect,
    read_content = Terminal::read_document
)]
impl Terminal {
    #[action]
    pub fn run(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<CommandRequest, CommandAnswer>,
        #[param("The command line, as typed at the prompt")] command: String,
    ) -> Result<(), Refusal> {
        self.queue(cx, requests, &command)
    }

    #[action(keywords = "run again, repeat command", item = commands)]
    pub fn rerun(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<CommandRequest, CommandAnswer>,
        command: &Id,
    ) -> Result<(), Refusal> {
        let command = self
            .commands
            .get(command)
            .ok_or("no such command")?
            .command
            .0
            .clone();
        self.queue(cx, requests, &command)
    }

    /// Run one test of a command again, the way its tool runs one test.
    #[action(item = commands)]
    pub fn rerun_test(
        &mut self,
        cx: &mut ActionCx,
        requests: &mut Outbox<CommandRequest, CommandAnswer>,
        command: &Id,
        #[param("The test's name, as the command reported it")] test: String,
    ) -> Result<(), Refusal> {
        let run = self.commands.get(command).ok_or("no such command")?;
        if !run
            .items
            .iter()
            .any(|item| item.code.as_deref() == Some(test.as_str()))
        {
            return Err(Refusal::new("the command reported no such test"));
        }
        let command = crate::parse::rerun_test(&run.command.0, &test)
            .ok_or("this command's tool cannot run one test")?;
        self.queue(cx, requests, &command)
    }

    #[action(keywords = "stop, cancel, interrupt, ctrl c")]
    pub fn stop(&mut self) -> Result<(), Refusal> {
        Ok(())
    }

    /// Open the session's shell on a terminal for the caller's device, where the workspace's
    /// host runs its sessions: it answers the stream that device alone takes, and the
    /// session's id, to come back to it. It runs the session's commands as `run` does, and the
    /// host records each command line typed into it on the session, as `run` records its own.
    #[action(keywords = "open terminal, interactive shell, attach")]
    pub fn open_terminal(
        &mut self,
        requests: &mut Outbox<CommandRequest, CommandAnswer>,
        #[param("The window's width, in columns")] cols: Option<u16>,
        #[param("The window's height, in rows")] rows: Option<u16>,
        #[param("The session to come back to, by the id an open answered")] attach: Option<String>,
    ) -> Result<(), Refusal> {
        let _ = (requests, cols, rows, attach);
        Err(Refusal::new(
            "a session opens on a terminal only where the workspace's host runs it",
        ))
    }

    #[action(external)]
    pub fn record_command(
        &mut self,
        cx: &mut ActionCx,
        #[param("The command, as the session marked it", flatten)] finished: FinishedCommand,
    ) -> Result<(), Refusal> {
        let output = finished.output.unwrap_or_default();
        let asked = finished
            .request
            .as_ref()
            .and_then(|request| self.requests.get(request))
            .map(|entry| entry.value.clone());
        let (actor, for_person) = match asked {
            Some(asked) => (asked.requested_by, asked.for_person),
            None => (cx.author().cloned(), None),
        };
        let mut items = finished.items;
        items.truncate(crate::parse::MAX_ITEMS);
        if finished.id.as_str().is_empty() || self.commands.contains(&finished.id) {
            return Err(Refusal::new("a command takes an id of its own"));
        }
        let id = finished.id;
        self.commands.push(
            id,
            Command {
                command: Atom(finished.command),
                cwd: Atom(finished.cwd),
                environment: self.target.clone(),
                actor,
                for_person,
                started_ms: finished.started_ms,
                ended_ms: finished.ended_ms,
                exit: finished.exit,
                output,
                truncated: finished.truncated,
                tool: Atom(finished.tool),
                undoable: finished.undoable,
                items,
            },
        );
        Ok(())
    }

    /// Record where the session runs and the window it is set up in: on `device`, the action's author's.
    #[action(external)]
    pub fn record_target(
        &mut self,
        cx: &mut ActionCx,
        #[param("An encoded session target")] target: String,
        #[param("The device of the window that sets the session up")] device: Option<String>,
    ) -> Result<(), Refusal> {
        if SessionTarget::decode(&target).is_none() {
            return Err(Refusal::new("session target is invalid"));
        }
        self.target = Some(Atom(target));
        self.set_up_on(cx, device);
        Ok(())
    }

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

    #[action(external)]
    pub fn claim_pane(
        &mut self,
        cx: &mut ActionCx,
        #[param("The pane's id")] pane: String,
        #[param("The device the process runs on")] device: Option<String>,
    ) -> Result<(), Refusal> {
        if uuid::Uuid::parse_str(&pane).is_err() {
            return Err(Refusal::new("pane id is invalid"));
        }
        self.pane = Some(Atom(pane));
        self.set_up_on(cx, device);
        Ok(())
    }
}

impl Terminal {
    fn queue(
        &self,
        cx: &mut ActionCx,
        requests: &mut Outbox<CommandRequest, CommandAnswer>,
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
        let by = cx.invoked_by().filter(|by| !by.is_person());
        let request = CommandRequest {
            command: Atom(command.to_owned()),
            requested_by: cx.author().cloned(),
            for_person: by
                .as_ref()
                .and_then(|by| by.acted_for_person())
                .map(|person| Atom(person.to_owned())),
            pane,
            agent: by.as_ref().is_some_and(ui::tilo_actions::Acting::is_agent),
        };
        requests.push(cx.id(), request);
        Ok(())
    }
}

#[cfg(feature = "desktop")]
/// Keep explicit reads bounded even when retained output has very long rows; the suffix ends on a
/// Unicode boundary and nothing is logged before the caller asks for it.
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

/// A public web link the terminal printed: an item of the `terminal.link`
/// type, whose address names the pane and the link.
#[derive(ui::tilo_resource::Row)]
#[row(kind = "terminal.link", label = "Link", effects = TerminalEffect)]
pub(crate) struct Link;

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
    /// Read this member's actual process lifecycle and startup cause.
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
    /// Member-private native editor metadata; never the surrounding buffer.
    #[effect(returns, kind = "read")]
    InspectEditor,
    /// Replace editor selection
    ///
    /// Literally replace the original qualified editor selection without execution.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(destructive)]
    #[effect(keywords = "type text, enter command")]
    Send {
        #[param("Text to send")]
        text: String,
    },
    /// Interrupt process
    #[effect(kind = "outside")]
    #[effect(destructive)]
    #[effect(keywords = "stop, cancel, kill process, ctrl c")]
    Interrupt,
    /// Clear screen
    #[effect(kind = "outside")]
    #[effect(destructive)]
    #[effect(keywords = "cls, clear terminal, reset screen")]
    Clear,
    /// Restart shell
    ///
    /// Starts the session's shell again in place, after it ended.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    Restart,
    /// Paste
    ///
    /// Pastes the exact supplied text; clipboard contents are never re-read
    /// after confirmation.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    Paste {
        #[param("Exact text to paste after confirmation")]
        text: String,
    },
    /// Paste primary selection
    ///
    /// Pastes the exact supplied text; clipboard contents are never re-read
    /// after confirmation.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    PastePrimary {
        #[param("Exact text to paste after confirmation")]
        text: String,
    },
    /// Open in…
    ///
    /// Restarts this session in another environment and records it as the
    /// session's target.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    OpenIn {
        #[param(
            "Where the session runs: `environment` (the default environment), \
             `environment:<record>`, or `linux-vm:<vm>`; append `@<relay endpoint id>` to an \
             environment to run it on that relay. The device's own shell (`host`) can only \
             be chosen by the person."
        )]
        target: String,
    },
    /// New environment
    ///
    /// Creates an environment on the default runtime with nothing added.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    #[effect(keywords = "new container, new sandbox")]
    CreateEnvironment {
        #[param("What to call the new environment")]
        name: String,
    },
    /// Grant folder to environment
    ///
    /// Binds a host folder into the environment, read-write.
    #[effect(kind = "outside")]
    #[effect(destructive)]
    GrantFolder {
        #[param("The environment's resource")]
        record: String,
        #[param("An absolute folder path; it appears at the same path in the environment")]
        path: AbsolutePath,
    },
    /// Rename environment
    #[effect(kind = "outside")]
    #[effect(destructive)]
    RenameEnvironment {
        #[param("The environment's resource")]
        record: String,
        #[param("The new name")]
        name: String,
    },
    /// Copy visible output
    #[effect(destructive, kind = "read")]
    #[effect(keywords = "copy output, copy screen")]
    CopyVisible,
    /// Copy selected output
    #[effect(destructive, own_device, kind = "read")]
    CopySelection,
    /// Select all output
    #[effect(destructive, screen)]
    SelectAll,
    /// Show oldest output
    #[effect(screen)]
    HistoryTop,
    /// Return to live output
    #[effect(screen)]
    HistoryLive,
    /// Show item
    ///
    /// Shows one thing a run's parsers found: a link opens in the browser,
    /// a location or a diagnostic opens its file.
    #[effect(screen, row, item = commands)]
    OpenItem {
        #[param("The run")]
        run: String,
        #[param("Which of the run's items, counting from 0")]
        position: u32,
    },
    /// Open link
    ///
    /// Opens a public web link the process printed.
    #[effect(destructive, own_device, kind = "read", on = Link)]
    OpenLink {
        #[param("The link")]
        url: String,
    },
}

impl std::fmt::Debug for TerminalEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Effect arguments may be member-private transcripts or commands.
        formatter.write_str(<Self as ui::tilo_resource::Effect>::id(self))
    }
}

/// Framework app: the terminal widget is the surface over [`Terminal`].
pub struct TerminalShell;

impl ResourceApp for TerminalShell {
    type Resource = Terminal;
    const ID: &'static str = ui::apps::TERMINAL;
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Processes];
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Terminal";
    const TITLE_KEY: &'static str = "terminal-app-title";
    const ICON: Option<&'static str> = Some("terminal");

    fn label(_state: &Terminal) -> String {
        "Terminal session".to_owned()
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.workspace_record::<crate::history::CommandHistory>(crate::history::DOCUMENT);
        resources.workspace_record::<tilo_environment::machine_secrets::MachineSecrets>(
            tilo_environment::machine_secrets::DOCUMENT,
        );
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 720)
    }

    /// A page has no shell: it shows the session's commands, and sends what
    /// the person types to the shell the workspace's server keeps.
    #[cfg(not(feature = "desktop"))]
    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        crate::command_list::view(cx)
    }

    #[cfg(feature = "desktop")]
    #[allow(
        clippy::expect_used,
        reason = "a framework surface without an emulator cannot present a terminal"
    )]
    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        let lent = cx.take_lent::<crate::HostedProcessServices>();
        let deferred = cx.take_lent::<crate::HostedSessionSource>().is_some();
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
                        Ok(TerminalWidget::without_hosted_runtime(&error.to_string()))
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
        if cx.mode().is_live() {
            widget.show_in_window(hosted_window);
        }
        let queue = crate::commands::CommandQueue::default();
        widget.run_through(queue.clone());
        let owner = queue.clone();
        cx.outbox_awaited(
            Terminal::paths().requests,
            move |_, request: &CommandRequest| owner.owns(&request.pane.0),
            move |id, request, cx: &mut EffectCx<Terminal>| {
                queue.ask(crate::commands::Asked {
                    request: id.clone(),
                    command: request.command.0,
                    requested_by: request.requested_by,
                    agent: request.agent,
                    reply: cx.reply(),
                });
                Ok(Effected::Pending)
            },
        );
        let live = cx.live().clone();
        cx.on_open_item(move |run| {
            if live.get().commands.contains(run) {
                Ok(())
            } else {
                Err(Refusal::new("the session has no such command"))
            }
        });
        cx.surface(crate::TerminalView::new(widget))
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }
}

#[cfg(feature = "desktop")]
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

    fn take_process_intent(&mut self) -> Option<ui::widget::AppProcessIntent> {
        self.take_session_intent()
    }

    fn present(&mut self, state: &std::rc::Rc<Terminal>) {
        self.present_session(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Terminal>) {
        self.wake_through(cx.waker());
        self.bind(cx.dispatcher());
        if let Some(record) = cx.document_id() {
            self.show_record(record.route());
        }
        if let (Some(device), Some(me)) = (cx.device(), cx.me()) {
            self.runs_on(device, me);
        }
        self.set_run_action(Terminal::RUN.on(cx.actions().resource_id()));
        if cx.mode().is_live()
            && let (Some(history), Some(record), Some(me)) = (
                cx.shared_resource::<crate::history::CommandHistory>(
                    ui::apps::TERMINAL,
                    crate::history::DOCUMENT,
                ),
                cx.document_id(),
                cx.me(),
            )
        {
            self.remember_in(history, record.uri(), me.clone());
        }
        if cx.mode().is_live() {
            self.recent_in(crate::target::RecentTarget::open(&cx));
        }
    }

    #[cfg(feature = "samples")]
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

#[cfg(feature = "desktop")]
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

    fn take_process_intent(&mut self) -> Option<ui::widget::AppProcessIntent> {
        self.take_session_intent()
    }
    fn present(&mut self, state: &std::rc::Rc<Terminal>) {
        self.present_session(state);
    }
    fn attach(&mut self, cx: SurfaceCx<Terminal>) {
        self.wake_through(cx.waker());
        self.bind(cx.dispatcher());
        if let Some(record) = cx.document_id() {
            self.terminal_mut().show_record(record.route());
        }
        if let (Some(device), Some(me)) = (cx.device(), cx.me()) {
            self.runs_on(device, me);
        }
        self.set_run_action(Terminal::RUN.on(cx.actions().resource_id()));
        if cx.mode().is_live()
            && let (Some(history), Some(record), Some(me)) = (
                cx.shared_resource::<crate::history::CommandHistory>(
                    ui::apps::TERMINAL,
                    crate::history::DOCUMENT,
                ),
                cx.document_id(),
                cx.me(),
            )
        {
            self.remember_in(history, record.uri(), me.clone());
        }
        if cx.mode().is_live() {
            self.recent_in(crate::target::RecentTarget::open(&cx));
        }
    }
    #[cfg(feature = "samples")]
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

ui::reach_tests!(TerminalShell);
