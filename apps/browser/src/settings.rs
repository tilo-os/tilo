//! Browser settings — a persistent preferences model plus the
//! [`SettingsPane`] widget that edits it.
//!
//! Two layers live here:
//!
//!  * [`Settings`] — a plain serde struct persisted as JSON under
//!    `dirs::config_dir()/qos-browser/settings.json`. It holds the
//!    durable preferences (ad-blocking, clear-on-exit, search engine,
//!    homepage) and knows how to [`Settings::load`] / [`Settings::save`]
//!    itself.
//!
//!  * [`SettingsPane`] — a `ui` [`Widget`] that renders the preferences
//!    surface. Like [`crate::browser_widget::BrowserWidget`] it holds no
//!    logical state of its own: the toggles and text fields read/write
//!    directly through the Automerge CRDT under stable string paths (see
//!    the `*_PATH` / `*_FLAG` constants), and the two "clear now" buttons
//!    raise boolean click-flags.
//!
//! The chrome (the orchestrator) owns the wiring: it polls
//! [`SettingsPane::take_pending`] once per frame to pull the toggle
//! states and the one-shot clear-history / clear-cookies intents, then
//! performs the actual cookie/data clearing in the engine and persists
//! the [`Settings`] back to disk. The pane never touches the engine.

use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, Column, Container, Row, ScrollAxis, Scrollable, Spacer, Text, TextInput, Toggle, Widget,
};

/// Resource bindings injected by Browser when this pane is hosted. They keep
/// pointer, keyboard and assistive-tech changes on the exact same action path
/// used by Ctrl+K and agents.
#[derive(Clone, Default)]
pub struct SettingsActionBindings {
    pub adblock: Option<qos_actions::ActionBinding<bool>>,
    pub clear_on_exit: Option<qos_actions::ActionBinding<bool>>,
    pub homepage: Option<qos_actions::ActionBinding<String>>,
    pub search_engine: Option<qos_actions::ActionBinding<String>>,
    pub clear_history: Option<qos_actions::ActionBinding<()>>,
    pub clear_site_data: Option<qos_actions::ActionBinding<()>>,
}

/// Boxed-widget currency the `ui` tree speaks.
type El = Box<dyn Widget>;

// === CRDT paths ======================================================
// Flat scalar/text paths written on the app root object, mirroring the
// `browser.*` convention in `browser_widget.rs`. Exposed so the chrome
// can reference the same keys when it seeds or inspects the document.

/// Boolean toggle: block ads & trackers. Written by the ad-block [`Toggle`].
pub const ADBLOCK_PATH: &str = "settings.adblock";
/// Boolean toggle: clear history & cookies when the browser exits.
pub const CLEAR_ON_EXIT_PATH: &str = "settings.clear_on_exit";
/// Click-flag raised by the "Clear history now" button. Reset on read.
pub const CLEAR_HISTORY_FLAG: &str = "settings.clear_history_clicked";
/// Click-flag raised by the "Clear cookies & site data now" button.
pub const CLEAR_COOKIES_FLAG: &str = "settings.clear_cookies_clicked";
/// Automerge `Text` object holding the search-engine query template.
pub const SEARCH_ENGINE_PATH: &str = "settings.search_engine";
/// Automerge `Text` object holding the homepage URL.
pub const HOMEPAGE_PATH: &str = "settings.homepage";
/// Click-flag raised by the "Done" button to close the settings pane.
pub const CLOSE_FLAG: &str = "settings.close_clicked";

/// Default search-engine query template. The `?q=` suffix lets the
/// chrome build a search URL by appending typed terms.
pub const DEFAULT_SEARCH_ENGINE: &str = "https://duckduckgo.com/?q=";
/// Default homepage — matches the browser's startup URL.
pub const DEFAULT_HOMEPAGE: &str = "https://duckduckgo.com";

// === Persistent model ================================================

/// Durable browser preferences, persisted as JSON.
///
/// Defaults: ad-blocking on, clear-on-exit off, `DuckDuckGo` for both
/// search and homepage. The struct is the single source of truth the
/// chrome persists — the [`SettingsPane`] only edits the live CRDT
/// mirror of these fields.
///
/// Persistence goes through `serde_json::Value` (see [`Settings::to_json`]
/// / [`Settings::from_json`]) rather than a `#[derive(Serialize)]`: the
/// browser crate depends on `serde_json` but not on `serde` with its
/// derive feature, so the JSON shape is built explicitly here. The
/// on-disk format is a flat object with the four field names as keys,
/// identical to what a derive would emit — a future `serde` dependency
/// could swap in the derive without changing the file format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Block ads & trackers via the engine's content blocker.
    pub adblock_enabled: bool,
    /// Clear browsing history & cookies on exit.
    pub clear_on_exit: bool,
    /// Search-engine query template (e.g. `https://duckduckgo.com/?q=`).
    pub search_engine: String,
    /// Homepage URL opened on startup / new tab.
    pub homepage: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            adblock_enabled: true,
            clear_on_exit: false,
            search_engine: DEFAULT_SEARCH_ENGINE.to_string(),
            homepage: DEFAULT_HOMEPAGE.to_string(),
        }
    }
}

impl Settings {
    /// Absolute path of the on-disk settings file:
    /// `dirs::config_dir()/qos-browser/settings.json`. `None` when the
    /// platform has no config directory.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("qos-browser").join("settings.json"))
    }

    /// Load the settings from the standard config path. Any failure —
    /// missing file, unreadable path, malformed JSON — falls back to
    /// [`Settings::default`] so a first run (or a corrupt file) never
    /// blocks startup.
    pub fn load() -> Settings {
        match Self::config_path() {
            Some(path) => Self::load_from(&path),
            None => Settings::default(),
        }
    }

    /// Persist the settings to the standard config path, creating the
    /// parent directory if needed. Errors are swallowed (best-effort
    /// persistence) so a read-only home can't crash the browser.
    pub fn save(&self) {
        if let Some(path) = Self::config_path() {
            let _ = self.save_to(&path);
        }
    }

    /// Load from an explicit path (injectable for tests). Returns the
    /// default settings on any I/O or parse error, and fills any missing
    /// field from the default (forward-compatible with older files).
    pub fn load_from(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(|v| Self::from_json(&v))
            .unwrap_or_default()
    }

    /// Serialize to pretty JSON and write to an explicit path, creating
    /// parent directories as required. Returns the underlying I/O error
    /// on failure so tests can assert on it.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory can't be created or if the
    /// write itself fails.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.to_json())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Build the JSON representation persisted to disk — a flat object
    /// keyed by the field names.
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "adblock_enabled": self.adblock_enabled,
            "clear_on_exit": self.clear_on_exit,
            "search_engine": self.search_engine,
            "homepage": self.homepage,
        })
    }

    /// Reconstruct settings from parsed JSON, falling back to the
    /// per-field default for any key that is missing or the wrong type.
    fn from_json(v: &serde_json::Value) -> Settings {
        let d = Settings::default();
        Settings {
            adblock_enabled: v
                .get("adblock_enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(d.adblock_enabled),
            clear_on_exit: v
                .get("clear_on_exit")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(d.clear_on_exit),
            search_engine: v
                .get("search_engine")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .unwrap_or(d.search_engine),
            homepage: v
                .get("homepage")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .unwrap_or(d.homepage),
        }
    }
}

// === Intent API ======================================================

/// One frame's worth of settings intents, produced by
/// [`SettingsPane::take_pending`] for the chrome to act on.
///
/// The two `clear_*` fields are edge-triggered one-shots: they are
/// `true` only on the frame the corresponding button was clicked, and
/// [`SettingsPane::take_pending`] resets the underlying CRDT flag as it
/// reads it. The two toggle fields are level values: `Some(state)`
/// reflects the current toggle position (or `None` before the pane has
/// seeded the document), so the chrome idempotently mirrors them into
/// its [`Settings`] and the engine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsIntents {
    /// The "Clear history now" button was pressed this frame.
    pub clear_history: bool,
    /// The "Clear cookies & site data now" button was pressed this frame.
    pub clear_cookies: bool,
    /// Current ad-block toggle state, or `None` before bootstrap.
    pub adblock_enabled: Option<bool>,
    /// Current clear-on-exit toggle state, or `None` before bootstrap.
    pub clear_on_exit: Option<bool>,
    /// Current search-engine query template.
    pub search_engine: Option<String>,
    /// Current homepage URL.
    pub homepage: Option<String>,
    /// The "Done" button was pressed this frame — close the pane.
    pub close: bool,
}

/// Read a boolean scalar at `path`, returning `None` when the path is
/// absent or holds a non-boolean value.
fn read_bool(state: &AutoCommit, root: &ObjId, path: &str) -> Option<bool> {
    match state.get(root, path) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn read_text(state: &AutoCommit, root: &ObjId, path: &str) -> Option<String> {
    let (value, object) = state.get(root, path).ok()??;
    matches!(value, Value::Object(automerge::ObjType::Text))
        .then(|| state.text(&object).ok())
        .flatten()
}

/// Read a click-flag and reset it to `false` if it was raised, returning
/// whether it had been raised. Mirrors the read-and-reset the browser's
/// `sync` does inline for its `browser.*_clicked` flags, but factored so
/// the chrome can poll it without owning the widget.
fn take_flag(state: &mut AutoCommit, root: &ObjId, path: &str) -> bool {
    let raised = matches!(read_bool(state, root, path), Some(true));
    if raised {
        let _ = state.put(root, path, false);
    }
    raised
}

// === Widget ==========================================================

/// The browser's settings surface: a scrollable column of preference
/// controls. Holds only render/layout caches — every editable value
/// lives in the CRDT under the `settings.*` paths above.
pub struct SettingsPane {
    rect: Rect,
    /// Initial values seeded into the CRDT on first `sync`. After
    /// bootstrap the CRDT is authoritative; this is only the seed.
    initial: Settings,
    /// Composed child tree, rebuilt on every `build_layout`.
    composed: Option<Box<dyn Widget>>,
    /// True once the `settings.*` paths have been seeded into the CRDT.
    bootstrap_done: bool,
    actions: SettingsActionBindings,
}

impl SettingsPane {
    /// Construct a pane seeded from [`Settings::default`].
    pub fn new() -> Self {
        Self::with_settings(Settings::default())
    }

    /// Construct a pane whose CRDT is seeded from `settings` on first
    /// `sync`. The chrome passes the on-disk [`Settings`] here so the
    /// toggles/fields open reflecting the persisted state.
    pub fn with_settings(settings: Settings) -> Self {
        Self {
            rect: Rect::ZERO,
            initial: settings,
            composed: None,
            bootstrap_done: false,
            actions: SettingsActionBindings::default(),
        }
    }

    /// Route hosted controls through Browser's canonical resource actions.
    #[must_use]
    pub fn with_action_bindings(mut self, actions: SettingsActionBindings) -> Self {
        self.actions = actions;
        self
    }

    /// Poll the pending settings intents for this frame and reset the
    /// one-shot click-flags. Called by the chrome once per frame with
    /// the same `state`/`root` handles the widget tree is driven with.
    ///
    /// The `clear_*` fields are edge-triggered (true only on the click
    /// frame; the flag is reset here). The toggle fields are level
    /// values read straight from the CRDT.
    pub fn take_pending(state: &mut AutoCommit, root: &ObjId) -> SettingsIntents {
        SettingsIntents {
            clear_history: take_flag(state, root, CLEAR_HISTORY_FLAG),
            clear_cookies: take_flag(state, root, CLEAR_COOKIES_FLAG),
            adblock_enabled: read_bool(state, root, ADBLOCK_PATH),
            clear_on_exit: read_bool(state, root, CLEAR_ON_EXIT_PATH),
            search_engine: read_text(state, root, SEARCH_ENGINE_PATH),
            homepage: read_text(state, root, HOMEPAGE_PATH),
            close: take_flag(state, root, CLOSE_FLAG),
        }
    }

    /// Seed the `settings.*` paths from `self.initial`. Idempotent: each
    /// path is only written when absent, so a document already carrying
    /// settings (e.g. restored from collab) is left untouched.
    fn bootstrap_crdt(&self, state: &mut AutoCommit, root: &ObjId) {
        put_bool_if_absent(state, root, ADBLOCK_PATH, self.initial.adblock_enabled);
        put_bool_if_absent(state, root, CLEAR_ON_EXIT_PATH, self.initial.clear_on_exit);
        // Click-flags start false so a false→true edge fires only on a
        // real button press.
        put_bool_if_absent(state, root, CLEAR_HISTORY_FLAG, false);
        put_bool_if_absent(state, root, CLEAR_COOKIES_FLAG, false);
        put_bool_if_absent(state, root, CLOSE_FLAG, false);
        put_text_if_absent(state, root, SEARCH_ENGINE_PATH, &self.initial.search_engine);
        put_text_if_absent(state, root, HOMEPAGE_PATH, &self.initial.homepage);
    }

    /// Build the composed widget tree from the CRDT-backed controls.
    ///
    /// The layout follows the qOS settings grammar: a header, then a stack
    /// of titled "cards", each a group of label/control rows. Vertical
    /// rhythm is done with explicit spacer rows because an intrinsic-height
    /// column lays out as a block stack that ignores taffy's inter-child
    /// gap — a plain `.push().push()` renders the rows touching.
    fn build_composition(&self) -> Box<dyn Widget> {
        let header = Row::new()
            .push(Box::new(Text::new("Settings").with_size(24.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Button::new("settings.close", "Done", Some(CLOSE_FLAG)).with_size(96.0, 36.0),
            ));

        let general = card(
            "General",
            rows(vec![
                field_block(
                    "Homepage",
                    HOMEPAGE_PATH,
                    "settings.homepage_input",
                    DEFAULT_HOMEPAGE,
                    self.actions.homepage.clone(),
                ),
                field_block(
                    "Search engine",
                    SEARCH_ENGINE_PATH,
                    "settings.search_engine_input",
                    DEFAULT_SEARCH_ENGINE,
                    self.actions.search_engine.clone(),
                ),
            ]),
        );

        let privacy = card(
            "Privacy & Security",
            rows(vec![
                setting_row_described(
                    "Block ads & trackers",
                    "Blocks ads, trackers, and YouTube video ads.",
                    Box::new(toggle_with_action(
                        Toggle::new("settings.adblock", ADBLOCK_PATH)
                            .with_id("settings.adblock")
                            .with_accessible_name("Block ads & trackers"),
                        self.actions.adblock.clone(),
                    )),
                ),
                setting_row_described(
                    "Clear data when the browser closes",
                    "Wipe history and cookies on exit.",
                    Box::new(toggle_with_action(
                        Toggle::new("settings.clear_on_exit", CLEAR_ON_EXIT_PATH)
                            .with_id("settings.clear_on_exit")
                            .with_accessible_name("Clear data when the browser closes"),
                        self.actions.clear_on_exit.clone(),
                    )),
                ),
                setting_row_described(
                    "Browsing data",
                    "Clear stored history and site data now.",
                    Box::new(
                        Row::new()
                            .push(Box::new(button_with_action(
                                Button::new(
                                    "settings.clear_history",
                                    "Clear history",
                                    Some(CLEAR_HISTORY_FLAG),
                                )
                                .with_size(140.0, 36.0),
                                self.actions.clear_history.clone(),
                            )))
                            .push(Box::new(button_with_action(
                                Button::new(
                                    "settings.clear_cookies",
                                    "Clear cookies & data",
                                    Some(CLEAR_COOKIES_FLAG),
                                )
                                .with_size(200.0, 36.0),
                                self.actions.clear_site_data.clone(),
                            ))),
                    ),
                ),
            ]),
        );

        let body = Column::new()
            .with_intrinsic_height()
            .push(Box::new(header))
            .push(vgap(20.0))
            .push(general)
            .push(vgap(CARD_GAP))
            .push(privacy);

        // Inset from the window edges (a Container's default padding) and
        // scroll if the window is short.
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(Container::new().with_child(Box::new(body)))),
        )
    }
}

impl Default for SettingsPane {
    fn default() -> Self {
        Self::new()
    }
}

// === composition helpers (qOS settings grammar) ======================
// Bundle the recurring shapes — titled card, label/control row, labelled
// field — so the pane reads consistently. Explicit spacer rows give the
// per-gap control an intrinsic-height column doesn't (its taffy gap is
// ignored in block layout), matching the system settings app.

/// Gap between stacked cards.
const CARD_GAP: f32 = 16.0;
/// Gap between a card's title and its body.
const CARD_TITLE_GAP: f32 = 12.0;
/// Gap between rows inside one card.
const ROW_GAP: f32 = 12.0;

/// The dim foreground colour, read from the OS dark theme (the builders
/// run before `draw`, where the live `Theme` would otherwise supply it).
/// A fixed-height empty box, to inject deliberate vertical space.
fn vgap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_height(px))
}

/// A titled card: a heading above a bordered, padded surface holding the
/// body. Keeping the title inside the background `Container` makes it draw
/// reliably (a bare `Text` in a column can be dropped by the cached draw).
fn card(title: &str, body: El) -> El {
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Row::new().push(Box::new(Text::new(title).with_size(16.0))),
        ))
        .push(vgap(CARD_TITLE_GAP))
        .push(body);
    Box::new(
        Container::new()
            .with_background()
            .with_child(Box::new(inner)),
    )
}

/// A vertical stack of rows with explicit inter-row spacing.
fn rows(items: Vec<El>) -> El {
    let mut col = Column::new().with_no_padding().with_intrinsic_height();
    let last = items.len().saturating_sub(1);
    for (i, item) in items.into_iter().enumerate() {
        col = col.push(item);
        if i != last {
            col = col.push(vgap(ROW_GAP));
        }
    }
    Box::new(col)
}

/// A label with a dim helper line beneath it, paired with a trailing
/// control. The `Spacer` pushes the control to the trailing edge.
fn setting_row_described(label: &str, description: &str, control: El) -> El {
    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(label).with_size(14.0)))
        .push(vgap(2.0))
        .push(Box::new(Text::new(description).with_size(12.0).dimmed()));
    Box::new(
        Row::new()
            .push(Box::new(text_block))
            .push(Box::new(Spacer::new()))
            .push(control),
    )
}

/// A labelled full-width text field: a caption above a [`TextInput`]
/// bound to the CRDT `Text` object at `path`.
fn field_block(
    label: &str,
    path: &str,
    id: &str,
    placeholder: &str,
    action: Option<qos_actions::ActionBinding<String>>,
) -> El {
    let input = TextInput::new(vec![])
        .with_id(id)
        .with_on_change_path(path)
        .with_placeholder(placeholder);
    let input = match action {
        Some(binding) => input.on_action(binding),
        None => input,
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(label).with_size(14.0)))
            .push(vgap(6.0))
            .push(Box::new(input)),
    )
}

fn toggle_with_action(toggle: Toggle, action: Option<qos_actions::ActionBinding<bool>>) -> Toggle {
    match action {
        Some(binding) => toggle.on_action(binding),
        None => toggle,
    }
}

fn button_with_action(button: Button, action: Option<qos_actions::ActionBinding<()>>) -> Button {
    match action {
        Some(binding) => button.on_action(binding),
        None => button,
    }
}

/// Write `value` at `path` only when the path is currently absent.
fn put_bool_if_absent(state: &mut AutoCommit, root: &ObjId, path: &str, value: bool) {
    if state.get(root, path).ok().flatten().is_none() {
        let _ = state.put(root, path, value);
    }
}

/// Seed an Automerge `Text` object at `path` with `value` when absent.
fn put_text_if_absent(state: &mut AutoCommit, root: &ObjId, path: &str, value: &str) {
    if state.get(root, path).ok().flatten().is_none()
        && let Ok(obj_id) = state.put_object(root, path, ObjType::Text)
    {
        let _ = state.splice_text(&obj_id, 0, 0, value);
    }
}

impl Widget for SettingsPane {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, path);
            self.bootstrap_done = true;
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child_node` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child_node) = children.first()
        {
            c.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child_node) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "SettingsPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_id(&self) -> Option<&str> {
        Some("settings.root")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_are_sensible() {
        let s = Settings::default();
        assert!(s.adblock_enabled, "ad-block defaults on");
        assert!(!s.clear_on_exit, "clear-on-exit defaults off");
        assert_eq!(s.search_engine, DEFAULT_SEARCH_ENGINE);
        assert_eq!(s.homepage, DEFAULT_HOMEPAGE);
    }

    #[test]
    fn json_round_trip_is_lossless() {
        let s = Settings {
            adblock_enabled: false,
            clear_on_exit: true,
            search_engine: "https://example.com/search?q=".to_string(),
            homepage: "https://start.example".to_string(),
        };
        // Round-trip through the exact on-disk JSON shape.
        let json = serde_json::to_string(&s.to_json()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let back = Settings::from_json(&parsed);
        assert_eq!(s, back);
    }

    #[test]
    fn save_to_then_load_from_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = Settings {
            adblock_enabled: false,
            clear_on_exit: true,
            search_engine: "https://s.example/?q=".to_string(),
            homepage: "https://home.example".to_string(),
        };
        s.save_to(&path).expect("save_to writes the file");
        let loaded = Settings::load_from(&path);
        assert_eq!(s, loaded, "loaded settings must equal what was saved");
    }

    #[test]
    fn load_from_missing_path_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        // Never created — load must fall back to the default.
        assert_eq!(Settings::load_from(&path), Settings::default());
    }

    #[test]
    fn bootstrap_seeds_all_crdt_paths() {
        let mut pane = SettingsPane::with_settings(Settings {
            adblock_enabled: true,
            clear_on_exit: true,
            search_engine: "https://s.example/?q=".to_string(),
            homepage: "https://home.example".to_string(),
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        // First sync bootstraps the document.
        pane.sync(&mut doc, &root);

        assert_eq!(read_bool(&doc, &root, ADBLOCK_PATH), Some(true));
        assert_eq!(read_bool(&doc, &root, CLEAR_ON_EXIT_PATH), Some(true));
        assert_eq!(
            read_bool(&doc, &root, CLEAR_HISTORY_FLAG),
            Some(false),
            "click-flags seed to false"
        );
        assert_eq!(read_bool(&doc, &root, CLEAR_COOKIES_FLAG), Some(false));

        // Text objects carry the seeded values.
        let (val, obj) = doc.get(&root, SEARCH_ENGINE_PATH).unwrap().unwrap();
        assert!(matches!(val, Value::Object(ObjType::Text)));
        assert_eq!(doc.text(&obj).unwrap(), "https://s.example/?q=");
        let (_, home_obj) = doc.get(&root, HOMEPAGE_PATH).unwrap().unwrap();
        assert_eq!(doc.text(&home_obj).unwrap(), "https://home.example");

        let intents = SettingsPane::take_pending(&mut doc, &root);
        assert_eq!(
            intents.search_engine.as_deref(),
            Some("https://s.example/?q=")
        );
        assert_eq!(intents.homepage.as_deref(), Some("https://home.example"));
    }

    #[test]
    fn clear_history_flag_becomes_intent_and_resets() {
        let mut pane = SettingsPane::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        // Simulate the button's CRDT write (Button raises the flag to true).
        doc.put(&root, CLEAR_HISTORY_FLAG, true).unwrap();

        let first = SettingsPane::take_pending(&mut doc, &root);
        assert!(first.clear_history, "raised flag surfaces as an intent");

        let second = SettingsPane::take_pending(&mut doc, &root);
        assert!(
            !second.clear_history,
            "the flag is reset after being taken (one-shot)"
        );
        assert_eq!(
            read_bool(&doc, &root, CLEAR_HISTORY_FLAG),
            Some(false),
            "take_pending reset the CRDT flag"
        );
    }

    #[test]
    fn clear_cookies_flag_is_independent() {
        let mut pane = SettingsPane::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        doc.put(&root, CLEAR_COOKIES_FLAG, true).unwrap();
        let intents = SettingsPane::take_pending(&mut doc, &root);
        assert!(intents.clear_cookies);
        assert!(
            !intents.clear_history,
            "clearing cookies must not report a history clear"
        );
    }

    #[test]
    fn toggle_states_surface_as_level_intents() {
        let mut pane = SettingsPane::with_settings(Settings {
            adblock_enabled: true,
            clear_on_exit: false,
            ..Settings::default()
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        let intents = SettingsPane::take_pending(&mut doc, &root);
        assert_eq!(intents.adblock_enabled, Some(true));
        assert_eq!(intents.clear_on_exit, Some(false));

        // Flip the ad-block bool as the Toggle would on click.
        doc.put(&root, ADBLOCK_PATH, false).unwrap();
        let after = SettingsPane::take_pending(&mut doc, &root);
        assert_eq!(after.adblock_enabled, Some(false), "toggle change surfaces");
    }

    #[test]
    fn take_pending_before_bootstrap_reports_none() {
        // A fresh document the pane hasn't seeded yet: toggle values are
        // absent, so the level intents are None and no clear fires.
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let intents = SettingsPane::take_pending(&mut doc, &root);
        assert_eq!(intents.adblock_enabled, None);
        assert_eq!(intents.clear_on_exit, None);
        assert!(!intents.clear_history);
        assert!(!intents.clear_cookies);
    }

    #[test]
    fn pane_lays_out_and_toggle_flip_writes_crdt_path() {
        // Drive the real five-pass path: build_layout → compute_layout →
        // apply_layout, then flip the ad-block toggle through the live
        // widget tree and assert the CRDT bool changed.
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

        // Locate the ad-block toggle's rect by walking the live tree.
        fn find_toggle_rect(w: &dyn Widget, id: &str) -> Option<Rect> {
            if w.test_id() == Some(id) {
                return Some(w.get_rect());
            }
            for c in w.children() {
                if let Some(r) = find_toggle_rect(c, id) {
                    return Some(r);
                }
            }
            None
        }

        let mut pane = SettingsPane::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Ad-block seeded on by default.
        assert_eq!(read_bool(&doc, &root, ADBLOCK_PATH), Some(true));

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = pane.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(600.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        pane.apply_layout(&taffy, node, Point::ZERO);
        // Re-sync so the toggle's cached `is_checked` reflects the CRDT.
        pane.sync(&mut doc, &root);

        let rect = find_toggle_rect(&pane, "settings.adblock")
            .expect("ad-block toggle is addressable in the tree");
        let center = rect.center();

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mods = winit::event::Modifiers::default();
        let mut dispatch = |pane: &mut SettingsPane, ev: WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            pane.handle_event(
                &ev,
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };
        dispatch(
            &mut pane,
            WindowEvent::CursorMoved {
                device_id: device,
                position: (center.x, center.y).into(),
            },
        );
        dispatch(
            &mut pane,
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );

        // The toggle flipped the ad-block bool off through the real path.
        assert_eq!(
            read_bool(&doc, &root, ADBLOCK_PATH),
            Some(false),
            "clicking the ad-block toggle writes the CRDT path"
        );
        let intents = SettingsPane::take_pending(&mut doc, &root);
        assert_eq!(intents.adblock_enabled, Some(false));
    }
}
