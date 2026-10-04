//! Terminal as a [`tilo_resource::Resource`]: the shared session record is
//! document state; PTY process control is published as [`Effect`] operations.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
    ParamType, RetryPolicy,
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
    /// Where this session runs. `None` until the first open records one.
    pub target: Option<crate::target::SessionTarget>,
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
    /// Record where this session runs (an encoded
    /// [`SessionTarget`](crate::target::SessionTarget)). The target outlives
    /// every process that runs in it, so reopening goes back to it.
    SetTarget { target: String },
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
    /// Replace one grid line's text and cell attributes after a print run.
    WriteLine {
        alternate: bool,
        index: u32,
        text: String,
        attrs: Vec<u8>,
    },
    /// Insert one blank grid line at `index`.
    InsertBlankLine {
        alternate: bool,
        index: u32,
        text: String,
        attrs: Vec<u8>,
    },
    /// Drop one grid line. Scrollback, scrolling regions, and the alternate
    /// screen all use this.
    DeleteGridLine { alternate: bool, index: u32 },
    /// Drop `count` lines from the front of the main scrollback.
    TrimGridPrefix { count: u32 },
    /// After scrollback is trimmed, move or drop main-screen image placements.
    ShiftImageRows { removed_rows: u32 },
}

impl Action for TerminalAction {
    #[allow(
        clippy::too_many_lines,
        reason = "terminal session CRDT mutations stay in one Action::apply match"
    )]
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetTitle { title } => {
                let parent = session_parent(doc)?;
                doc.put(&parent, "terminal_title", title.as_str())?;
            }
            Self::AppendScrollback { line } => {
                let parent = session_parent(doc)?;
                let list = match doc.get(&parent, "terminal_scrollback")? {
                    Some((_, id)) => id,
                    None => {
                        doc.put_object(&parent, "terminal_scrollback", automerge::ObjType::List)?
                    }
                };
                let index = doc.length(&list);
                doc.insert(&list, index, line.as_str())?;
            }
            Self::BindLocalSession { session_id } => {
                if session_id.is_empty() || session_id.len() > 128 {
                    return Err(ActionError::Refused("local session id is invalid".into()));
                }
                let parent = session_parent(doc)?;
                for key in [
                    "terminal_grid",
                    "terminal_grid_alt",
                    "cursor",
                    "terminal_links",
                    "terminal_images",
                    "terminal_title",
                    "terminal_pane_id",
                ] {
                    let _ = doc.delete(&parent, key);
                }
                doc.put(&parent, "terminal_local_session", session_id.as_str())?;
            }
            Self::SetTarget { target } => {
                if crate::target::SessionTarget::decode(target).is_none() {
                    return Err(ActionError::Refused("session target is invalid".into()));
                }
                let parent = session_parent(doc)?;
                doc.put(&parent, crate::target::TARGET_KEY, target.as_str())?;
            }
            Self::RegisterLink { id, url } => {
                if *id == 0 || url.is_empty() || url.len() > 2 * 1024 {
                    return Err(ActionError::Refused("hyperlink is invalid".into()));
                }
                let parent = session_parent(doc)?;
                let links = match doc.get(&parent, "terminal_links")? {
                    Some((_, existing)) => existing,
                    None => doc.put_object(&parent, "terminal_links", automerge::ObjType::Map)?,
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
                let parent = session_parent(doc)?;
                if doc.get(&parent, "terminal_pane_id")?.is_some() {
                    return Ok(());
                }
                doc.put(&parent, "terminal_pane_id", pane_id.as_str())?;
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
                let parent = session_parent(doc)?;
                let images = match doc.get(&parent, "terminal_images")? {
                    Some((_, existing)) => existing,
                    None => doc.put_object(&parent, "terminal_images", automerge::ObjType::List)?,
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
            Self::WriteLine {
                alternate,
                index,
                text,
                attrs,
            } => write_line(doc, *alternate, *index, text, attrs)?,
            Self::InsertBlankLine {
                alternate,
                index,
                text,
                attrs,
            } => insert_blank_line(doc, *alternate, *index, text, attrs)?,
            Self::DeleteGridLine { alternate, index } => {
                delete_grid_line(doc, *alternate, *index)?;
            }
            Self::TrimGridPrefix { count } => trim_grid_prefix(doc, *count)?,
            Self::ShiftImageRows { removed_rows } => shift_image_rows(doc, *removed_rows)?,
        }
        Ok(())
    }
}

fn session_parent(doc: &Doc) -> Result<automerge::ObjId, ActionError> {
    match doc.get(ROOT, "terminal_panel")? {
        Some((Value::Object(automerge::ObjType::Map), id)) => Ok(id),
        Some(_) => Err(ActionError::Refused("terminal panel is not a map".into())),
        None => Ok(ROOT),
    }
}

fn object_at(
    doc: &mut Doc,
    key: &str,
    kind: automerge::ObjType,
) -> Result<automerge::ObjId, ActionError> {
    let parent = session_parent(doc)?;
    match doc.get(&parent, key)? {
        Some((_, existing)) => Ok(existing),
        None => Ok(doc.put_object(&parent, key, kind)?),
    }
}

fn ensure_structure(doc: &mut Doc) -> Result<(), ActionError> {
    object_at(doc, "terminal_grid", automerge::ObjType::List)?;
    object_at(doc, "terminal_grid_alt", automerge::ObjType::List)?;
    object_at(doc, "terminal_links", automerge::ObjType::Map)?;
    object_at(doc, "terminal_images", automerge::ObjType::List)?;
    let parent = session_parent(doc)?;
    if doc.get(&parent, "cursor")?.is_none() {
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

fn line_payload_too_large(text: &str, attrs: &[u8]) -> bool {
    text.len() > 1024 * 1024 || attrs.len() > 1024 * 1024
}

fn write_line(
    doc: &mut Doc,
    alternate: bool,
    index: u32,
    text: &str,
    attrs: &[u8],
) -> Result<(), ActionError> {
    if line_payload_too_large(text, attrs) {
        return Err(ActionError::Refused("grid line is too large".into()));
    }
    let key = if alternate {
        "terminal_grid_alt"
    } else {
        "terminal_grid"
    };
    let grid = object_at(doc, key, automerge::ObjType::List)?;
    let index = index as usize;
    if index >= doc.length(&grid) {
        return Err(ActionError::Refused("grid line is missing".into()));
    }
    let Some((Value::Object(automerge::ObjType::Map), line)) = doc.get(&grid, index)? else {
        return Err(ActionError::Refused("grid line is missing".into()));
    };
    let text_id = match doc.get(&line, "text")? {
        Some((_, id)) => id,
        None => doc.put_object(&line, "text", automerge::ObjType::Text)?,
    };
    ui::doc::splice_text_diffed(doc, &text_id, text);
    doc.put(&line, "attrs", attrs.to_vec())?;
    Ok(())
}

fn insert_blank_line(
    doc: &mut Doc,
    alternate: bool,
    index: u32,
    text: &str,
    attrs: &[u8],
) -> Result<(), ActionError> {
    if line_payload_too_large(text, attrs) {
        return Err(ActionError::Refused("grid line is too large".into()));
    }
    let key = if alternate {
        "terminal_grid_alt"
    } else {
        "terminal_grid"
    };
    let grid = object_at(doc, key, automerge::ObjType::List)?;
    let at = index as usize;
    if at > doc.length(&grid) {
        return Err(ActionError::Refused("grid line is missing".into()));
    }
    doc.insert_object(&grid, at, automerge::ObjType::Map)?;
    write_line(doc, alternate, index, text, attrs)
}

fn delete_grid_line(doc: &mut Doc, alternate: bool, index: u32) -> Result<(), ActionError> {
    let key = if alternate {
        "terminal_grid_alt"
    } else {
        "terminal_grid"
    };
    let grid = object_at(doc, key, automerge::ObjType::List)?;
    let at = index as usize;
    if at >= doc.length(&grid) {
        return Err(ActionError::Refused("grid line is missing".into()));
    }
    doc.delete(&grid, at)?;
    Ok(())
}

fn trim_grid_prefix(doc: &mut Doc, count: u32) -> Result<(), ActionError> {
    if count > 100_000 {
        return Err(ActionError::Refused("scrollback trim is too large".into()));
    }
    let parent = session_parent(doc)?;
    let Some((_, grid)) = doc.get(&parent, "terminal_grid")? else {
        return Ok(());
    };
    let count = (count as usize).min(doc.length(&grid));
    for _ in 0..count {
        doc.delete(&grid, 0)?;
    }
    Ok(())
}

fn shift_image_rows(doc: &mut Doc, removed_rows: u32) -> Result<(), ActionError> {
    let parent = session_parent(doc)?;
    let Some((_, images)) = doc.get(&parent, "terminal_images")? else {
        return Ok(());
    };
    let removed = removed_rows as usize;
    for index in (0..doc.length(&images)).rev() {
        let Some((Value::Object(automerge::ObjType::Map), placement)) = doc.get(&images, index)?
        else {
            continue;
        };
        let alternate = matches!(
            doc.get(&placement, "alternate_screen")?,
            Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Boolean(true))
        );
        if alternate {
            continue;
        }
        let row = match doc.get(&placement, "row")? {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Uint(n) => *n as usize,
                ScalarValue::Int(n) if *n >= 0 => *n as usize,
                _ => 0,
            },
            _ => 0,
        };
        if row < removed {
            doc.delete(&images, index)?;
        } else {
            doc.put(&placement, "row", (row - removed) as u64)?;
        }
    }
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
            target: read_string(doc, crate::target::TARGET_KEY)
                .as_deref()
                .and_then(crate::target::SessionTarget::decode),
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
    const SHOT_VIEWPORT: (u32, u32) = (1200, 720);

    #[allow(
        clippy::expect_used,
        reason = "a framework surface without an emulator cannot present a terminal"
    )]
    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        // History mode and library tests present a headless emulator. The live
        // window prefers a real PTY and falls back to that emulator.
        let widget = if cx.read_only() || cfg!(test) {
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
            "targets",
            "environments",
            "host",
        ]
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

#[cfg(test)]
thread_local! {
    static ATTACH_RETAINED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn note_attach_retained(retained: bool) {
    ATTACH_RETAINED.with(|cell| cell.set(retained));
}

impl ResourceSurface<Terminal> for TerminalWidget {
    /// The grid is this PTY's. An empty session projection must not clear it.
    fn present(&mut self, _state: &std::rc::Rc<SessionState>) {}

    fn attach(&mut self, cx: SurfaceCx<Terminal>) {
        self.attach_framework(cx.dispatcher());
        #[cfg(test)]
        note_attach_retained(self.retains_framework_dispatcher());
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
    fn attach_retains_the_dispatcher_and_hosted_lookup_does_not_rebuild_the_document_root() {
        let mut widget = TerminalWidget::spawn_headless().expect("headless terminal");
        assert!(!widget.retains_framework_dispatcher());
        let mut doc = ui::automerge::AutoCommit::new();
        let standalone = widget.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, Terminal::KIND);
        let root_id = standalone[0].id.as_str().to_owned();
        assert!(ui::resources::resource(&widget, &doc, &root_id).is_some());

        widget.apply_session_action(
            &mut doc,
            &TerminalAction::SetTitle {
                title: "Session".into(),
            },
        );
        let title = doc
            .get(automerge::ROOT, "terminal_title")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        assert_eq!(title.as_deref(), Some("Session"));
        assert!(
            widget.last_session_action().is_some(),
            "a standalone session action uses the local action host"
        );

        widget.mark_hosted_for_test();
        assert!(
            ui::resources::resource(&widget, &doc, &root_id).is_none(),
            "hosted lookup does not rebuild the document root"
        );
        let hosted = widget.resources(&doc);
        assert!(
            hosted
                .iter()
                .all(|resource| resource.kind != Terminal::KIND && resource.id.as_str() != root_id),
            "hosted resources must not publish a second terminal root"
        );
        let transcript_id = hosted[0].id.as_str().to_owned();
        let transcript = ui::resources::resource(&widget, &doc, &transcript_id)
            .expect("hosted transcript stays addressable");
        assert_ne!(transcript.kind, Terminal::KIND);

        super::ATTACH_RETAINED.with(|cell| cell.set(false));
        let _app = ui::framework::testing::TestApp::<TerminalShell>::new().expect("terminal shell");
        assert!(
            super::ATTACH_RETAINED.with(std::cell::Cell::get),
            "attach retains the dispatcher"
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
                automerge::Value::Object(_) => None,
            });
        assert_eq!(x, Some(3));
        TerminalAction::WriteLine {
            alternate: false,
            index: 0,
            text: "Hi".into(),
            attrs: vec![0, 1],
        }
        .apply(&mut doc)
        .unwrap();
        let line = doc.get(&grid, 0).unwrap().map(|(_, id)| id).unwrap();
        let text_id = doc.get(&line, "text").unwrap().map(|(_, id)| id).unwrap();
        assert_eq!(doc.text(&text_id).unwrap(), "Hi");
        TerminalAction::InsertBlankLine {
            alternate: false,
            index: 1,
            text: " ".into(),
            attrs: vec![0],
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.length(&grid), 3);
        TerminalAction::TrimGridPrefix { count: 1 }
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.length(&grid), 2);
        TerminalAction::DeleteGridLine {
            alternate: false,
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.length(&grid), 1);
        TerminalAction::PlaceImage {
            hash: "blake3:row".into(),
            mime: "image/png".into(),
            byte_len: 4,
            row: 2,
            col: 0,
            columns: 1,
            rows: 1,
            pixel_width: 1,
            pixel_height: 1,
            protocol_id: None,
            alternate_screen: false,
        }
        .apply(&mut doc)
        .unwrap();
        TerminalAction::ShiftImageRows { removed_rows: 2 }
            .apply(&mut doc)
            .unwrap();
        let images = doc
            .get(automerge::ROOT, "terminal_images")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        let placement = doc.get(&images, 0).unwrap().map(|(_, id)| id).unwrap();
        let row = doc
            .get(&placement, "row")
            .unwrap()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(value) => value.as_u64(),
                automerge::Value::Object(_) => None,
            });
        assert_eq!(row, Some(0));
    }

    #[test]
    fn a_nested_panel_keeps_grid_actions_inside_the_panel() {
        let mut doc = ui::automerge::AutoCommit::new();
        let panel = doc
            .put_object(automerge::ROOT, "terminal_panel", automerge::ObjType::Map)
            .unwrap();
        TerminalAction::InsertBlankLine {
            alternate: false,
            index: 0,
            text: "hi".into(),
            attrs: vec![0, 0],
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(automerge::ROOT, "terminal_grid").unwrap().is_none(),
            "a nested session does not create a second grid at the document root"
        );
        let grid = doc
            .get(&panel, "terminal_grid")
            .unwrap()
            .map(|(_, id)| id)
            .expect("the blank line is inside the panel");
        assert_eq!(doc.length(&grid), 1);
    }

    #[test]
    fn a_nested_panel_claims_its_pane_inside_the_panel() {
        let mut doc = ui::automerge::AutoCommit::new();
        let panel = doc
            .put_object(automerge::ROOT, "terminal_panel", automerge::ObjType::Map)
            .unwrap();
        TerminalAction::ClaimPane {
            pane_id: "pane-1".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(
            doc.get(automerge::ROOT, "terminal_pane_id")
                .unwrap()
                .is_none()
        );
        let id = doc
            .get(&panel, "terminal_pane_id")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        assert_eq!(id.as_deref(), Some("pane-1"));
    }

    #[test]
    fn a_nested_panel_binds_its_local_session_inside_the_panel() {
        use automerge::transaction::Transactable;
        let mut doc = ui::automerge::AutoCommit::new();
        let panel = doc
            .put_object(automerge::ROOT, "terminal_panel", automerge::ObjType::Map)
            .unwrap();
        doc.put(&panel, "terminal_title", "old session").unwrap();
        TerminalAction::BindLocalSession {
            session_id: "process-2".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.get(&panel, "terminal_title").unwrap().is_none());
        assert!(
            doc.get(automerge::ROOT, "terminal_local_session")
                .unwrap()
                .is_none()
        );
        let id = doc
            .get(&panel, "terminal_local_session")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        assert_eq!(id.as_deref(), Some("process-2"));
    }

    #[test]
    fn a_nested_panel_keeps_its_cursor_and_deletes_its_own_line() {
        use automerge::transaction::Transactable;
        let mut doc = ui::automerge::AutoCommit::new();
        let panel = doc
            .put_object(automerge::ROOT, "terminal_panel", automerge::ObjType::Map)
            .unwrap();
        TerminalAction::EnsureStructure.apply(&mut doc).unwrap();
        TerminalAction::SetCursor { x: 4, y: 5 }
            .apply(&mut doc)
            .unwrap();
        TerminalAction::EnsureStructure.apply(&mut doc).unwrap();
        assert!(doc.get(automerge::ROOT, "cursor").unwrap().is_none());
        let cursor = doc
            .get(&panel, "cursor")
            .unwrap()
            .map(|(_, id)| id)
            .expect("cursor stays in the panel");
        let x = doc
            .get(&cursor, "x")
            .unwrap()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(value) => value.as_i64(),
                automerge::Value::Object(_) => None,
            });
        assert_eq!(x, Some(4));
        TerminalAction::EnsureLines {
            alternate: false,
            min_len: 2,
        }
        .apply(&mut doc)
        .unwrap();
        TerminalAction::DeleteGridLine {
            alternate: false,
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        let grid = doc
            .get(&panel, "terminal_grid")
            .unwrap()
            .map(|(_, id)| id)
            .unwrap();
        assert_eq!(doc.length(&grid), 1);
    }
}
