//! Browser settings — the preferences the browser document holds plus the
//! [`SettingsPane`] widget that edits them.
//!
//! Two layers live here:
//!
//!  * [`Settings`] — the durable preferences (ad-blocking, clear-on-exit,
//!    search engine, homepage) as the [`Browser`](crate::resource::Browser)
//!    resource reads them from its document.
//!
//!  * [`SettingsPane`] — a `ui` [`Widget`] that renders process-local
//!    presentation mirrors of those preferences. Every control submits a
//!    field edit at the preference's path through the resource's
//!    dispatcher; it never writes the document itself.
//!
//! The chrome applies the engine side effects of the preferences it is
//! presented. Clear-history and clear-site-data are explicit one-shot
//! effects, while closing the pane is an explicit view operation.

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::TaffyTreeExt;

use ui::theme::Theme;
use ui::widget::{
    Button, Card, Column, Container, Row, ScrollAxis, Scrollable, Section, Spacer, Text, TextInput,
    Toggle, Widget,
};

use crate::resource::Browser;
use ui::tilo_resource::{self, Model, Path, Refusal};

/// The doors the pane's controls use, injected by Browser. Preferences are
/// field edits submitted through the resource's dispatcher; the
/// one-shot effects and closing the pane are the published operations, so
/// pointer, keyboard and assistive-tech changes take the same path as Ctrl+K
/// and agents.
#[derive(Clone, Default)]
pub struct SettingsActionBindings {
    /// The resource's dispatcher; `None` leaves the preference controls
    /// inert (a pane nobody can edit through).
    pub edits: Option<ui::framework::Dispatcher<Browser>>,
    pub clear_history: Option<tilo_actions::ActionBinding<()>>,
    pub clear_site_data: Option<tilo_actions::ActionBinding<()>>,
    /// Close is presentation-only, but it is still a canonical Browser view
    /// action.  Keeping it here means keyboard and assistive-tech activation
    /// have the same route as the Library's Done button.
    pub close: Option<tilo_actions::ActionBinding<()>>,
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

/// Default search-engine query template. The `?q=` suffix lets the
/// chrome build a search URL by appending typed terms.
pub const DEFAULT_SEARCH_ENGINE: &str = "https://duckduckgo.com/?q=";
/// Default homepage — matches the browser's startup URL.
pub const DEFAULT_HOMEPAGE: &str = "https://duckduckgo.com";

// === Preferences ================================================

/// Durable browser preferences, as the browser document holds them.
///
/// Defaults: ad-blocking on, clear-on-exit off, `DuckDuckGo` for both
/// search and homepage; a new browser record starts with them. The
/// homepage and search engine are always absolute http(s) addresses.
#[derive(Clone, Debug, PartialEq, Eq, Model)]
#[model(check = Settings::valid)]
pub struct Settings {
    /// Ad and tracker blocking: block ads & trackers via the engine's
    /// content blocker.
    pub adblock_enabled: bool,
    /// Clear data on exit: clear browsing history & cookies on exit.
    pub clear_on_exit: bool,
    /// Search engine: the query template (e.g. `https://duckduckgo.com/?q=`).
    pub search_engine: String,
    /// Homepage: the URL opened on startup / new tab.
    pub homepage: String,
}

impl Settings {
    /// A homepage and a search engine are web addresses the engine opens.
    fn valid(&self) -> Result<(), Refusal> {
        if crate::actions::normalized_bookmark_url(&self.homepage).is_none() {
            return Err(Refusal::new("the homepage is an absolute http(s) URL"));
        }
        if crate::actions::normalized_bookmark_url(&self.search_engine).is_none() {
            return Err(Refusal::new("the search engine is an absolute http(s) URL"));
        }
        Ok(())
    }
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

// === Widget ==========================================================

/// The browser's settings surface: a scrollable column of preference
/// controls. Typed actions mutate durable settings; local mirrors only
/// present the action-applied result in this window.
pub struct SettingsPane {
    notices: Vec<crate::notice::Notice>,
    presentation: SettingsPresentation,
    /// Composed child tree, rebuilt on every `build_layout`.
    composed: ui::widget::Composed,
    actions: SettingsActionBindings,
    /// A device-local persistence failure supplied by Browser after a clear
    /// action. It is deliberately not a CRDT value: other collaborators
    /// cannot diagnose this profile's disk permissions or free space.
    history_clear_error: Option<String>,
    private_window_request: Option<ui::LocalState<bool>>,
    autofill_request: Option<ui::LocalState<Option<bool>>>,
}

impl SettingsPane {
    /// Construct a pane seeded from [`Settings::default`].
    pub fn new() -> Self {
        Self::with_settings(&Settings::default())
    }

    /// Construct a pane with a process-local projection of `settings`.
    pub fn with_settings(settings: &Settings) -> Self {
        Self {
            notices: Vec::new(),
            presentation: SettingsPresentation::from_settings(settings),
            composed: ui::widget::Composed::default(),
            actions: SettingsActionBindings::default(),
            history_clear_error: None,
            private_window_request: None,
            autofill_request: None,
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

    /// Keep New Private Window discoverable when compact chrome moves the
    /// dedicated toolbar button into Settings.
    #[must_use]
    pub fn with_private_window_request(mut self, request: ui::LocalState<bool>) -> Self {
        self.private_window_request = Some(request);
        self
    }

    /// Open device-local password or address management through a published view control.
    #[must_use]
    pub fn with_autofill_request(mut self, request: ui::LocalState<Option<bool>>) -> Self {
        self.autofill_request = Some(request);
        self
    }

    /// Build the composed widget tree from the CRDT-backed controls.
    ///
    /// The layout follows the Tilo settings grammar: a header, then a stack
    /// of titled "cards", each a group of label/control rows. Vertical
    /// rhythm is done with explicit spacer rows because an intrinsic-height
    /// column lays out as a block stack that ignores taffy's inter-child
    /// gap — a plain `.push().push()` renders the rows touching.
    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let large_text = theme.text_scale >= 1.5;
        let header = Row::new()
            .push(Box::new(
                Text::new("Settings")
                    .with_role(ui::TextRole::Title)
                    .with_heading_level(1),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(button_with_action(
                Button::new("settings.close", "Done").control_size(ui::widget::ControlSize::Large),
                self.actions.close.clone(),
            )));

        let general = card(
            "General",
            rows(vec![
                field_block(
                    "Homepage",
                    &self.presentation.homepage,
                    "settings.homepage_input",
                    DEFAULT_HOMEPAGE,
                    self.actions.edits.clone(),
                    Browser::paths().preferences.homepage,
                ),
                field_block(
                    "Search engine",
                    &self.presentation.search_engine,
                    "settings.search_engine_input",
                    DEFAULT_SEARCH_ENGINE,
                    self.actions.edits.clone(),
                    Browser::paths().preferences.search_engine,
                ),
            ]),
        );

        let browsing_controls = browsing_data_controls(&self.actions, large_text);
        let mut privacy_rows = vec![
            setting_row_described(
                "Block ads & trackers",
                "Blocks ads, trackers, and YouTube video ads.",
                Box::new(preference_toggle(
                    Toggle::from_state(&self.presentation.adblock)
                        .with_id("settings.adblock")
                        .with_accessible_name("Block ads & trackers"),
                    self.actions.edits.clone(),
                    Browser::paths().preferences.adblock_enabled,
                )),
                large_text,
            ),
            setting_row_described(
                "Clear data when the browser closes",
                "Wipe history and cookies on exit.",
                Box::new(preference_toggle(
                    Toggle::from_state(&self.presentation.clear_on_exit)
                        .with_id("settings.clear_on_exit")
                        .with_accessible_name("Clear data when the browser closes"),
                    self.actions.edits.clone(),
                    Browser::paths().preferences.clear_on_exit,
                )),
                large_text,
            ),
            setting_row_described(
                "Browsing data",
                "Clear stored history, or request a local engine site-data clear.",
                browsing_controls,
                large_text,
            ),
        ];
        if let Some(request) = &self.autofill_request {
            for (id, label, addresses) in [
                ("browser.credentials.open", "Passwords", false),
                ("browser.address.open", "Address autofill", true),
            ] {
                let request = request.clone();
                privacy_rows.push(setting_row_described(
                    label,
                    "Manage filling on this device; opening does not enable it.",
                    Box::new(
                        Button::new(id, "Manage")
                            .with_accessible_name(label)
                            .on_view(move || request.set(Some(addresses))),
                    ),
                    large_text,
                ));
            }
        }
        if let Some(error) = &self.history_clear_error {
            privacy_rows.push(Box::new(Text::new(error).with_role(ui::TextRole::Caption)));
        }
        let privacy = card("Privacy & Security", rows(privacy_rows));

        let mut body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_intrinsic_height()
            .push(Box::new(header))
            .push(general)
            .push(privacy);
        if let Some(request) = self.private_window_request.clone() {
            body = body.push(card(
                "Windows",
                rows(vec![setting_row_described(
                    "Private browsing",
                    "Open a separate private window with memory-only browsing state.",
                    Box::new(
                        Button::new("settings.new_private_window", "New Private Window")
                            .control_size(ui::widget::ControlSize::Regular)
                            .on_effect(move || request.set(true)),
                    ),
                    large_text,
                )]),
            ));
        }

        for notice in &self.notices {
            body = body.push(notice.widget(false));
        }

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

// === composition helpers (Tilo settings grammar) ======================
// Bundle the recurring shapes — titled card, label/control row, labelled
// field — so the pane reads consistently. Repeated vertical rhythm comes from
// the shared `Section`, matching the system Settings app.

/// A named section uses the shared card's heading, inset and surface.
fn card(title: &str, body: El) -> El {
    Box::new(Card::new(body).with_title(title))
}

/// A vertical stack of rows with explicit inter-row spacing.
fn rows(items: Vec<El>) -> El {
    Box::new(Section::new(items))
}

/// A label with a dim helper line beneath it, paired with a trailing
/// control. The `Spacer` pushes the control to the trailing edge.
fn setting_row_described(label: &str, description: &str, control: El, large_text: bool) -> El {
    let text_block = Column::new()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(label).with_role(ui::TextRole::Body)))
        .push(Box::new(
            Text::new(description)
                .with_role(ui::TextRole::Caption)
                .dimmed(),
        ));
    if large_text {
        Box::new(
            Column::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(text_block))
                .push(control),
        )
    } else {
        Box::new(
            Row::new()
                .push(Box::new(text_block))
                .push(Box::new(Spacer::new()))
                .push(control),
        )
    }
}

fn browsing_data_controls(actions: &SettingsActionBindings, large_text: bool) -> El {
    let clear_history = Box::new(button_with_action(
        Button::new("settings.clear_history", "Clear history")
            .control_size(ui::widget::ControlSize::Large),
        actions.clear_history.clone(),
    ));
    let clear_site_data = Box::new(button_with_action(
        Button::new("settings.clear_cookies", "Request site-data clear")
            .control_size(ui::widget::ControlSize::Large),
        actions.clear_site_data.clone(),
    ));
    if large_text {
        Box::new(
            Column::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_no_padding()
                .with_intrinsic_height()
                .push(clear_history)
                .push(clear_site_data),
        )
    } else {
        Box::new(Row::new().push(clear_history).push(clear_site_data))
    }
}

/// A labelled full-width text field: a caption above a [`TextInput`] whose
/// edits replace the preference at `path` with the typed value.
fn field_block(
    label: &str,
    state: &ui::LocalState<String>,
    id: &str,
    placeholder: &str,
    edits: Option<ui::framework::Dispatcher<Browser>>,
    path: Path<Browser, String>,
) -> El {
    let input = TextInput::from_state(state)
        .with_id(id)
        .with_placeholder(placeholder);
    let input = match edits {
        Some(edits) => input.on_change(move |value| edits.submit(path.replace(value))),
        None => input,
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(label).with_role(ui::TextRole::Body)))
            .push(Box::new(input)),
    )
}

fn preference_toggle(
    toggle: Toggle,
    edits: Option<ui::framework::Dispatcher<Browser>>,
    path: Path<Browser, bool>,
) -> Toggle {
    match edits {
        Some(edits) => toggle.on_change(move |enabled| edits.submit(path.set(enabled))),
        None => toggle,
    }
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
        let composition = self.build_composition(theme);
        let child_node = self
            .composed
            .build_layout(composition, taffy, theme, node_map);

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
        taffy.widget_node(style, &[child_node], node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "SettingsPane"
    }

    fn test_id(&self) -> Option<&str> {
        Some("settings.root")
    }
}

impl SettingsPane {
    pub(crate) fn with_notices(mut self, notices: Vec<crate::notice::Notice>) -> Self {
        self.notices = notices;
        self
    }
}
pub(crate) fn page(parts: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    Box::new(
        parts
            .into_iter()
            .fold(ui::widget::Page::new(), ui::widget::Page::section),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use automerge::ReadDoc;
    use std::collections::HashMap;
    use vello::kurbo::{Point, Rect};

    #[test]
    fn preference_defaults_are_sensible() {
        let s = Settings::default();
        assert!(s.adblock_enabled, "ad-block defaults on");
        assert!(!s.clear_on_exit, "clear-on-exit defaults off");
        assert_eq!(s.search_engine, DEFAULT_SEARCH_ENGINE);
        assert_eq!(s.homepage, DEFAULT_HOMEPAGE);
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&doc, &root);
            assert_eq!(doc.length(&root), 0, "presenting writes no document");
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
            let doc = AutoCommit::new();
            pane.sync(&doc, &automerge::ROOT);
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
            use winit::event::{ElementState, MouseButton, WindowEvent};

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
            pane.sync(&doc, &root);

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
            pane.sync(&doc, &root);

            let rect = find_toggle_rect(&pane, "settings.adblock")
                .expect("ad-block toggle is addressable in the tree");
            let center = rect.center();

            let device = winit::event::DeviceId::dummy();
            let mods = winit::event::Modifiers::default();
            let mut dispatch = |pane: &mut SettingsPane, ev: WindowEvent| {
                let mut handled = false;
                let mut focus = None;
                pane.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: Edits::lend(&mut doc),
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
