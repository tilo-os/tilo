//! Terminal as a [`tilo_resource::Resource`]: a session record holds what the
//! session shows — its title, where it runs, and the screen of its process —
//! so every collaborator and every past version reads it. The process itself
//! is the surface's; its output reaches the record as one action per batch.

use automerge::transaction::Transactable;
use automerge::{ObjId, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ChangeKind};
use ui::tilo_resource::{self, Delta, Doc, Effect, ModelError, Resource};
use ui::widget::Element;

use crate::TerminalWidget;
use crate::screen::{Screen, ScreenEdit, ScreenObjects};

const TERMINAL_ACTION_TYPE: &str = "terminal.action";

const TITLE: &str = "terminal_title";
const PANE: &str = "terminal_pane_id";
const LOCAL_SESSION: &str = "terminal_local_session";

/// What a terminal session shows, as its record holds it.
#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub title: String,
    /// The pane collaborators' presence is scoped to; empty until the
    /// first process claims one.
    pub pane_id: String,
    /// Where this session runs. `None` until the first open records one.
    pub target: Option<crate::target::SessionTarget>,
    /// The process whose screen this is.
    pub local_session: Option<String>,
    /// The session on a relay this session re-attaches to.
    pub relay_session: Option<String>,
    pub screen: Screen,
    pub(crate) objects: ScreenObjects,
}

/// Changes to a session record. None is anyone's undo step: each records
/// what a process did or where it runs, which no undo can take back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TerminalAction {
    /// The title the running program set (OSC 0/2).
    SetTitle { title: String },
    /// A new process takes this session: the previous one's screen, title
    /// and pane are dropped.
    BindLocalSession { session_id: String },
    /// Scope presence to `pane_id`, unless a peer already claimed a pane.
    ClaimPane { pane_id: String },
    /// Record where this session runs (an encoded
    /// [`SessionTarget`](crate::target::SessionTarget)). The target outlives
    /// every process that runs in it, so reopening goes back to it.
    SetTarget { target: String },
    /// Record (or, with `None`, forget) the relay session this session
    /// re-attaches to.
    SetRelaySession { session: Option<String> },
    /// What one batch of the process's output did to its screen.
    Output { edits: Vec<ScreenEdit> },
}

impl TerminalAction {
    /// Apply this action to a session stored under `obj` of another
    /// resource's document: a terminal nested in an editor's panel.
    ///
    /// # Errors
    /// Refuses an invalid id, title or target, and output naming lines the
    /// screen does not have.
    pub fn apply_under(&self, doc: &mut Doc, obj: &ObjId) -> Result<(), ActionError> {
        match self {
            Self::SetTitle { title } => {
                if crate::inner::safe_osc_text(title.as_bytes(), 256).as_deref() != Some(title) {
                    return Err(ActionError::Refused("title is invalid".into()));
                }
                doc.put(obj, TITLE, title.as_str())?;
            }
            Self::BindLocalSession { session_id } => {
                if session_id.is_empty() || session_id.len() > 128 {
                    return Err(ActionError::Refused("local session id is invalid".into()));
                }
                crate::screen::clear(doc, obj)?;
                for key in [TITLE, PANE] {
                    if doc.get(obj, key)?.is_some() {
                        doc.delete(obj, key)?;
                    }
                }
                doc.put(obj, LOCAL_SESSION, session_id.as_str())?;
            }
            Self::ClaimPane { pane_id } => {
                if pane_id.is_empty() || pane_id.len() > 128 {
                    return Err(ActionError::Refused("pane id is invalid".into()));
                }
                if doc.get(obj, PANE)?.is_none() {
                    doc.put(obj, PANE, pane_id.as_str())?;
                }
            }
            Self::SetTarget { target } => {
                if crate::target::SessionTarget::decode(target).is_none() {
                    return Err(ActionError::Refused("session target is invalid".into()));
                }
                doc.put(obj, crate::target::TARGET_KEY, target.as_str())?;
            }
            Self::SetRelaySession { session } => match session {
                Some(session)
                    if session.len() == 32 && session.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    doc.put(obj, crate::target::RELAY_SESSION_KEY, session.as_str())?;
                }
                Some(_) => {
                    return Err(ActionError::Refused("relay session id is invalid".into()));
                }
                None => {
                    if doc.get(obj, crate::target::RELAY_SESSION_KEY)?.is_some() {
                        doc.delete(obj, crate::target::RELAY_SESSION_KEY)?;
                    }
                }
            },
            Self::Output { edits } => crate::screen::apply(doc, obj, edits)?,
        }
        Ok(())
    }
}

impl Action for TerminalAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        self.apply_under(doc, &ROOT)
    }

    fn change_kind(&self) -> ChangeKind {
        ChangeKind::External
    }
}

/// The terminal session resource.
pub struct Terminal;

impl Terminal {
    /// The session stored under `obj` of another resource's document.
    pub fn read_under(doc: &Doc, obj: &ObjId) -> SessionState {
        let (screen, objects) = crate::screen::read(doc, obj);
        SessionState {
            screen,
            objects,
            ..session_fields(doc, obj)
        }
    }
}

impl Resource for Terminal {
    const KIND: &'static str = "terminal.session";
    const LABEL: &'static str = "Terminal";
    const ACTION_TYPE: &'static str = TERMINAL_ACTION_TYPE;

    type State = SessionState;
    type Action = TerminalAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<TerminalAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<SessionState, ModelError> {
        Ok(Self::read_under(doc, &ROOT))
    }

    /// Output re-reads only the lines it touched; anything else reads the
    /// session's few other fields again.
    fn refresh(state: &mut SessionState, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if delta.is_empty() {
            return Ok(());
        }
        let mut screen = std::mem::take(&mut state.screen);
        let mut objects = std::mem::take(&mut state.objects);
        crate::screen::refresh(&mut screen, &mut objects, doc, delta);
        *state = SessionState {
            screen,
            objects,
            ..session_fields(doc, &ROOT)
        };
        Ok(())
    }

    const READABLE: bool = true;

    /// Read the shared screen, not this participant's local process lifecycle.
    fn read_content(
        state: &SessionState,
        _doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if item.is_some() {
            return Err("Terminal has no nested readable resource".into());
        }
        let (output, truncated) = readable_output(&state.screen);
        Ok(serde_json::json!({
            "kind": Self::KIND,
            "title": state.title,
            "target": state.target,
            "session": state.local_session,
            "relay_session": state.relay_session,
            "pane": state.pane_id,
            "alternate": state.screen.alternate,
            "cursor": { "column": state.screen.cursor.0, "row": state.screen.cursor.1 },
            "output": output,
            "output_truncated": truncated,
            "retained_lines": state.screen.active().len(),
        }))
    }
}

/// Keep explicit reads bounded even when retained output contains very long
/// rows. The suffix ends at a Unicode scalar boundary; no terminal content is
/// logged or copied until the caller requests the canonical Read action.
fn readable_output(screen: &Screen) -> (String, bool) {
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
    /// The shared screen is read separately through the resource's Read action.
    #[effect(returns, permission = "read")]
    InspectSession,
    /// Inspect editable field
    ///
    /// Participant-private native editor metadata; never the surrounding buffer.
    #[effect(returns, device_only, permission = "read")]
    InspectEditor,
    /// Replace editor selection
    ///
    /// Literally replace the original qualified editor selection without execution.
    #[effect(job, device_only, destructive)]
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
    const TITLE: &'static str = "Terminal";
    const ICON: Option<&'static str> = Some("terminal");

    fn label(state: &SessionState) -> String {
        if state.title == "Terminal" {
            "Terminal session".to_owned()
        } else {
            format!("Terminal — {}", state.title)
        }
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

fn read_string(doc: &Doc, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The pane every replica scopes presence to. Two peers claiming a pane at
/// once leave both claims in the document; every replica picks the same one
/// (the first valid id in order) rather than whichever `get` exposes.
fn shared_pane_id(doc: &Doc, obj: &ObjId) -> Option<String> {
    let mut candidates = doc
        .get_all(obj, PANE)
        .ok()?
        .into_iter()
        .filter_map(|(value, _)| match value {
            Value::Scalar(value) => value.as_str().map(str::to_owned),
            Value::Object(_) => None,
        })
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.into_iter().next()
}

/// The session's fields beside its screen.
fn session_fields(doc: &Doc, obj: &ObjId) -> SessionState {
    SessionState {
        title: read_string(doc, obj, TITLE)
            .and_then(|title| crate::inner::safe_osc_text(title.as_bytes(), 256))
            .unwrap_or_else(|| "Terminal".into()),
        pane_id: shared_pane_id(doc, obj).unwrap_or_default(),
        target: read_string(doc, obj, crate::target::TARGET_KEY)
            .as_deref()
            .and_then(crate::target::SessionTarget::decode),
        local_session: read_string(doc, obj, LOCAL_SESSION),
        relay_session: read_string(doc, obj, crate::target::RELAY_SESSION_KEY),
        ..SessionState::default()
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

    fn present(&mut self, state: &std::rc::Rc<SessionState>) {
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
        cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        if effect == TerminalEffect::InspectSession {
            return Ok(Effected::value(self.inspect_session()));
        }
        if effect == TerminalEffect::InspectEditor {
            return Ok(Effected::value(self.inspect_editor()));
        }
        if let TerminalEffect::ReplaceEditorSelection {
            session,
            lease,
            text,
        } = effect
        {
            self.replace_editor_selection(&session, &lease, text, cx.reply())?;
            return Ok(Effected::Pending);
        }
        self.perform(effect).map(|()| Effected::done())
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
    fn present(&mut self, state: &std::rc::Rc<SessionState>) {
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
        cx: &mut EffectCx<Terminal, Self>,
    ) -> EffectAnswer {
        if effect == TerminalEffect::InspectSession {
            return Ok(Effected::value(self.inspect_session()));
        }
        if effect == TerminalEffect::InspectEditor {
            return Ok(Effected::value(self.terminal_mut().inspect_editor()));
        }
        if let TerminalEffect::ReplaceEditorSelection {
            session,
            lease,
            text,
        } = effect
        {
            self.terminal_mut()
                .replace_editor_selection(&session, &lease, text, cx.reply())?;
            return Ok(Effected::Pending);
        }
        self.perform(effect).map(|()| Effected::done())
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
mod tests {
    use super::*;

    #[test]
    fn canonical_read_reports_recorded_screen_target_and_unicode_bound_without_local_claims() {
        let mut app = ui::framework::testing::TestApp::<TerminalShell>::new().expect("terminal");
        app.invoke(
            ui::tilo_resource::Invoke::apply(&TerminalAction::SetTarget {
                target: crate::target::SessionTarget::default_environment().encode(),
            })
            .expect("encode target action"),
        )
        .expect("recorded target");
        app.with_surface(|terminal: &mut TerminalWidget| terminal.feed(b"recorded output\r\n"))
            .expect("surface");
        app.settle();
        let state = app.state();
        let read = app
            .call("read", serde_json::json!({}))
            .expect("published canonical read")
            .expect("read answers with shared content");
        assert!(read["output"].as_str().unwrap().contains("recorded output"));
        assert_eq!(
            read["target"],
            serde_json::to_value(crate::target::SessionTarget::default_environment()).unwrap()
        );
        assert_eq!(read["cursor"]["row"], state.screen.cursor.1);
        assert!(
            read.get("lifecycle").is_none(),
            "a shared screen cannot assert local process liveness"
        );
        assert!(
            app.call("read", serde_json::json!({"item": "unknown"}))
                .is_err()
        );
        let screen = Screen {
            main: vec![std::rc::Rc::new(crate::screen::Line {
                text: "é".repeat(40_000),
                attrs: Vec::new(),
            })],
            ..Screen::default()
        };
        let (output, truncated) = readable_output(&screen);
        assert!(truncated);
        assert_eq!(output.len(), 64 * 1024);
        assert!(output.chars().all(|character| character == 'é'));
    }

    #[test]
    fn native_replacement_preserves_private_effect_contract() {
        use ui::tilo_resource::Effect as _;
        let effect = TerminalEffect::ReplaceEditorSelection {
            session: "original-session".into(),
            lease: "private-lease".into(),
            text: "private transcript".into(),
        };
        let debug = format!("{effect:?}");
        assert!(!debug.contains("private"));
        assert!(!debug.contains("original-session"));
        let declaration = TerminalEffect::declarations()
            .into_iter()
            .find(|declaration| declaration.id == effect.id())
            .unwrap();
        assert!(declaration.device_only);
        let text = declaration
            .params
            .iter()
            .find(|param| param.name == "text")
            .unwrap();
        assert_eq!(text.ty, ui::tilo_actions::ParamType::Secret);
        assert_eq!(effect.arguments()["text"], "private transcript");
    }

    #[test]
    fn the_session_publishes_its_effects_and_the_links_on_screen() {
        let mut app = ui::framework::testing::TestApp::<TerminalShell>::new().expect("terminal");
        let mut output = Vec::new();
        for line in 0..100 {
            output.extend_from_slice(format!("line {line:03}\r\n").as_bytes());
        }
        output.extend_from_slice(
            b"see \x1b]8;;https://example.test/docs\x1b\\docs\x1b]8;;\x1b\\ \
              \x1b]8;;file:///home/me/.ssh/id_ed25519\x1b\\key\x1b]8;;\x1b\\\r\n",
        );
        app.with_surface(|terminal: &mut crate::TerminalView| terminal.feed(&output))
            .expect("the terminal surface is hosted");
        app.settle();

        let published = app.agent_surface().to_string();
        for effect in TerminalEffect::declarations() {
            assert!(
                published.contains(&format!("\"{}\"", effect.id)),
                "`{}` is published: {published}",
                effect.id
            );
        }
        assert_eq!(
            published
                .matches("\"resource_kind\":\"terminal.link\"")
                .count(),
            1,
            "the public link is a row, the local file link is not: {published}"
        );

        // Scrolling to the oldest output through the door moves the view.
        app.effect(&TerminalEffect::HistoryTop)
            .expect("history-top is offered while older output is retained");
        let scrolled = app
            .with_surface(|terminal: &mut crate::TerminalView| terminal.scroll_offset())
            .expect("the terminal surface is hosted");
        assert!(scrolled > 0, "the view scrolled back");

        // A reviewed command may not smuggle control characters.
        assert!(
            app.effect_confirmed(&TerminalEffect::Run {
                command: "printf first\nprintf second".into(),
            })
            .is_err()
        );

        // An emulating terminal keeps its own screen when the record is presented.
        let mut widget = TerminalWidget::spawn_headless().expect("headless terminal");
        widget.feed(b"hello");
        ResourceSurface::<Terminal, TerminalEffect>::present(
            &mut widget,
            &std::rc::Rc::new(SessionState::default()),
        );
        assert!(
            widget.grid_text().contains("hello"),
            "the record must not replace the screen of a terminal running its own output"
        );
    }

    /// A scenario's output on screen through `--shots`' own path, with the
    /// view state staged over it.
    fn captured<T>(scenario: &str, read: impl FnOnce(&mut crate::TerminalView) -> T) -> T {
        let mut app = ui::framework::testing::TestApp::<TerminalShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("{scenario} stages: {error}"));
        app.with_surface(read)
            .expect("the terminal surface is hosted")
    }

    #[test]
    fn scenarios_stage_their_output_and_view_state() {
        let (open, counter) = captured("find", |terminal| {
            (terminal.find_is_open(), terminal.find_counter())
        });
        assert!(open, "the find scenario opens the find bar");
        assert_eq!(
            counter,
            Some(("build step 042".into(), Some((1, 1)))),
            "one line matches `build step 042`"
        );

        let selected = captured("shell-select", |terminal| terminal.test_selected_text());
        assert_eq!(
            selected.as_deref(),
            Some("primes below 20: 2 3 5 7 11 13 17 19"),
            "the printed line is selected, not the command that echoed it"
        );

        let (scrolled, oldest) = captured("scrollback", |terminal| {
            (
                terminal.scroll_offset(),
                terminal.visible_lines().join("\n"),
            )
        });
        assert_eq!(scrolled, 20);
        assert!(oldest.contains("log entry 040"), "{oldest}");

        assert!(captured("tui", |terminal| terminal.is_alt_screen()));
        assert!(captured("ended", |terminal| terminal.is_disconnected()));
        assert!(captured("empty-menu", |terminal| terminal.context_menu_is_open()));
        let target = captured("host", |terminal| terminal.session_target().cloned());
        assert_eq!(target, Some(crate::target::SessionTarget::Host));
    }

    fn texts(state: &SessionState) -> String {
        state
            .screen
            .main
            .iter()
            .map(|line| line.text.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn output_reaches_the_session_record_but_is_nobodys_undo_step() {
        let mut app = ui::framework::testing::TestApp::<TerminalShell>::new().expect("terminal");
        app.with_surface(|terminal: &mut crate::TerminalView| {
            terminal.feed(b"\x1b]2;build\x07first line\r\nsecond \x1b[1;31mred\x1b[0m\r\n");
        })
        .expect("the terminal surface is hosted");
        app.settle();
        let state = app.state();
        assert!(texts(&state).contains("first line"), "{}", texts(&state));
        assert!(texts(&state).contains("second red"), "{}", texts(&state));
        assert_eq!(state.title, "build");
        assert_eq!(state.screen.cursor, (0, 2));
        assert!(!app.undo(), "a process's output is not an undo step");
        assert!(texts(&app.state()).contains("first line"));

        // More output records only what changed.
        app.with_surface(|terminal: &mut crate::TerminalView| terminal.feed(b"third\r\n"))
            .expect("the terminal surface is hosted");
        app.settle();
        let state = app.state();
        assert!(
            texts(&state).contains("first line\nsecond red\nthird"),
            "{}",
            texts(&state)
        );
    }

    #[test]
    fn a_terminal_without_a_process_shows_the_session_record() {
        let mut doc = Doc::new();
        let screen = crate::screen::Screen {
            main: vec![std::rc::Rc::new(crate::screen::Line {
                text: "recorded elsewhere".into(),
                attrs: Vec::new(),
            })],
            cursor: (3, 0),
            ..crate::screen::Screen::default()
        };
        TerminalAction::Output {
            edits: crate::screen::edits(None, &screen),
        }
        .apply(&mut doc)
        .unwrap();
        TerminalAction::SetTitle {
            title: "vim".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let state = Terminal::read(&doc).unwrap();
        let mut viewer = TerminalWidget::spawn_headless().expect("headless terminal");
        ResourceSurface::<Terminal, TerminalEffect>::present(&mut viewer, &std::rc::Rc::new(state));
        assert!(viewer.grid_text().contains("recorded elsewhere"));
        assert_eq!(viewer.title(), "vim");
    }

    #[test]
    fn bind_local_session_drops_the_previous_process_record() {
        let mut doc = Doc::new();
        let screen = crate::screen::Screen {
            main: vec![std::rc::Rc::new(crate::screen::Line {
                text: "old prompt".into(),
                attrs: Vec::new(),
            })],
            ..crate::screen::Screen::default()
        };
        for action in [
            TerminalAction::SetTitle {
                title: "old session".into(),
            },
            TerminalAction::ClaimPane {
                pane_id: uuid::Uuid::new_v4().to_string(),
            },
            TerminalAction::Output {
                edits: crate::screen::edits(None, &screen),
            },
            TerminalAction::BindLocalSession {
                session_id: "process-2".into(),
            },
        ] {
            action.apply(&mut doc).unwrap();
        }
        let state = Terminal::read(&doc).unwrap();
        assert_eq!(state.title, "Terminal");
        assert!(state.pane_id.is_empty());
        assert!(state.screen.main.is_empty());
        assert_eq!(state.local_session.as_deref(), Some("process-2"));
    }

    #[test]
    fn the_first_pane_claim_wins() {
        let mut doc = Doc::new();
        let first = uuid::Uuid::new_v4().to_string();
        for pane_id in [first.clone(), uuid::Uuid::new_v4().to_string()] {
            TerminalAction::ClaimPane { pane_id }
                .apply(&mut doc)
                .unwrap();
        }
        assert_eq!(Terminal::read(&doc).unwrap().pane_id, first);
    }

    #[test]
    fn a_session_nested_in_another_resource_stays_under_its_object() {
        let mut doc = Doc::new();
        let panel = doc
            .put_object(ROOT, "terminal_panel", automerge::ObjType::Map)
            .unwrap();
        let screen = crate::screen::Screen {
            main: vec![std::rc::Rc::new(crate::screen::Line {
                text: "nested".into(),
                attrs: Vec::new(),
            })],
            ..crate::screen::Screen::default()
        };
        for action in [
            TerminalAction::BindLocalSession {
                session_id: "process-1".into(),
            },
            TerminalAction::Output {
                edits: crate::screen::edits(None, &screen),
            },
        ] {
            action.apply_under(&mut doc, &panel).unwrap();
        }
        assert!(
            Terminal::read(&doc).unwrap().screen.main.is_empty(),
            "nothing lands at the document root"
        );
        let nested = Terminal::read_under(&doc, &panel);
        assert_eq!(nested.screen.main[0].text, "nested");
        assert_eq!(nested.local_session.as_deref(), Some("process-1"));
    }
}
