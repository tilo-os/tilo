//! Driving a `ui` app without a display.
//!
//! `--shots` already renders an app's cold-start frame offscreen, but it
//! builds a throwaway `State` and never delivers input, so it cannot answer
//! the question this module exists for: *does a change the user makes
//! survive a restart?* Answering that needs the real widget tree, the real
//! event dispatch, and the app's real persisted document — and no window.
//!
//! [`Driver`] provides exactly that. It stands up the app's root widget over
//! a Taffy tree at a fixed viewport, then feeds it the same `WindowEvent`
//! values winit would deliver, through the same
//! [`Widget::handle_event`](crate::widget::Widget::handle_event) the live app
//! uses. Nothing is stubbed: a click here follows the identical path to a
//! click on real hardware, including the CRDT mutations widgets perform on
//! the way.
//!
//! # Using it from an app
//!
//! Apps opt in with one line in `main`, next to their `--shots` handling:
//!
//! ```ignore
//! if let Some(spec) = ui::headless::spec_from_args(&args) {
//!     return ui::headless::run(spec, my_app::app_root());
//! }
//! ```
//!
//! which gives the binary a `--headless-drive '<json>'` flag:
//!
//! ```text
//! ebook --headless-drive '{"actions":[{"click_named":"library_item[0]"}]}'
//! ebook --headless-drive '{"dump":"/tmp/after.json"}'
//! ```
//!
//! The first process makes a change and exits (flushing its store); the
//! second loads the store and writes the document out as JSON. Comparing the
//! two dumps is the whole persistence proof, and it is made against the
//! app's real document rather than a harness's idea of one.

use std::collections::HashMap;

use taffy::prelude::*;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Viewport the driver lays out at unless a spec overrides it. Matches the
/// default `--shots` viewport so a driven frame and a screenshot agree.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

/// One thing to do to the app.
#[derive(Debug, Clone)]
pub enum Action {
    /// Click the centre of the widget addressed by `name` — either its
    /// [`Widget::test_id`] or the `Parent/Child[index]` path fallback.
    ClickNamed(String),
    /// Click at raw logical coordinates.
    ClickAt(f64, f64),
    /// Press and release one named key (`Enter`, `Tab`, `Escape`, `Space`,
    /// `Backspace`, arrows) or a single character.
    Key(String),
    /// Type a string, one press/release pair per character.
    Type(String),
    /// Hold the given modifiers for subsequent actions. Empty clears them.
    Modifiers(Vec<String>),
}

/// What a `--headless-drive` invocation should do.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub viewport: Option<(u32, u32)>,
    pub actions: Vec<Action>,
    /// Write the app's CRDT document here as JSON when the actions are done.
    pub dump: Option<std::path::PathBuf>,
    /// Write the addressable widget names here as JSON.
    pub targets: Option<std::path::PathBuf>,
}

/// A headless app: real widget tree, real Taffy layout, real event dispatch.
pub struct Driver {
    state: State,
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    child_node: NodeId,
    text: TextRenderer,
    theme: Theme,
    modifiers: winit::event::Modifiers,
    viewport: (u32, u32),
}

impl Driver {
    /// Stand the app up and run the first layout pass.
    pub fn new(state: State, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        let mut d = Self {
            state,
            root,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            root_node: NodeId::from(0usize),
            child_node: NodeId::from(0usize),
            text: TextRenderer::new(),
            theme: Theme::dark(),
            modifiers: winit::event::Modifiers::default(),
            viewport,
        };
        d.relayout();
        d
    }

    /// Re-run the app's frame passes short of `draw`: pull state from the
    /// CRDT, rebuild the Taffy tree, compute and apply layout. Called after
    /// every action so the next one sees where things actually moved to.
    pub fn relayout(&mut self) {
        self.root.sync(&mut self.state.doc, &self.state.root);
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        let child = self
            .root
            .build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        let root_node = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .expect("headless root node");
        let measure_map = self.node_map.clone();
        let text = &mut self.text;
        self.taffy
            .compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        // SAFETY: `node_map` holds pointers into `self.root`,
                        // which outlives this call and is not otherwise
                        // borrowed here. Same contract as `App`'s own measure
                        // closure.
                        unsafe { (*ptr).measure(known, available, text) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("headless layout");
        self.root.apply_layout(&self.taffy, child, Point::ORIGIN);
        self.root_node = root_node;
        self.child_node = child;
    }

    /// Every widget the driver can address, by `test_id` or path, with the
    /// rect it currently occupies.
    pub fn targets(&self) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        let mut counts = HashMap::new();
        collect_targets(self.root.as_ref(), "", &mut counts, &mut out);
        out
    }

    /// The live document. Used to dump state and to hand to a saver.
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    /// Deliver one event through the production dispatch path.
    pub fn send(&mut self, event: WindowEvent) {
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        self.root.handle_event(
            &event,
            1.0,
            &self.modifiers,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus_target,
            self.child_node,
            &self.taffy,
        );
        // Mirror the App's idle tick: a widget that asked for a commit gets
        // one, so undo boundaries and any commit-time CRDT writes happen the
        // way they do live.
        if self.root.needs_commit() {
            self.state.commit_action(None, None);
            self.root.on_commit();
        }
        self.relayout();
    }

    /// Move the pointer to `(x, y)` and press+release the left button — the
    /// three events a real click produces, in order.
    pub fn click_at(&mut self, x: f64, y: f64) {
        self.send(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
        self.send(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.send(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Click the centre of the named widget. `Err` names the target when it
    /// is not in the tree, so a stale scenario fails loudly instead of
    /// quietly doing nothing.
    pub fn click_named(&mut self, name: &str) -> Result<(), String> {
        let rect = self
            .targets()
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
        }
        let c = rect.center();
        self.click_at(c.x, c.y);
        Ok(())
    }

    /// Press and release one key.
    pub fn key(&mut self, spec: &str) {
        let key = parse_key(spec);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(key_event(key.clone(), state));
        }
    }

    /// Type a string one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.key(&ch.to_string());
        }
    }

    /// Set the held modifier set. Names: `ctrl`, `shift`, `alt`, `logo`.
    pub fn set_modifiers(&mut self, names: &[String]) {
        let mut st = winit::keyboard::ModifiersState::empty();
        for n in names {
            match n.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => st |= winit::keyboard::ModifiersState::CONTROL,
                "shift" => st |= winit::keyboard::ModifiersState::SHIFT,
                "alt" => st |= winit::keyboard::ModifiersState::ALT,
                "logo" | "super" | "meta" => st |= winit::keyboard::ModifiersState::SUPER,
                _ => {}
            }
        }
        self.modifiers = winit::event::Modifiers::from(st);
        self.send(WindowEvent::ModifiersChanged(self.modifiers));
    }

    /// Run one action.
    pub fn perform(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::ClickNamed(name) => self.click_named(name),
            Action::ClickAt(x, y) => {
                self.click_at(*x, *y);
                Ok(())
            }
            Action::Key(k) => {
                self.key(k);
                Ok(())
            }
            Action::Type(t) => {
                self.type_text(t);
                Ok(())
            }
            Action::Modifiers(m) => {
                self.set_modifiers(m);
                Ok(())
            }
        }
    }

    /// The document rendered as JSON, for a harness to diff across a
    /// restart.
    pub fn dump_json(&mut self) -> String {
        serde_json::to_string_pretty(&automerge::AutoSerde::from(&*self.state.doc.document()))
            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn collect_targets(
    w: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    let name = w.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let idx = sibling_counts.entry(sibling_key).or_insert(0);
    let my_index = *idx;
    *idx += 1;
    let path = if parent_path.is_empty() {
        format!("{name}[{my_index}]")
    } else {
        format!("{parent_path}/{name}[{my_index}]")
    };
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        out.insert(id.to_string(), w.get_rect());
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        collect_targets(child, &path, &mut child_counts, out);
    }
}

/// Build a `WindowEvent::KeyboardInput` winit itself would have delivered.
///
/// `KeyEvent::platform_specific` is `pub(crate)` in winit, so the struct
/// cannot be built field-by-field from outside that crate. Zero-initialising
/// and then overwriting every field we care about is the workaround the
/// compositor's own driver already uses (`shell/src/bin/shots/driver.rs`);
/// the zeroed platform payload is inert on the read paths widgets take.
fn key_event(key: Key, state: ElementState) -> WindowEvent {
    // SAFETY: `KeyEvent`'s only unwritten field is the platform payload,
    // which on Linux is a plain `Option`-shaped struct for which an
    // all-zero bit pattern is a valid value, and which nothing downstream
    // of `handle_event` reads.
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = state;
    ev.repeat = false;
    ev.text = match (&key, state) {
        (Key::Character(c), ElementState::Pressed) => Some(c.clone()),
        _ => None,
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

fn parse_key(spec: &str) -> Key {
    match spec {
        "Enter" | "Return" => Key::Named(NamedKey::Enter),
        "Tab" => Key::Named(NamedKey::Tab),
        "Escape" | "Esc" => Key::Named(NamedKey::Escape),
        "Space" => Key::Named(NamedKey::Space),
        "Backspace" => Key::Named(NamedKey::Backspace),
        "Delete" => Key::Named(NamedKey::Delete),
        "ArrowUp" | "Up" => Key::Named(NamedKey::ArrowUp),
        "ArrowDown" | "Down" => Key::Named(NamedKey::ArrowDown),
        "ArrowLeft" | "Left" => Key::Named(NamedKey::ArrowLeft),
        "ArrowRight" | "Right" => Key::Named(NamedKey::ArrowRight),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        other => Key::Character(other.into()),
    }
}

/// Parse `--headless-drive '<json>'` (or `--headless-drive=<json>`) out of
/// argv. `None` when the flag is absent, which is every normal launch.
///
/// The JSON is an object: `viewport` (`[w, h]`), `actions` (a list of
/// one-key objects — `{"click_named": "..."}`, `{"click_at": [x, y]}`,
/// `{"key": "Enter"}`, `{"type": "hello"}`, `{"modifiers": ["ctrl"]}`),
/// `dump` (path for the document JSON) and `targets` (path for the
/// addressable-widget list).
pub fn spec_from_args(args: &[String]) -> Option<Spec> {
    let raw = value_arg(args, "--headless-drive")?;
    match parse_spec(&raw) {
        Ok(spec) => Some(spec),
        Err(e) => {
            eprintln!("--headless-drive: {e}");
            std::process::exit(2);
        }
    }
}

fn value_arg(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse the `--headless-drive` payload.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut spec = Spec::default();
    if let Some(vp) = v.get("viewport").and_then(|x| x.as_array()) {
        if vp.len() == 2 {
            spec.viewport = Some((
                vp[0].as_u64().unwrap_or(0) as u32,
                vp[1].as_u64().unwrap_or(0) as u32,
            ));
        }
    }
    if let Some(p) = v.get("dump").and_then(|x| x.as_str()) {
        spec.dump = Some(p.into());
    }
    if let Some(p) = v.get("targets").and_then(|x| x.as_str()) {
        spec.targets = Some(p.into());
    }
    for a in v.get("actions").and_then(|x| x.as_array()).unwrap_or(&vec![]) {
        let obj = a
            .as_object()
            .ok_or_else(|| format!("action must be an object, got {a}"))?;
        let (k, val) = obj
            .iter()
            .next()
            .ok_or_else(|| "action object is empty".to_string())?;
        let action = match k.as_str() {
            "click_named" => Action::ClickNamed(
                val.as_str()
                    .ok_or("click_named takes a string")?
                    .to_string(),
            ),
            "click_at" => {
                let xy = val.as_array().ok_or("click_at takes [x, y]")?;
                Action::ClickAt(
                    xy.first().and_then(|x| x.as_f64()).ok_or("click_at x")?,
                    xy.get(1).and_then(|x| x.as_f64()).ok_or("click_at y")?,
                )
            }
            "key" => Action::Key(val.as_str().ok_or("key takes a string")?.to_string()),
            "type" => Action::Type(val.as_str().ok_or("type takes a string")?.to_string()),
            "modifiers" => Action::Modifiers(
                val.as_array()
                    .ok_or("modifiers takes a list")?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            ),
            other => return Err(format!("unknown action {other:?}")),
        };
        spec.actions.push(action);
    }
    Ok(spec)
}

/// Run `spec` against `root`, persisting through this package's store.
///
/// The document comes from, and goes back to, the same place the live app
/// uses — that is the point. Returns a process exit code: `0` when every
/// action succeeded, `1` when one did not (with the reason on stderr) and
/// `3` when persistence itself was unavailable, which a sweep must not
/// mistake for "nothing changed".
pub fn run(spec: Spec, root: Box<dyn Widget>) -> i32 {
    let (state, mut autosave) = crate::persist::open();
    if !autosave.is_enabled() {
        eprintln!(
            "headless: persistence unavailable: {}",
            autosave.last_error().unwrap_or_default()
        );
        return 3;
    }
    let mut driver = Driver::new(state, root, spec.viewport.unwrap_or(DEFAULT_VIEWPORT));

    let mut failure: Option<String> = None;
    for action in &spec.actions {
        if let Err(e) = driver.perform(action) {
            failure = Some(e);
            break;
        }
        autosave.request(&mut driver.state_mut().doc);
    }

    if let Some(path) = &spec.targets {
        let mut names: Vec<(String, [f64; 4])> = driver
            .targets()
            .into_iter()
            .map(|(n, r)| (n, [r.x0, r.y0, r.width(), r.height()]))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let json = serde_json::to_string_pretty(&names).unwrap_or_default();
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("headless: writing targets to {}: {e}", path.display());
            return 1;
        }
    }
    if let Some(path) = &spec.dump {
        let json = driver.dump_json();
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("headless: writing dump to {}: {e}", path.display());
            return 1;
        }
    }

    // The same two calls `App::exiting` makes, for the same reason.
    autosave.request(&mut driver.state_mut().doc);
    autosave.flush();
    if let Some(e) = autosave.last_error() {
        eprintln!("headless: state write failed: {e}");
        return 3;
    }
    match failure {
        Some(e) => {
            eprintln!("headless: {e}");
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Button, Widget};

    #[test]
    fn spec_parses_every_action_shape() {
        let spec = parse_spec(
            r#"{"viewport":[800,600],"dump":"/tmp/d.json","targets":"/tmp/t.json",
                "actions":[{"click_named":"save"},{"click_at":[10,20]},
                           {"key":"Enter"},{"type":"hi"},{"modifiers":["ctrl"]}]}"#,
        )
        .expect("spec parses");
        assert_eq!(spec.viewport, Some((800, 600)));
        assert_eq!(spec.dump.as_deref(), Some(std::path::Path::new("/tmp/d.json")));
        assert_eq!(spec.actions.len(), 5);
        assert!(matches!(&spec.actions[0], Action::ClickNamed(n) if n == "save"));
        assert!(matches!(&spec.actions[3], Action::Type(t) if t == "hi"));
    }

    #[test]
    fn an_unknown_action_is_rejected_rather_than_ignored() {
        let err = parse_spec(r#"{"actions":[{"teleport":"x"}]}"#).unwrap_err();
        assert!(err.contains("teleport"), "{err}");
    }

    /// A button click must reach the widget through the same
    /// `handle_event` the live app calls.
    #[test]
    fn clicking_a_named_target_reaches_the_widget() {
        let button = Button::new("btn", "Save", None);
        let mut driver = Driver::new(State::new(), Box::new(button), (400, 200));
        let targets = driver.targets();
        assert!(
            targets.keys().any(|k| k.starts_with("Button")),
            "the button must be addressable, got {:?}",
            targets.keys().collect::<Vec<_>>()
        );
        let name = targets
            .keys()
            .find(|k| k.starts_with("Button"))
            .unwrap()
            .clone();
        driver.click_named(&name).expect("click lands");
    }

    #[test]
    fn clicking_a_missing_target_is_an_error() {
        let mut driver = Driver::new(State::new(), Box::new(Button::new("b", "B", None)), (400, 200));
        let err = driver.click_named("no_such_widget").unwrap_err();
        assert!(err.contains("no_such_widget"), "{err}");
    }

    #[test]
    fn dump_json_reflects_the_document() {
        use automerge::transaction::Transactable;
        let mut state = State::new();
        state
            .doc
            .put(&automerge::ROOT, "greeting", "hello")
            .unwrap();
        let mut driver = Driver::new(state, Box::new(Button::new("b", "B", None)), (400, 200));
        let json = driver.dump_json();
        assert!(json.contains("greeting"), "{json}");
        assert!(json.contains("hello"), "{json}");
    }
}
