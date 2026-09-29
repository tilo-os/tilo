//! Terminal as a [`tilo_resource::Resource`]: the shared session record is
//! document state; PTY process control is published as [`Effect`] operations.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
    ParamType,
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
    /// A new local process is taking this tab. Drop the previous process's
    /// grid, cursor, links, and images, and record the process id.
    BindLocalSession { session_id: String },
    /// Register one OSC 8 hyperlink on the shared session.
    RegisterLink { id: u16, url: String },
    /// Write the shared pane id when no peer has claimed it yet.
    ClaimPane { pane_id: String },
    /// Record one content-addressed image placement on the shared session.
    PlaceImage {
        hash: String,
        mime: String,
        byte_len: u64,
        row: u64,
        col: u64,
        columns: u64,
        rows: u64,
        pixel_width: u64,
        pixel_height: u64,
        protocol_id: Option<u64>,
        alternate_screen: bool,
    },
    /// Create the shared grid, links, images, and cursor when they are missing.
    EnsureStructure,
    /// Grow one grid with empty lines up to `min_len`.
    EnsureLines { alternate: bool, min_len: u32 },
    /// Write the shared cursor after a feed.
    SetCursor { x: u64, y: u64 },
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
            Self::BindLocalSession { session_id } => {
                if session_id.is_empty() || session_id.len() > 128 {
                    return Err(ActionError::Refused("local session id is invalid".into()));
                }
                for key in [
                    "terminal_grid",
                    "terminal_grid_alt",
                    "cursor",
                    "terminal_links",
                    "terminal_images",
                    "terminal_title",
                    "terminal_pane_id",
                ] {
                    let _ = doc.delete(ROOT, key);
                }
                doc.put(ROOT, "terminal_local_session", session_id.as_str())?;
            }
            Self::RegisterLink { id, url } => {
                if *id == 0 || url.is_empty() || url.len() > 2 * 1024 {
                    return Err(ActionError::Refused("hyperlink is invalid".into()));
                }
                let links = match doc.get(ROOT, "terminal_links")? {
                    Some((_, existing)) => existing,
                    None => doc.put_object(ROOT, "terminal_links", automerge::ObjType::Map)?,
                };
                let key = id.to_string();
                if doc.get(&links, &key)?.is_none() && doc.length(&links) >= 4096 {
                    return Err(ActionError::Refused("hyperlink table is full".into()));
                }
                doc.put(&links, key, url.as_str())?;
            }
            Self::ClaimPane { pane_id } => {
                if pane_id.is_empty() || pane_id.len() > 128 {
                    return Err(ActionError::Refused("pane id is invalid".into()));
                }
                if doc.get(ROOT, "terminal_pane_id")?.is_some() {
                    return Ok(());
                }
                doc.put(ROOT, "terminal_pane_id", pane_id.as_str())?;
            }
            Self::PlaceImage {
                hash,
                mime,
                byte_len,
                row,
                col,
                columns,
                rows,
                pixel_width,
                pixel_height,
                protocol_id,
                alternate_screen,
            } => {
                if hash.is_empty()
                    || hash.len() > 128
                    || mime.is_empty()
                    || mime.len() > 64
                    || *columns == 0
                    || *rows == 0
                    || *columns > 1024
                    || *rows > 1024
                {
                    return Err(ActionError::Refused("image placement is invalid".into()));
                }
                let images = match doc.get(ROOT, "terminal_images")? {
                    Some((_, existing)) => existing,
                    None => doc.put_object(ROOT, "terminal_images", automerge::ObjType::List)?,
                };
                let index = doc.length(&images);
                let placement = doc.insert_object(&images, index, automerge::ObjType::Map)?;
                doc.put(&placement, "hash", hash.as_str())?;
                doc.put(&placement, "mime", mime.as_str())?;
                doc.put(&placement, "byte_len", *byte_len)?;
                doc.put(&placement, "row", *row)?;
                doc.put(&placement, "col", *col)?;
                doc.put(&placement, "columns", *columns)?;
                doc.put(&placement, "rows", *rows)?;
                doc.put(&placement, "pixel_width", *pixel_width)?;
                doc.put(&placement, "pixel_height", *pixel_height)?;
                doc.put(&placement, "alternate_screen", *alternate_screen)?;
                if let Some(protocol_id) = protocol_id {
                    doc.put(&placement, "protocol_id", *protocol_id)?;
                }
            }
            Self::EnsureStructure => ensure_structure(doc)?,
            Self::EnsureLines { alternate, min_len } => {
                ensure_lines(doc, *alternate, *min_len)?;
            }
            Self::SetCursor { x, y } => set_cursor(doc, *x, *y)?,
        }
        Ok(())
    }
}

fn object_at(
    doc: &mut Doc,
    key: &str,
    kind: automerge::ObjType,
) -> Result<automerge::ObjId, ActionError> {
    match doc.get(ROOT, key)? {
        Some((_, existing)) => Ok(existing),
        None => Ok(doc.put_object(ROOT, key, kind)?),
    }
}

fn ensure_structure(doc: &mut Doc) -> Result<(), ActionError> {
    object_at(doc, "terminal_grid", automerge::ObjType::List)?;
    object_at(doc, "terminal_grid_alt", automerge::ObjType::List)?;
    object_at(doc, "terminal_links", automerge::ObjType::Map)?;
    object_at(doc, "terminal_images", automerge::ObjType::List)?;
    if doc.get(ROOT, "cursor")?.is_none() {
        set_cursor(doc, 0, 0)?;
    }
    Ok(())
}

fn ensure_lines(doc: &mut Doc, alternate: bool, min_len: u32) -> Result<(), ActionError> {
    if min_len > 4096 {
        return Err(ActionError::Refused("grid is too tall".into()));
    }
    let key = if alternate {
        "terminal_grid_alt"
    } else {
        "terminal_grid"
    };
    let grid = object_at(doc, key, automerge::ObjType::List)?;
    let target = min_len as usize;
    while doc.length(&grid) < target {
        let index = doc.length(&grid);
        let line = doc.insert_object(&grid, index, automerge::ObjType::Map)?;
        doc.put_object(&line, "text", automerge::ObjType::Text)?;
    }
    Ok(())
}

fn set_cursor(doc: &mut Doc, x: u64, y: u64) -> Result<(), ActionError> {
    let cursor = object_at(doc, "cursor", automerge::ObjType::Map)?;
    doc.put(&cursor, "x", x)?;
    doc.put(&cursor, "y", y)?;
    Ok(())
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
                .privileged(),
            );
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
    /// The grid is this PTY's. An empty session projection must not clear it.
    fn present(&mut self, _state: &std::rc::Rc<SessionState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Terminal>) {
        self.mark_framework_hosted();
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
        widget.force_grid(vec!["hello".into()]);
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
            widget
                .visible_lines()
                .iter()
                .any(|line| line.contains("hello")),
            "empty present must not wipe the local grid"
        );
    }

    #[test]
    fn bind_local_session_drops_the_previous_process_record() {
        use automerge::transaction::Transactable;
        let mut doc = ui::automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "terminal_title", "old session")
            .unwrap();
        doc.put(automerge::ROOT, "terminal_pane_id", "pane-1")
            .unwrap();
        TerminalAction::BindLocalSession {
            session_id: "process-2".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(automerge::ROOT, "terminal_title")
                .unwrap()
                .is_none()
        );
        assert!(
            doc.get(automerge::ROOT, "terminal_pane_id")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            read_string(&doc, "terminal_local_session").as_deref(),
            Some("process-2")
        );
    }

    #[test]
    fn register_link_and_claim_pane_are_replayable() {
        let mut doc = ui::automerge::AutoCommit::new();
        TerminalAction::RegisterLink {
            id: 1,
            url: "https://tilo.so".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let links = doc
            .get(automerge::ROOT, "terminal_links")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        let stored = doc.get(&links, "1").unwrap().map(|(value, _)| value);
        assert!(matches!(stored, Some(automerge::Value::Scalar(_))));
        TerminalAction::ClaimPane {
            pane_id: "first".into(),
        }
        .apply(&mut doc)
        .unwrap();
        TerminalAction::ClaimPane {
            pane_id: "second".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            read_string(&doc, "terminal_pane_id").as_deref(),
            Some("first")
        );
    }

    #[test]
    fn place_image_records_a_content_addressed_descriptor() {
        let mut doc = ui::automerge::AutoCommit::new();
        TerminalAction::PlaceImage {
            hash: "blake3:abc".into(),
            mime: "image/png".into(),
            byte_len: 12,
            row: 1,
            col: 2,
            columns: 8,
            rows: 4,
            pixel_width: 64,
            pixel_height: 32,
            protocol_id: Some(7),
            alternate_screen: false,
        }
        .apply(&mut doc)
        .unwrap();
        let images = doc
            .get(automerge::ROOT, "terminal_images")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        assert_eq!(doc.length(&images), 1);
    }

    #[test]
    fn structure_lines_and_cursor_are_session_actions() {
        let mut doc = ui::automerge::AutoCommit::new();
        TerminalAction::EnsureStructure.apply(&mut doc).unwrap();
        TerminalAction::EnsureStructure.apply(&mut doc).unwrap();
        TerminalAction::EnsureLines {
            alternate: false,
            min_len: 2,
        }
        .apply(&mut doc)
        .unwrap();
        TerminalAction::SetCursor { x: 3, y: 1 }
            .apply(&mut doc)
            .unwrap();
        let grid = doc
            .get(automerge::ROOT, "terminal_grid")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        assert_eq!(doc.length(&grid), 2);
        let cursor = doc
            .get(automerge::ROOT, "cursor")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        let x = doc
            .get(&cursor, "x")
            .unwrap()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(value) => value.as_i64(),
                _ => None,
            });
        assert_eq!(x, Some(3));
    }
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
