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
//! through the view that runs it ([`TerminalEffect::Read`]).

use automerge::ObjId;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{
    self, ActionCx, Atom, Doc, Effect, Fields, Model, ModelError, Refusal, Resource, resource,
};
use ui::widget::Element;

use crate::TerminalWidget;
use crate::screen::Screen;
use crate::target::SessionTarget;

/// The terminal's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "terminal-record-target").with_system_locale();

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
}

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

#[resource(kind = "terminal.session", label = "Terminal")]
impl Terminal {
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
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "terminal.effects")]
pub enum TerminalEffect {
    /// Inspect session
    ///
    /// Read this participant's actual process lifecycle and startup cause.
    /// What the session shows is read with [`Self::Read`].
    #[effect(returns, permission = "read")]
    InspectSession,
    /// Read output
    ///
    /// Read what the session shows: its output (the newest 64 KiB of the
    /// screen and scrollback), title, cursor and where it runs.
    #[effect(returns, permission = "read")]
    Read,
    /// Send text
    ///
    /// Writes reviewed text without pressing Enter.
    #[effect(destructive)]
    Send {
        #[param("Text to send")]
        text: String,
    },
    /// Run command
    ///
    /// Runs one reviewed command in this live process.
    #[effect(destructive)]
    Run {
        #[param("Command to run")]
        command: String,
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
    #[effect(destructive, permission = "read")]
    CopyVisible,
    /// Copy selected output
    #[effect(destructive, permission = "read")]
    CopySelection,
    /// Select all output
    #[effect(destructive, permission = "read")]
    SelectAll,
    /// Show oldest output
    #[effect(permission = "read")]
    HistoryTop,
    /// Return to live output
    #[effect(permission = "read")]
    HistoryLive,
    /// Open link
    ///
    /// Opens a public web link the process printed.
    #[effect(destructive, permission = "read")]
    OpenLink {
        #[param("The link")]
        url: String,
    },
}

/// Framework app: the terminal widget is the surface over [`Terminal`].
pub struct TerminalShell;

impl ResourceApp for TerminalShell {
    type Resource = Terminal;
    const ID: &'static str = "so.tilo.terminal";
    const TITLE: &'static str = "Terminal";
    const ICON: Option<&'static str> = Some("terminal");

    fn label(_state: &Terminal) -> String {
        "Terminal session".to_owned()
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
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
        // connection the shell lent; in its own process, locally.
        let lent = cx.take_lent::<crate::HostedProcessServices>();
        let widget = if cx.read_only() {
            TerminalWidget::spawn_headless()
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
        let widget = match cx.content() {
            Some(content) => widget.with_content(content),
            None => widget,
        };
        cx.surface(crate::TerminalView::new(widget))
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }
}

impl ResourceSurface<Terminal, TerminalEffect> for TerminalWidget {
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
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(name) = scenario {
            self.stage_scenario(name);
        }
    }

    fn effect(
        &mut self,
        effect: TerminalEffect,
        _cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        match effect {
            TerminalEffect::InspectSession => Ok(Effected::value(self.inspect_session())),
            TerminalEffect::Read => Ok(Effected::value(self.read_session())),
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
        _cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        match effect {
            TerminalEffect::InspectSession => Ok(Effected::value(self.inspect_session())),
            TerminalEffect::Read => Ok(Effected::value(self.read_session())),
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
