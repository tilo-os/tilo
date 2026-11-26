//! A tilolang app's window as an agent reads it (`outline`): what it shows, as lines.
//!
//! The lines are written from the views the instance shows ([`Instance::shown_now`]), the same
//! trees, rows and bound values its window is sent, so they are exact, never read back from
//! pixels: each part's words and the value it holds, the resource each level shows, and for each
//! control the address its action is called by (`<app>/<resource>#<action>`), as the dispatch
//! takes it. An agent reads the window, calls the action a line names, and reads the change in
//! the next outline. Nothing of the person's window moves, wherever it is drawn or whether it is
//! drawn at all.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use tilo_render_protocol::patch::{Level, Node, ToUi, Value};
use tilolang_tilo::instance::Instance;

/// The most lines one outline carries: enough for a whole window, the rest counted rather than
/// shown, so an answer stays in an agent's context.
const MAX_LINES: usize = 2_000;

/// The most characters of one part's words or value an outline keeps.
const MAX_WORDS: usize = 240;

/// The node kinds that only arrange what they hold: they are no line of their own unless they
/// carry words, and what they hold reads as their parent's.
const ARRANGING: [&str; 12] = [
    "part",
    "page",
    "stack",
    "fragment",
    "row",
    "column",
    "grid",
    "ui.Scroll",
    "ui.Stack",
    "ui.Row",
    "ui.Column",
    "spacer",
];

/// What `instance`'s window shows now, as the `outline` operation answers it.
#[must_use]
pub fn read(instance: &Instance) -> tilo_actions::ActionOutput {
    let shown = instance.shown_now();
    let mut reading = Reading {
        app: instance.app(),
        bound: BTreeMap::new(),
        rows: BTreeMap::new(),
        text: String::new(),
        lines: 0,
        omitted: 0,
        protected: false,
    };
    for message in &shown {
        match message {
            ToUi::Bound {
                level, key, value, ..
            } => {
                reading.bound.insert((*level, key.as_str()), value);
            }
            ToUi::Rows {
                level,
                list,
                len,
                rows,
                ..
            } => {
                reading.rows.insert((*level, list.as_str()), (rows, *len));
            }
            _ => {}
        }
    }
    let mut tree = Vec::new();
    for message in &shown {
        if let ToUi::Open {
            level,
            target,
            tree: view,
            capture_protected,
            ..
        } = message
        {
            reading.protected = *capture_protected;
            let address = reading.address(&target.id);
            let level_name = match level {
                Level::Root => "window",
                Level::Detail => "detail",
            };
            reading.line(0, &format!("{level_name} shows {address}"));
            let mut children = Vec::new();
            reading.node(*level, view, 1, &mut children);
            tree.push(serde_json::json!({
                "role": level_name,
                "resource": address,
                "children": children,
            }));
        }
    }
    if tree.is_empty() {
        // Nothing is open to read: say why, rather than answer an empty window. A window still
        // starting reads busy, which a reader reads again until it settles.
        let (line, state) = match (instance.open_failed(), instance.host()) {
            (Some(why), _) => (format!("window (the app did not open: {why})"), None),
            (None, None) => (
                "window [busy] (the app is still starting)".to_owned(),
                Some("busy"),
            ),
            (None, Some(_)) => ("window (the app shows no view)".to_owned(), None),
        };
        reading.line(0, &line);
        tree.push(
            serde_json::json!({ "role": "window", "state": state.into_iter().collect::<Vec<_>>() }),
        );
    }
    tilo_actions::ActionOutput {
        value: Some(serde_json::json!({
            "outline": reading.text,
            "tree": tree,
            "nodes": reading.lines,
            "omitted": reading.omitted,
        })),
        resources: Vec::new(),
    }
}

/// An outline being written: what the window holds besides its trees, and the lines so far.
struct Reading<'a> {
    app: &'a str,
    /// The values bound controls hold, by level and node key.
    bound: BTreeMap<(Level, &'a str), &'a Value>,
    /// The rows each virtual list shows, by level and list key, with how many it has.
    rows: BTreeMap<(Level, &'a str), (&'a Vec<Node>, u32)>,
    text: String,
    lines: usize,
    omitted: usize,
    /// The level being read keeps what it shows off captures: its values are not said.
    protected: bool,
}

impl Reading<'_> {
    /// The address the dispatch calls the record `id` of this app by.
    fn address(&self, id: &str) -> String {
        format!(
            "{}/{}",
            self.app,
            tilolang_tilo::naming::resource_id(self.app, id)
        )
    }

    fn line(&mut self, depth: usize, words: &str) -> bool {
        if self.lines >= MAX_LINES {
            self.omitted = self.omitted.saturating_add(1);
            return false;
        }
        self.lines = self.lines.saturating_add(1);
        let _ = writeln!(self.text, "{}{words}", "  ".repeat(depth));
        true
    }

    /// Write `node` at `depth`, and what it holds beneath it, into `tree` as well.
    fn node(&mut self, level: Level, node: &Node, depth: usize, tree: &mut Vec<serde_json::Value>) {
        let read = self.read(level, node);
        let (depth, into) = match &read {
            Some(read) if self.line(depth, &read.line()) => {
                let at = tree.len();
                tree.push(read.json());
                (depth.saturating_add(1), Some(at))
            }
            Some(_) => return,
            None => (depth, None),
        };
        let mut children = Vec::new();
        let rows = self.rows.get(&(level, node.key.as_str())).copied();
        for child in &node.children {
            self.node(level, child, depth, &mut children);
        }
        if let Some((rows, len)) = rows {
            for row in rows {
                self.node(level, row, depth, &mut children);
            }
            let more = usize::try_from(len)
                .unwrap_or(usize::MAX)
                .saturating_sub(rows.len());
            if more > 0 {
                self.line(depth, &format!("… {more} more rows"));
            }
        }
        match into.and_then(|at| tree.get_mut(at)) {
            Some(parent) => {
                if !children.is_empty() {
                    parent["children"] = serde_json::Value::Array(children);
                }
            }
            None => tree.extend(children),
        }
    }

    /// What `node` says as a line of its own, or `None` for a node that only arranges.
    fn read(&self, level: Level, node: &Node) -> Option<Read> {
        let text = |name: &str| match node.props.get(name) {
            Some(Value::Text(text)) if !text.trim().is_empty() => Some(clip(text)),
            _ => None,
        };
        let name = text("label")
            .or_else(|| text("title"))
            .or_else(|| text("name"));
        let bound = self.bound.get(&(level, node.key.as_str())).copied();
        let secret = self.protected
            || [Some("secret"), Some("protected")]
                .into_iter()
                .flatten()
                .any(|flag| matches!(node.props.get(flag), Some(Value::Bool(true))));
        let value = if secret {
            None
        } else {
            bound
                .or_else(|| node.props.get("value"))
                .or_else(|| node.props.get("text"))
                .and_then(said)
        };
        let mut actions = Vec::new();
        for event in node.events.values() {
            if let Some(invokes) = &event.invokes {
                actions.push(format!(
                    "{}#{}",
                    self.address(&invokes.resource),
                    invokes.action
                ));
            } else if let Some((record, field)) = &event.toggles {
                actions.push(format!(
                    "{}#edit-flag {{\"field\": \"{field}\"}}",
                    self.address(record)
                ));
            }
        }
        if let Some((record, field)) = node.binding.as_ref().and_then(|binding| binding.field()) {
            let verb = if matches!(bound, Some(Value::Bool(_))) {
                "edit-flag"
            } else {
                "edit-text"
            };
            actions.push(format!(
                "{}#{verb} {{\"field\": \"{field}\"}}",
                self.address(record)
            ));
        }
        actions.sort();
        actions.dedup();
        let mut state = Vec::new();
        if matches!(node.props.get("enabled"), Some(Value::Bool(false))) {
            state.push("disabled");
        }
        if matches!(node.props.get("selected"), Some(Value::Bool(true))) {
            state.push("selected");
        }
        let arranges = ARRANGING.contains(&node.kind.as_str());
        if arranges && name.is_none() && value.is_none() && actions.is_empty() {
            return None;
        }
        if name.is_none() && value.is_none() && actions.is_empty() && node.children.is_empty() {
            return None;
        }
        Some(Read {
            role: role(&node.kind),
            name,
            value,
            state,
            actions,
        })
    }
}

/// One line of an outline.
struct Read {
    role: String,
    name: Option<String>,
    value: Option<String>,
    state: Vec<&'static str>,
    actions: Vec<String>,
}

impl Read {
    /// `<role> "<name>": "<value>" (<state>) → <action>`, each part where it has one.
    fn line(&self) -> String {
        let mut line = self.role.clone();
        match (&self.name, &self.value) {
            (Some(name), Some(value)) => {
                let _ = write!(line, " {name:?}: {value:?}");
            }
            (Some(words), None) | (None, Some(words)) => {
                let _ = write!(line, " {words:?}");
            }
            (None, None) => {}
        }
        if !self.state.is_empty() {
            let _ = write!(line, " ({})", self.state.join(", "));
        }
        for action in &self.actions {
            let _ = write!(line, " → {action}");
        }
        line
    }

    fn json(&self) -> serde_json::Value {
        let mut node = serde_json::Map::new();
        node.insert("role".into(), self.role.clone().into());
        if let Some(name) = &self.name {
            node.insert("name".into(), name.clone().into());
        }
        if let Some(value) = &self.value {
            node.insert("value".into(), value.clone().into());
        }
        if !self.state.is_empty() {
            node.insert("state".into(), self.state.clone().into());
        }
        if !self.actions.is_empty() {
            node.insert("actions".into(), self.actions.clone().into());
        }
        node.into()
    }
}

/// A node kind as a role: `ui.TextField` reads `text_field`, the window's shell `window`.
fn role(kind: &str) -> String {
    let kind = kind.strip_prefix("ui.").unwrap_or(kind);
    if kind == "NavShell" {
        return "window".to_owned();
    }
    let mut role = String::new();
    for (at, character) in kind.chars().enumerate() {
        if character.is_uppercase() {
            if at > 0 {
                role.push('_');
            }
            role.extend(character.to_lowercase());
        } else {
            role.push(character);
        }
    }
    role
}

/// What `value` says, as words: `None` for a value that says nothing a line can carry.
fn said(value: &Value) -> Option<String> {
    let words = match value {
        Value::Text(text) | Value::Enum(text) => text.clone(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Opt(inner) => return inner.as_deref().and_then(said),
        Value::Ref(resource) => resource.id.clone(),
        Value::Rich(rich) => rich
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::List(items) => items.iter().filter_map(said).collect::<Vec<_>>().join(", "),
        _ => return None,
    };
    (!words.trim().is_empty()).then(|| clip(&words))
}

fn clip(words: &str) -> String {
    if words.chars().count() <= MAX_WORDS {
        return words.to_owned();
    }
    let mut kept: String = words.chars().take(MAX_WORDS).collect();
    kept.push('…');
    kept
}
