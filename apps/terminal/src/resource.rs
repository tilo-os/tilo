//! Terminal as a [`tilo_resource::Resource`]: a session record holds what the
//! session shows — its title, where it runs, and the screen of its process —
//! so every collaborator and every past version reads it. The process itself
//! is the surface's; its output reaches the record as one action per batch.

use automerge::transaction::Transactable;
use automerge::{ObjId, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, ChangeKind, EncodedAction,
    OperationKind, ParamType, RetryPolicy,
};
use ui::tilo_resource::{Delta, Doc, ModelError, Resource};
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

    #[allow(
        clippy::expect_used,
        reason = "string payloads for prompted PTY effects always encode"
    )]
    fn local_operations() -> Vec<ActionSpec> {
        // PTY process control: Effects with no document delta. Order matches
        // the live session resource; the widget overlays disconnect state.
        let mut ops = vec![
            ActionSpec::prompted_operation(
                "send",
                "Send text",
                "Text to send",
                "text",
                OperationKind::Effect,
                "terminal.send",
                |text| EncodedAction::encode_operation("terminal.send", &text).expect("payload"),
            )
            .with_description(
                "Writes reviewed text without pressing Enter; confirmation is required.",
            )
            .privileged()
            .retry_policy(RetryPolicy::AtMostOnce),
            ActionSpec::prompted_operation(
                "run",
                "Run command",
                "Command to run",
                "command",
                OperationKind::Effect,
                "terminal.run",
                |text| EncodedAction::encode_operation("terminal.run", &text).expect("payload"),
            )
            .with_description(
                "Runs one reviewed command in this live process; confirmation is required.",
            )
            .privileged()
            .retry_policy(RetryPolicy::AtMostOnce),
        ];
        ops.extend(target_operations());
        for (id, label, type_id) in [
            ("interrupt", "Interrupt process", "terminal.control"),
            ("clear", "Clear screen", "terminal.control"),
        ] {
            if let Ok(spec) = ActionSpec::operation(id, label, OperationKind::Effect, type_id, &id)
            {
                ops.push(spec.privileged().retry_policy(RetryPolicy::AtMostOnce));
            }
        }
        for (id, label) in [
            ("paste", "Paste"),
            ("paste-primary", "Paste primary selection"),
        ] {
            ops.push(
                ActionSpec::typed_operation(
                    id,
                    label,
                    "text",
                    OperationKind::Effect,
                    "terminal.paste",
                    vec![ActionParam::required(
                        "text",
                        ParamType::Text,
                        "Exact text to paste after confirmation",
                    )],
                    |args| {
                        EncodedAction::encode_operation(
                            "terminal.paste",
                            &args.text("text").unwrap_or_default(),
                        )
                        .expect("a borrowed string always serializes")
                    },
                )
                .with_description(
                    "Pastes the exact supplied text; clipboard contents are never re-read after confirmation.",
                )
                .privileged()
                .retry_policy(RetryPolicy::AtMostOnce),
            );
        }
        ops
    }
}

/// Where the session runs and the environments it can run in: effects an
/// agent can invoke as well as the person. The device's own shell is never
/// an agent's to choose; the handler refuses it.
#[allow(
    clippy::expect_used,
    reason = "string payloads for target effects always encode"
)]
fn target_operations() -> Vec<ActionSpec> {
    let mut out = Vec::new();
    out.push(
        ActionSpec::typed_operation(
            "open-in",
            "Open in…",
            "target",
            OperationKind::Effect,
            "terminal.target",
            vec![ActionParam::required(
                "target",
                ParamType::Text,
                "Where the session runs: `environment` (the default environment), \
                     `environment:<record>`, or `legacy:<vm>`. The device's own shell \
                     (`host`) can only be chosen by the person.",
            )],
            |args| {
                EncodedAction::encode_operation(
                    "terminal.target",
                    &args.text("target").unwrap_or_default(),
                )
                .expect("a borrowed string always serializes")
            },
        )
        .with_description(
            "Restarts this session in another environment and records it as the session's target.",
        )
        .privileged()
        .retry_policy(RetryPolicy::AtMostOnce),
    );
    out.extend(environment_operations());
    out
}

/// The environments a session can run in, managed from the session: create,
/// grant a folder, rename.
#[allow(
    clippy::expect_used,
    reason = "string payloads for environment effects always encode"
)]
fn environment_operations() -> Vec<ActionSpec> {
    let mut out = Vec::new();
    out.push(
        ActionSpec::typed_operation(
            "create-environment",
            "New environment",
            "name",
            OperationKind::Effect,
            "terminal.environment",
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "What to call the new environment",
            )],
            |args| {
                EncodedAction::encode_operation(
                    "terminal.environment",
                    &serde_json::json!({
                        "op": "create",
                        "name": args.text("name").unwrap_or_default(),
                    })
                    .to_string(),
                )
                .expect("a borrowed string always serializes")
            },
        )
        .with_description("Creates an environment on the default runtime with nothing added.")
        .privileged()
        .retry_policy(RetryPolicy::AtMostOnce),
    );
    out.push(
        ActionSpec::typed_operation(
            "grant-folder",
            "Grant folder to environment",
            "path",
            OperationKind::Effect,
            "terminal.environment",
            vec![
                ActionParam::required("record", ParamType::Text, "The environment's record"),
                ActionParam::required(
                    "path",
                    ParamType::Text,
                    "An absolute folder path; it appears at the same path in the environment",
                ),
            ],
            |args| {
                EncodedAction::encode_operation(
                    "terminal.environment",
                    &serde_json::json!({
                        "op": "grant-folder",
                        "record": args.text("record").unwrap_or_default(),
                        "path": args.text("path").unwrap_or_default(),
                    })
                    .to_string(),
                )
                .expect("a borrowed string always serializes")
            },
        )
        .with_description(
            "Binds a host folder into the environment, read-write; confirmation is required.",
        )
        .privileged()
        .retry_policy(RetryPolicy::AtMostOnce),
    );
    out.push(
        ActionSpec::typed_operation(
            "rename-environment",
            "Rename environment",
            "name",
            OperationKind::Effect,
            "terminal.environment",
            vec![
                ActionParam::required("record", ParamType::Text, "The environment's record"),
                ActionParam::required("name", ParamType::Text, "The new name"),
            ],
            |args| {
                EncodedAction::encode_operation(
                    "terminal.environment",
                    &serde_json::json!({
                        "op": "rename",
                        "record": args.text("record").unwrap_or_default(),
                        "name": args.text("name").unwrap_or_default(),
                    })
                    .to_string(),
                )
                .expect("a borrowed string always serializes")
            },
        )
        .privileged()
        .retry_policy(RetryPolicy::AtMostOnce),
    );
    out
}

/// Framework app: the terminal widget is the surface over [`Terminal`].
pub struct TerminalShell;

impl ResourceApp for TerminalShell {
    type Resource = Terminal;
    const ID: &'static str = "so.tilo.terminal";
    const TITLE: &'static str = "Terminal";
    const ICON: Option<&'static str> = Some("terminal");

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
                .or_else(|_| TerminalWidget::spawn_headless())
        } else if cx.mode().is_live() && !cfg!(test) {
            TerminalWidget::spawn_default().or_else(|_| TerminalWidget::spawn_headless())
        } else {
            TerminalWidget::spawn_headless()
        }
        .expect("terminal surface requires an emulator");
        cx.surface(widget)
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
        ..SessionState::default()
    }
}

impl ResourceSurface<Terminal> for TerminalWidget {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::resources::PublishesResources;

    #[test]
    fn publish_owns_session_door_and_hosted_keeps_only_the_transcript() {
        let published = ui::tilo_resource::publish::<Terminal>(
            "terminal.session",
            "Terminal",
            &SessionState::default(),
        );
        let ids: Vec<&str> = published
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for expected in [
            "send",
            "run",
            "interrupt",
            "clear",
            "paste",
            "paste-primary",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }

        let mut widget = TerminalWidget::spawn_headless().expect("headless terminal");
        widget.feed(b"hello");
        assert!(!widget.is_framework_hosted());
        let doc = ui::automerge::AutoCommit::new();
        let standalone = widget.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, Terminal::KIND);

        widget.mark_hosted_for_test();
        let extras = widget.resources(&doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != Terminal::KIND),
            "hosted must not republish terminal.session"
        );
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].kind, "terminal.transcript");
        ResourceSurface::<Terminal>::present(
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
    fn captured<T>(scenario: &str, read: impl FnOnce(&mut TerminalWidget) -> T) -> T {
        let mut app = ui::framework::testing::TestApp::<TerminalShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("{scenario} stages: {error}"));
        app.with_surface(read)
            .expect("the terminal surface is hosted")
    }

    #[test]
    fn scenarios_stage_their_output_and_view_state() {
        let (open, matches) = captured("find", |terminal| terminal.find_test_state());
        assert!(open, "the find scenario opens the find bar");
        assert_eq!(matches.len(), 1, "one line matches `build step 042`");

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
        app.with_surface(|terminal: &mut TerminalWidget| {
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
        app.with_surface(|terminal: &mut TerminalWidget| terminal.feed(b"third\r\n"))
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
        ResourceSurface::<Terminal>::present(&mut viewer, &std::rc::Rc::new(state));
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
