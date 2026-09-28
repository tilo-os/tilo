//! Terminal as a [`tilo_resource::Resource`]: the shared session record is
//! document state; PTY process control is published as [`Effect`] operations.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::TerminalWidget;

const TERMINAL_ACTION_TYPE: &str = "terminal.action";

/// Shared session fields that sync across collaborators.
#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub title: String,
    pub pane_id: String,
}

/// Document mutations for the session record (title / scrollback attribution).
/// PTY stdin and process control are Effects in [`Terminal::local_operations`],
/// not variants here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TerminalAction {
    /// Update the OSC / window title mirrored into the shared session.
    SetTitle { title: String },
    /// Append one line to the shared scrollback record.
    AppendScrollback { line: String },
}

impl Action for TerminalAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetTitle { title } => {
                doc.put(ROOT, "terminal_title", title.as_str())?;
            }
            Self::AppendScrollback { line } => {
                let list = match doc.get(ROOT, "terminal_scrollback")? {
                    Some((_, id)) => id,
                    None => {
                        doc.put_object(ROOT, "terminal_scrollback", automerge::ObjType::List)?
                    }
                };
                let index = doc.length(&list);
                doc.insert(&list, index, line.as_str())?;
            }
        }
        Ok(())
    }
}

/// The terminal session resource.
pub struct Terminal;

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
        Ok(SessionState {
            title: read_string(doc, "terminal_title").unwrap_or_else(|| "Terminal".into()),
            pane_id: read_string(doc, "terminal_pane_id").unwrap_or_default(),
        })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if doc.get(ROOT, "terminal_pane_id")?.is_none() {
            let id = uuid::Uuid::new_v4().to_string();
            doc.put(ROOT, "terminal_pane_id", id.as_str())?;
            doc.put(ROOT, "terminal_title", "Terminal")?;
        }
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
            .privileged(),
            ActionSpec::prompted_operation(
                "run",
                "Run command",
                "Command to run",
                "command",
                OperationKind::Effect,
                "terminal.run",
                |text| EncodedAction::encode_operation("terminal.run", &text).expect("payload"),
            )
            .privileged(),
        ];
        for (id, label, type_id) in [
            ("interrupt", "Interrupt process", "terminal.control"),
            ("clear", "Clear screen", "terminal.control"),
        ] {
            if let Ok(spec) = ActionSpec::operation(id, label, OperationKind::Effect, type_id, &id)
            {
                ops.push(spec.privileged());
            }
        }
        ops
    }
}

/// Framework app: the terminal widget is the surface over [`Terminal`].
pub struct TerminalShell;

impl ResourceApp for TerminalShell {
    type Resource = Terminal;
    const ID: &'static str = "so.tilo.terminal";
    const TITLE: &'static str = "Terminal";
    const ICON: Option<&'static str> = Some("terminal");
    const SHOT_VIEWPORT: (u32, u32) = (1200, 720);

    #[allow(
        clippy::expect_used,
        reason = "a framework surface without an emulator cannot present a terminal"
    )]
    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        // History / read-only captures never own a login shell. The live app
        // prefers a real PTY and falls back to a headless emulator.
        let widget = if cx.read_only() {
            TerminalWidget::spawn_headless()
        } else {
            TerminalWidget::spawn_default().or_else(|_| TerminalWidget::spawn_headless())
        }
        .expect("terminal surface requires an emulator");
        cx.surface(widget)
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "shell",
            "shell-select",
            "colors",
            "tui",
            "scrollback",
            "find",
            "empty",
            "empty-menu",
            "ended",
        ]
    }
}

impl ResourceSurface<Terminal> for TerminalWidget {
    fn present(&mut self, _state: &std::rc::Rc<SessionState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Terminal>) {}
}

fn read_string(doc: &Doc, key: &str) -> Option<String> {
    match doc.get(ROOT, key).ok().flatten() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}
