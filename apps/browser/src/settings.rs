//! Browser settings — a persistent preferences model plus the
//! [`SettingsPane`] widget that edits it.
//!
//! Two layers live here:
//!
//!  * [`Settings`] — a plain serde struct persisted as JSON under
//!    `tilo_paths::config_dir()/tilo-browser/settings.json`. It holds the
//!    durable preferences (ad-blocking, clear-on-exit, search engine,
//!    homepage) and knows how to [`Settings::load`] / [`Settings::save`]
//!    itself.
//!
//!  * [`SettingsPane`] — a `ui` [`Widget`] that renders process-local
//!    presentation mirrors of those preferences. Every control invokes a
//!    canonical Browser action; it never writes click flags or preference
//!    values into the collaborative document.
//!
//! The chrome owns the action bindings and engine side effects. Applied
//! preference actions update [`Settings`], refresh the pane's local mirrors,
//! and persist the device configuration. Clear-history and clear-site-data
//! are explicit one-shot actions, while closing the pane is an explicit view
//! operation. There is no per-frame intent polling channel.

use std::path::{Path, PathBuf};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, Button, ButtonRole, Column, Container, Field, Form, FormSection, ListRow, Row,
    ScrollAxis, Scrollable, Section, Spacer, Text, TextInput, Toggle, Widget,
};

use crate::actions::BrowserAction;

/// Resource bindings injected by Browser when this pane is hosted. They keep
/// pointer, keyboard and assistive-tech changes on the exact same action path
/// used by Ctrl+K and agents.
#[derive(Clone, Default)]
pub struct SettingsActionBindings {
    pub adblock: Option<tilo_actions::ActionBinding<bool>>,
    pub clear_on_exit: Option<tilo_actions::ActionBinding<bool>>,
    pub homepage: Option<tilo_actions::ActionBinding<String>>,
    pub search_engine: Option<tilo_actions::ActionBinding<String>>,
    pub clear_history: Option<tilo_actions::ActionBinding<()>>,
    pub clear_site_data: Option<tilo_actions::ActionBinding<()>>,
    /// The autofill panels and a new private window, reached from rows of the
    /// settings page.
    pub open_passwords: Option<tilo_actions::ActionBinding<()>>,
    pub open_addresses: Option<tilo_actions::ActionBinding<()>>,
    pub new_private_window: Option<tilo_actions::ActionBinding<()>>,
    /// Close is presentation-only, but it is still a canonical Browser view
    /// action.  Keeping it here means keyboard and assistive-tech activation
    /// have the same route as the Library's Done button.
    pub close: Option<tilo_actions::ActionBinding<()>>,
    /// Hosted bookmark and preference edits. Used when the matching binding
    /// is absent, so the surface submits a [`BrowserAction`] instead of
    /// writing the document itself.
    pub document_edits: Option<ui::LocalState<Vec<BrowserAction>>>,
}

/// Process-local mirrors used only to present the action-applied settings.
/// They are not a persistence or collaboration channel.
#[derive(Clone)]
pub struct SettingsPresentation {
    pub adblock: ui::LocalState<bool>,
    pub clear_on_exit: ui::LocalState<bool>,
    pub homepage: ui::LocalState<String>,
    pub search_engine: ui::LocalState<String>,
}

impl SettingsPresentation {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            adblock: ui::LocalState::new(settings.adblock_enabled),
            clear_on_exit: ui::LocalState::new(settings.clear_on_exit),
            homepage: ui::LocalState::new(settings.homepage.clone()),
            search_engine: ui::LocalState::new(settings.search_engine.clone()),
        }
    }

    pub fn reflect(&self, settings: &Settings) {
        if self.adblock.with(|value| *value) != settings.adblock_enabled {
            self.adblock.set(settings.adblock_enabled);
        }
        if self.clear_on_exit.with(|value| *value) != settings.clear_on_exit {
            self.clear_on_exit.set(settings.clear_on_exit);
        }
        if self.homepage.with(Clone::clone) != settings.homepage {
            self.homepage.set(settings.homepage.clone());
        }
        if self.search_engine.with(Clone::clone) != settings.search_engine {
            self.search_engine.set(settings.search_engine.clone());
        }
    }
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
/// Automerge `Text` object holding the search-engine query template.
pub const SEARCH_ENGINE_PATH: &str = "settings.search_engine";
/// Automerge `Text` object holding the homepage URL.
pub const HOMEPAGE_PATH: &str = "settings.homepage";

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
/// Persistence goes through `serde_json::Value` (see `Settings::to_json`
/// / `Settings::from_json`) rather than a `#[derive(Serialize)]`: the
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
    /// `tilo_paths::config_dir()/tilo-browser/settings.json`. `None` when the
    /// platform has no config directory.
    pub fn config_path() -> Option<PathBuf> {
        crate::storage::config_dir().map(|d| d.join("settings.json"))
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

// === Widget ==========================================================

/// The browser's settings surface: a scrollable column of preference
/// controls. Typed actions mutate durable settings; local mirrors only
/// present the action-applied result in this window.
pub struct SettingsPane {
    rect: Rect,
    presentation: SettingsPresentation,
    /// Composed child tree, rebuilt on every `build_layout`.
    composed: Option<Box<dyn Widget>>,
    actions: SettingsActionBindings,
    /// A device-local persistence failure supplied by Browser after a clear
    /// action. It is deliberately not a CRDT value: other collaborators
    /// cannot diagnose this profile's disk permissions or free space.
    history_clear_error: Option<String>,
}

impl SettingsPane {
    /// Construct a pane seeded from [`Settings::default`].
    pub fn new() -> Self {
        Self::with_settings(&Settings::default())
    }

    /// Construct a pane with a process-local projection of `settings`.
    pub fn with_settings(settings: &Settings) -> Self {
        Self {
            rect: Rect::ZERO,
            presentation: SettingsPresentation::from_settings(settings),
            composed: None,
            actions: SettingsActionBindings::default(),
            history_clear_error: None,
        }
    }

    /// Route hosted controls through Browser's canonical resource actions.
    #[must_use]
    pub fn with_action_bindings(mut self, actions: SettingsActionBindings) -> Self {
        self.actions = actions;
        self
    }

    /// Reuse Browser's retained presentation values across composition
    /// rebuilds. This avoids turning field drafts or focus into CRDT state.
    #[must_use]
    pub fn with_presentation(mut self, presentation: SettingsPresentation) -> Self {
        self.presentation = presentation;
        self
    }

    /// Surface an honest local failure next to the control that initiated it.
    #[must_use]
    pub fn with_history_clear_error(mut self, error: Option<String>) -> Self {
        self.history_clear_error = error;
        self
    }

    /// Build the composed widget tree from the CRDT-backed controls.
    ///
    /// A page title over a form: sections on the ground, separated by space,
    /// never boxed. The form owns label placement, rows and separators.
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let header = Row::new()
            .no_wrap()
            .push(Box::new(
                Text::new("Settings")
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(button_with_action(
                Button::new("settings.close", "Done", None),
                self.actions.close.clone(),
            )));

        let general = FormSection::new("General")
            .field(field_block(
                "Homepage",
                &self.presentation.homepage,
                "settings.homepage_input",
                DEFAULT_HOMEPAGE,
                self.actions.homepage.clone(),
                hosted_text(self.actions.document_edits.clone(), homepage_action),
            ))
            .field(field_block(
                "Search engine",
                &self.presentation.search_engine,
                "settings.search_engine_input",
                DEFAULT_SEARCH_ENGINE,
                self.actions.search_engine.clone(),
                hosted_text(self.actions.document_edits.clone(), search_engine_action),
            ));

        let mut privacy = FormSection::new("Privacy and security")
            .grouped()
            .field(Field::new(
                "Block ads and trackers",
                preference_toggle(
                    Toggle::from_state(&self.presentation.adblock).with_id("settings.adblock"),
                    self.actions.adblock.clone(),
                    self.actions.document_edits.as_ref(),
                    adblock_action,
                ),
            ))
            .field(Field::new(
                "Clear history and cookies when Browser closes",
                preference_toggle(
                    Toggle::from_state(&self.presentation.clear_on_exit)
                        .with_id("settings.clear_on_exit"),
                    self.actions.clear_on_exit.clone(),
                    self.actions.document_edits.as_ref(),
                    clear_on_exit_action,
                ),
            ))
            .content(browsing_data_controls(
                &self.actions,
                theme.tokens.spacing.s3,
            ));
        if let Some(error) = &self.history_clear_error {
            privacy = privacy.content(Box::new(Banner::error(error)));
        }

        let row = |id: &str,
                   title: &str,
                   secondary: &str,
                   binding: &Option<tilo_actions::ActionBinding<()>>| {
            let row = ListRow::new(id, title)
                .icon(Icon::Shield)
                .secondary(secondary)
                .disclosure();
            Box::new(match binding {
                Some(binding) => row.on_action(binding.clone()),
                None => row,
            }) as El
        };
        let form = Form::new()
            .section(general)
            .section(privacy)
            .section(
                FormSection::new("Autofill").content(Box::new(Section::new(vec![
                    row(
                        "browser.credentials.open",
                        "Passwords",
                        "Saved logins for the page you are on",
                        &self.actions.open_passwords,
                    ),
                    row(
                        "browser.address.open",
                        "Addresses",
                        "Profiles stay on this device",
                        &self.actions.open_addresses,
                    ),
                ]))),
            )
            .section(
                FormSection::new("Windows").content(Box::new(Section::new(vec![row(
                    "settings.new_private_window",
                    "New private window",
                    "Browsing data is removed when the window closes",
                    &self.actions.new_private_window,
                )]))),
            );

        // A page: a title over the form, bounded to a readable width,
        // inset from the window edges and scrolled when the window is short.
        let page = Column::new()
            .with_intrinsic_height()
            .push(Box::new(header))
            .push(Box::new(Spacer::fixed(theme.tokens.spacing.rhythm.group)))
            .push(Box::new(form));
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_centered_max_width(
                            theme.tokens.metrics.panel_width_sheet
                                + theme.tokens.metrics.panel_width_utility,
                        )
                        .with_child(Box::new(page)),
                )),
        )
    }
}

impl Default for SettingsPane {
    fn default() -> Self {
        Self::new()
    }
}

// === composition helpers =============================================

/// The two browsing-data commands, destructive and explicit.
fn browsing_data_controls(actions: &SettingsActionBindings, gap: f32) -> El {
    Box::new(
        Container::new().with_no_padding().with_child(Box::new(
            Row::new()
                .with_padding(0.0, gap)
                .push(Box::new(
                    button_with_action(
                        Button::new("settings.clear_history", "Clear history", None),
                        actions.clear_history.clone(),
                    )
                    .role(ButtonRole::Destructive),
                ))
                .push(Box::new(
                    button_with_action(
                        Button::new("settings.clear_cookies", "Clear site data", None),
                        actions.clear_site_data.clone(),
                    )
                    .role(ButtonRole::Destructive),
                )),
        )),
    )
}

/// A labelled text field bound to the CRDT `Text` object at `path`.
#[allow(
    clippy::type_complexity,
    reason = "hosted LocalState+builder pair is local to settings chrome"
)]
fn field_block(
    label: &str,
    state: &ui::LocalState<String>,
    id: &str,
    placeholder: &str,
    action: Option<tilo_actions::ActionBinding<String>>,
    hosted: Option<(
        ui::LocalState<Vec<BrowserAction>>,
        fn(String) -> BrowserAction,
    )>,
) -> Field {
    let input = TextInput::from_state(state)
        .with_id(id)
        .with_kind(ui::widget::TextInputKind::Url)
        .with_placeholder(placeholder);
    let input = if let Some(binding) = action {
        input.on_action(binding)
    } else if let Some((edits, build)) = hosted {
        input.on_change(move |value| {
            let next = build(value.to_string());
            edits.update(|queue| queue.push(next));
        })
    } else {
        input
    };
    Field::new(label, input)
}

fn preference_toggle(
    toggle: Toggle,
    action: Option<tilo_actions::ActionBinding<bool>>,
    edits: Option<&ui::LocalState<Vec<BrowserAction>>>,
    build: fn(bool) -> BrowserAction,
) -> Toggle {
    if let Some(binding) = action {
        return toggle.on_action(binding);
    }
    let Some(edits) = edits.cloned() else {
        return toggle;
    };
    toggle.on_change(move |enabled| {
        let next = build(enabled);
        edits.update(|queue| queue.push(next));
    })
}

#[allow(
    clippy::type_complexity,
    reason = "hosted LocalState+builder pair is local to settings chrome"
)]
fn hosted_text(
    edits: Option<ui::LocalState<Vec<BrowserAction>>>,
    build: fn(String) -> BrowserAction,
) -> Option<(
    ui::LocalState<Vec<BrowserAction>>,
    fn(String) -> BrowserAction,
)> {
    edits.map(|queue| (queue, build))
}

fn adblock_action(enabled: bool) -> BrowserAction {
    BrowserAction::SetAdblock { enabled }
}

fn clear_on_exit_action(enabled: bool) -> BrowserAction {
    BrowserAction::SetClearOnExit { enabled }
}

fn homepage_action(url: String) -> BrowserAction {
    BrowserAction::SetHomepage { url }
}

fn search_engine_action(template: String) -> BrowserAction {
    BrowserAction::SetSearchEngine { template }
}

fn button_with_action(button: Button, action: Option<tilo_actions::ActionBinding<()>>) -> Button {
    match action {
        Some(binding) => button.on_action(binding),
        None => button,
    }
}

impl Widget for SettingsPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme);
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
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child_node) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
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
    use automerge::AutoCommit;
    use automerge::ReadDoc;
    use std::collections::HashMap;

    /// Defaults, JSON and disk persistence.
    #[test]
    fn settings_defaults_and_persistence_round_trip() {
        // Phase `defaults_are_sensible`.
        {
            let s = Settings::default();
            assert!(s.adblock_enabled, "ad-block defaults on");
            assert!(!s.clear_on_exit, "clear-on-exit defaults off");
            assert_eq!(s.search_engine, DEFAULT_SEARCH_ENGINE);
            assert_eq!(s.homepage, DEFAULT_HOMEPAGE);
        }

        // Phase `json_round_trip_is_lossless`.
        {
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

        // Phase `save_to_then_load_from_round_trips`.
        {
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

        // Phase `load_from_missing_path_is_default`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("does-not-exist.json");
            // Never created — load must fall back to the default.
            assert_eq!(Settings::load_from(&path), Settings::default());
        }
    }

    /// Presentation values, destructive controls and pane layout.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn settings_presentation_is_local_per_instance_and_action_bound() {
        // Phase `presentation_seeds_local_values_without_document_writes`.
        {
            let settings = Settings {
                adblock_enabled: true,
                clear_on_exit: true,
                search_engine: "https://s.example/?q=".to_string(),
                homepage: "https://home.example".to_string(),
            };
            let mut pane = SettingsPane::with_settings(&settings);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(doc.get(&root, ADBLOCK_PATH).unwrap().is_none());
            assert!(doc.get(&root, HOMEPAGE_PATH).unwrap().is_none());
            let presentation = SettingsPresentation::from_settings(&settings);
            assert_eq!(presentation.homepage.with(Clone::clone), settings.homepage);
            assert_eq!(
                presentation.search_engine.with(Clone::clone),
                settings.search_engine
            );
        }

        // Phase `presentation_reflects_action_applied_values`.
        {
            let presentation = SettingsPresentation::from_settings(&Settings::default());
            let settings = Settings {
                adblock_enabled: false,
                ..Settings::default()
            };
            presentation.reflect(&settings);
            assert!(!presentation.adblock.with(|value| *value));
        }

        // Phase `destructive_controls_have_no_document_fallback_paths`.
        {
            let mut pane = SettingsPane::new();
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(
                ui::widget::controls::control_action_bindings(&pane)
                    .iter()
                    .all(|binding| !matches!(
                        binding.kind,
                        ui::widget::ControlBindingKind::CollaborativeDocument(_)
                    ))
            );
        }

        // Phase `presentation_is_per_instance`.
        {
            let first = SettingsPresentation::from_settings(&Settings::default());
            let second = SettingsPresentation::from_settings(&Settings::default());
            first.adblock.set(false);
            assert!(!first.adblock.with(|value| *value));
            assert!(second.adblock.with(|value| *value));
        }

        // Phase `pane_lays_out_and_toggle_has_a_canonical_action_binding`.
        {
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
            // Re-sync so the toggle adopts its retained presentation state.
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
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
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

            assert!(
                ui::widget::controls::control_action_bindings(&pane)
                    .iter()
                    .all(|binding| !matches!(
                        binding.kind,
                        ui::widget::ControlBindingKind::CollaborativeDocument(_)
                    ))
            );
        }
    }
}
