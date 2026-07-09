//! `SettingsApp` — the top-level settings hub.
//!
//! ## Layout
//!
//! On a wide window the app is a two-pane split: a categorized sidebar of
//! [`Category`] entries on the left, and the selected category's content
//! pane on the right. On a narrow window (below [`NARROW_BREAKPOINT_PX`])
//! it collapses to a single pane — the sidebar shows alone until the user
//! taps a category, which "pushes" the panel in over the full width with a
//! back affordance to return to the list. This mirrors the Adwaita
//! leaflet/overlay-flap behaviour.
//!
//! ## How responsiveness works
//!
//! The two panes are wrapped in a single [`NavSplit`] from the shared
//! toolkit. `NavSplit` owns the responsive behaviour entirely: when its
//! allocated width is at or above [`NAV_BREAKPOINT`] the sidebar is docked
//! beside the content; below it the sidebar becomes an overlay flap that
//! slides in over a dimming scrim (dismissed by the scrim, Esc, or a
//! toggle). Settings supplies the two child widgets and reacts to category
//! selection; it does not re-implement the flap, the breakpoint, or the
//! scrim.
//!
//! ## State and the rebuild that drives navigation
//!
//! The selected category lives on the widget (and is mirrored into the
//! document via the per-row select key). When [`SettingsApp::sync`] sees a
//! category's select key set, it updates `selected` **and flags
//! `structure_dirty`**. The host's frame loop polls `take_structure_dirty`
//! and, when set, re-runs `build_layout` — which is where the `NavSplit`
//! (and therefore the content pane for the new category) is rebuilt. That
//! rebuild-on-selection is exactly what makes clicking a category switch the
//! visible pane: without the dirty flag, `build_layout` would never re-run
//! after the initial frame and the content would stay frozen on the first
//! category.

use crate::categories::Category;
use crate::panes;
use crate::sidebar;
use ui::widget::{Button, Column, Container, NavSplit, Row, Spacer, SplitLayout, SplitMode, Text, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// CRDT key the header's collapse/expand button writes `true` to. Read in
/// [`SettingsApp::sync`], it flips [`SettingsApp::sidebar_collapsed`].
pub const TOGGLE_SIDEBAR_KEY: &str = "settings.toggle_sidebar";

/// At or above this allocated width (logical px) the sidebar docks beside the
/// content; below it the sidebar becomes an overlay flap. Sized so a typical
/// settings window is docked while a narrow / split-screen placement gets the
/// overlay.
pub const NAV_BREAKPOINT: f64 = 720.0;

pub struct SettingsApp {
    /// Currently-selected sidebar category.
    selected: Category,
    /// `true` once a selection (or other composition-affecting change) has
    /// happened and the host must re-run `build_layout` to rebuild the
    /// `NavSplit` with the new content pane. Drained by `take_structure_dirty`.
    structure_dirty: bool,
    /// In `--shots` / headless rendering the live `ui::App` event loop that
    /// would normally drive the flap open is absent, so we force the sidebar
    /// open after building. Set via [`SettingsApp::with_sidebar_open_shot`].
    force_sidebar_open: bool,
    /// Seed width hint so the very first offscreen frame resolves the correct
    /// docked/overlay layout. `None` lets `NavSplit` decide from its own rect.
    width_hint: Option<f32>,
    /// Whether the sidebar is explicitly collapsed (hidden even when the
    /// window is wide enough to dock it), reclaiming the space for content.
    /// Toggled by the header's collapse button; mirrored into the document
    /// via [`TOGGLE_SIDEBAR_KEY`] so it survives structure rebuilds.
    sidebar_collapsed: bool,
    /// The live search query typed into the sidebar's search field (mirrored
    /// from [`crate::sidebar::SEARCH_KEY`]). When non-empty the sidebar shows
    /// a filtered results list instead of the sectioned category list; a
    /// change flags `structure_dirty` so the filtered list is rebuilt.
    search_query: String,
    rect: vello::kurbo::Rect,
    /// The composed split (sidebar | content), rebuilt each `build_layout` so
    /// a category switch shows up once the host re-runs the layout pass.
    split: Option<NavSplit>,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            selected: Category::DEFAULT,
            structure_dirty: false,
            force_sidebar_open: false,
            width_hint: None,
            sidebar_collapsed: false,
            search_query: String::new(),
            rect: vello::kurbo::Rect::ZERO,
            split: None,
        }
    }

    /// Construct with a specific category pre-selected. Used by tests and
    /// future deep-linking (`settings://network`).
    pub fn with_selected(category: Category) -> Self {
        let mut s = Self::new();
        s.selected = category;
        s
    }

    /// Seed the responsive layout with a known width before the first frame —
    /// the offscreen `--shots` renderer calls this so the cold screenshot
    /// resolves the correct docked/overlay layout immediately.
    pub fn with_width_hint(mut self, width: f32) -> Self {
        self.width_hint = Some(width);
        self
    }

    /// Force the overlay sidebar open for a `--shots` still. Without the live
    /// event loop nothing toggles the flap, so the narrow screenshot would
    /// otherwise show a closed sidebar. Has no visible effect when docked.
    pub fn with_sidebar_open_shot(mut self) -> Self {
        self.force_sidebar_open = true;
        self
    }

    pub fn selected(&self) -> Category {
        self.selected
    }

    /// Switch the active category. Flags `structure_dirty` so the host's next
    /// frame re-runs `build_layout` and rebuilds the content pane.
    pub fn select(&mut self, category: Category) {
        if self.selected != category {
            self.selected = category;
            self.structure_dirty = true;
        }
    }

    /// Whether the current frame resolves to the narrow (overlay) layout.
    /// Reads the live `NavSplit` once it exists, else the seeded width hint.
    pub fn is_narrow(&self) -> bool {
        match &self.split {
            Some(s) => s.layout_mode() == SplitLayout::Overlay,
            None => self
                .width_hint
                .map(|w| (w as f64) < NAV_BREAKPOINT)
                .unwrap_or(false),
        }
    }

    /// Whether the overlay sidebar is currently open (or docked, which always
    /// reads as "open"). Used by `--shots` assertions.
    pub fn sidebar_open(&self) -> bool {
        self.split.as_ref().is_some_and(|s| s.is_sidebar_open())
    }

    /// Whether the sidebar is explicitly collapsed (hidden even when docked).
    pub fn sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    /// The current search query (empty when not searching). Read by `--shots`
    /// / tests to confirm the filter reached the sidebar.
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Pre-seed a search query so the deterministic `--shots` render shows the
    /// filtered sidebar (there is no live event loop to type into the field).
    pub fn with_search_shot(mut self, query: &str) -> Self {
        self.search_query = query.to_string();
        self
    }

    /// Start with the sidebar collapsed — used by the `--shots` capture that
    /// proves the docked sidebar can be hidden to reclaim content width.
    pub fn with_sidebar_collapsed_shot(mut self) -> Self {
        self.sidebar_collapsed = true;
        self
    }

    /// The glyph the sidebar-toggle button shows. A hamburger reads as
    /// "toggle the navigation" in both states, so the button stays legible
    /// whether the sidebar is docked or collapsed.
    fn collapse_glyph(&self) -> &'static str {
        "\u{f035c}" // menu / hamburger
    }

    /// The header shown above the content pane: a sidebar-toggle button, the
    /// category's own glyph, the category title, and a one-line subtitle,
    /// closed off by a hairline rule. Rendered as a plain (borderless) block
    /// so it reads as a page title rather than another card — the cards below
    /// it are the raised surfaces, the header sits on the page background.
    fn build_pane_header(&self) -> Box<dyn Widget> {
        let theme = ui::Theme::dark();

        // The toggle button hides/shows the docked sidebar, reclaiming its
        // width for content. It writes a one-shot CRDT key that `sync` reads.
        let toggle_btn = Button::new(
            "settings.toggle_sidebar",
            self.collapse_glyph(),
            Some(TOGGLE_SIDEBAR_KEY),
        )
        .with_size(38.0, 38.0);

        // Title line: the title first (so it measures and draws — the same
        // `Row[Text, Spacer, action]` shape the card headers use), then the
        // trailing sidebar-toggle button. A leading `Text` in a Row draws
        // reliably; a trailing one can collapse to zero width, so title order
        // matters here.
        let title_row = Row::new()
            .push(Box::new(Text::new(self.selected.label()).with_size(24.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(toggle_btn));

        // Subtitle on its own line, dimmed, leading `Text` so it draws.
        let subtitle_row = Row::new()
            .push(Box::new(
                Text::new(self.selected.subtitle())
                    .with_size(13.0)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(Spacer::new()));

        // The title band. Wrapping the content in a background `Container` is
        // what makes the large title `Text` measure and draw reliably — a
        // borderless wrapper collapses the leading title to zero width (the
        // same cached-draw-path quirk `card()` documents). The band reads as
        // the pane's headerbar, above the raised content cards.
        let stack = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(title_row))
            .push(crate::widgets::vgap(5.0))
            .push(Box::new(subtitle_row));
        Box::new(Container::new().with_background().with_child(Box::new(stack)))
    }

    /// Build the content pane: header + the selected category's body.
    ///
    /// The body is a vertical stack of cards. We deliberately use a plain
    /// `Column` (filling its pane) rather than a `Scrollable` wrapper: in
    /// the retained-mode layout a `Scrollable` whose child sizes itself
    /// intrinsically collapses to a zero-height viewport unless the host
    /// drives its measure pass, which the standalone settings window does
    /// not. The panels fit the window at the sizes they ship; making them
    /// scroll is a follow-up that belongs in `ui` (a measured
    /// `Scrollable` viewport), noted in the growth ledger.
    fn build_content(&self) -> Box<dyn Widget> {
        let header = self.build_pane_header();
        let body = self.build_pane_body();
        // A deliberate gap between the title band and the first section card
        // so the title has breathing room (the column is block-display and
        // ignores the toolkit's inter-child gap).
        let inner = Column::new()
            .with_intrinsic_height()
            .push(header)
            .push(crate::widgets::vgap(20.0))
            .push(body);
        Box::new(Container::new().filling().with_child(Box::new(inner)))
    }

    /// Dispatch to the per-category pane body.
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            Category::Network => panes::network::build(),
            Category::Bluetooth => panes::bluetooth::build(),
            Category::Sound => panes::sound::build(),
            Category::Displays => panes::display::build(),
            Category::MouseKeyboard => panes::mouse_keyboard::build(),
            Category::KeyboardShortcuts => panes::keyboard_shortcuts::build(),
            Category::TimeDate => panes::time_date::build(),
            Category::Users => panes::users::build(),
            Category::Printers => panes::printers::build(),
            Category::Power => panes::power::build(),
            Category::Appearance => panes::appearance::build(),
            Category::ExternalAccounts => panes::accounts::build(),
            Category::RegionFormats => panes::region::build(),
            Category::Permissions => panes::permissions::build(),
            Category::Updates => panes::updates::build(),
            Category::About => panes::about::build(),
        }
    }

    /// Build the responsive split: the cohesive category sidebar on the left
    /// (see [`crate::sidebar::build`]) and the selected category's content
    /// pane on the right, wrapped in a [`NavSplit`] that docks the sidebar
    /// when wide and turns it into an overlay flap when narrow.
    ///
    /// The split is rebuilt fresh here (rather than mutated in place) because
    /// the content child depends on `self.selected`; rebuilding is how a
    /// category switch reaches the screen. The live `ui::App` loop never
    /// drives `NavSplit::update` for this app, so the flap is snapped — not
    /// eased — to its resolved open state right after construction (the same
    /// approach `tube` uses for its rebuilt nav rail).
    fn build_split(&self) -> NavSplit {
        let sidebar = sidebar::build(self.selected, &self.search_query);
        let content = self.build_content();
        let mut split = NavSplit::new(sidebar, content)
            .with_mode(SplitMode::Adaptive)
            .with_breakpoint(NAV_BREAKPOINT)
            .with_sidebar_width(sidebar::SIDEBAR_WIDTH);
        // An explicit collapse hides the docked sidebar and reclaims its
        // width for content. Snap (not ease) so the rebuilt split lands in
        // the right state immediately. This takes precedence over the shots
        // force-open below.
        if self.sidebar_collapsed {
            split.snap_open(false);
        } else if self.force_sidebar_open {
            // Open the overlay flap for shots; docked layouts open by
            // default so this is a no-op there.
            split.snap_open(true);
        }
        split
    }
}

impl Widget for SettingsApp {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::{transaction::Transactable, ReadDoc, Value};

        // Category selection: each sidebar row writes `settings.select.<slug>`
        // = true on click. Reading it true here switches the active category
        // and (via `select`) flags `structure_dirty` so the host rebuilds the
        // layout — that rebuild is what moves the content pane to the new
        // category. The first key seen wins; we clear it so the selection is
        // a one-shot edge, not a sticky state.
        for category in Category::ALL {
            let key = format!("settings.select.{}", category.slug());
            let pressed = matches!(
                state.get(path, key.as_str()),
                Ok(Some((Value::Scalar(s), _)))
                    if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if pressed {
                let _ = state.put(path, key.as_str(), automerge::ScalarValue::Boolean(false));
                self.select(*category);
            }
        }

        // Sidebar collapse toggle: the header button writes
        // `settings.toggle_sidebar = true` on click. Reading it true flips
        // the collapse state, persists it, flags a rebuild, and clears the
        // bit so it's a one-shot edge.
        let toggle_pressed = matches!(
            state.get(path, TOGGLE_SIDEBAR_KEY),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if toggle_pressed {
            let _ = state.put(path, TOGGLE_SIDEBAR_KEY, automerge::ScalarValue::Boolean(false));
            self.sidebar_collapsed = !self.sidebar_collapsed;
            self.structure_dirty = true;
        }

        // In `--shots` there is no event loop to type into the search field,
        // so a pre-seeded query (via `with_search_shot`) is mirrored into the
        // field's key once, so the field visibly shows the query it filtered
        // by. Harmless live: `search_query` starts empty there.
        if !self.search_query.is_empty()
            && read_search(state, path).is_none()
        {
            let _ = state.put(path, sidebar::SEARCH_KEY, self.search_query.as_str());
        }

        // Search query: the sidebar's search field writes `settings.search`.
        // Reading a changed value here updates the filter and flags a rebuild
        // so the sidebar's filtered results list is recomposed. The field
        // re-reads the same key on its own `sync`, so the query text survives
        // that rebuild; keeping the field the first focusable keeps the caret.
        if let Some(q) = read_search(state, path) {
            if q != self.search_query {
                self.search_query = q;
                self.structure_dirty = true;
            }
        }

        if let Some(split) = &mut self.split {
            // Re-assert a collapse every frame so a window resize (which can
            // reset `NavSplit`'s flap preference in adaptive mode) never
            // silently re-reveals a collapsed sidebar. When NOT collapsed we
            // leave the flap alone so the narrow overlay's open/close (and
            // the shots force-open) keep working.
            if self.sidebar_collapsed && split.is_sidebar_open() {
                split.snap_open(false);
            }
            split.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Rebuild the split for the current selection. The host re-runs this
        // pass whenever `take_structure_dirty` reports true (i.e. after a
        // category change), so the freshly-built content pane is what reaches
        // the screen.
        let mut split = self.build_split();
        let split_node = split.build_layout(taffy, theme, node_map);
        self.split = Some(split);

        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[split_node]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let (Some(split), Ok(children)) = (&mut self.split, taffy.children(node)) {
            if let Some(&child_node) = children.first() {
                split.apply_layout(
                    taffy,
                    child_node,
                    vello::kurbo::Point::new(self.rect.x0, self.rect.y0),
                );
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(split) = &mut self.split {
            split.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        if let Some(split) = &mut self.split {
            split.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        if let (Some(split), Ok(children)) = (&mut self.split, taffy.children(node_id)) {
            if let Some(&child_node) = children.first() {
                split.handle_event(
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
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.split
            .as_ref()
            .map(|s| s as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.split
            .as_mut()
            .map(|s| s as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "SettingsApp"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    /// Drains the selection-rebuild flag. Returning true tells the host's
    /// frame loop to rebuild the taffy tree (re-running `build_layout`), which
    /// is how a category switch reaches the content pane. Also bubbles any
    /// child's own structure-dirty signal.
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .split
            .as_mut()
            .map(|s| s.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.split.as_ref().is_some_and(|s| s.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.split.as_ref().is_some_and(|s| s.needs_idle_sync())
    }
}

/// Read the sidebar search query from the document. The field writes either a
/// plain string scalar or an Automerge `Text` object, so handle both.
fn read_search(state: &automerge::AutoCommit, path: &automerge::ObjId) -> Option<String> {
    use automerge::{ReadDoc, Value};
    match state.get(path, sidebar::SEARCH_KEY) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

impl AppWidget for SettingsApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        "Settings"
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0493}") // cog
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};

    fn layout_at(app: &mut SettingsApp, w: f32, h: f32) {
        let theme = ui::Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(w),
                        height: taffy::Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let measure_map = node_map.clone();
        let mut tr = ui::text_render::TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
    }

    #[test]
    fn new_starts_on_default_category() {
        assert_eq!(SettingsApp::new().selected(), Category::DEFAULT);
    }

    #[test]
    fn with_selected_overrides_default() {
        assert_eq!(
            SettingsApp::with_selected(Category::About).selected(),
            Category::About
        );
    }

    #[test]
    fn select_changes_category_and_flags_structure_dirty() {
        let mut app = SettingsApp::new();
        app.select(Category::Users);
        assert_eq!(app.selected(), Category::Users);
        assert!(
            app.take_structure_dirty(),
            "a real category change must flag a rebuild so the pane updates"
        );
        // Re-selecting the same category is a no-op (no spurious rebuild).
        app.select(Category::Users);
        assert!(!app.take_structure_dirty());
    }

    #[test]
    fn wide_layout_docks_the_sidebar() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
        assert!(!app.is_narrow(), "1200px should dock the sidebar");
        assert!(app.sidebar_open(), "docked sidebar is open by default");
    }

    #[test]
    fn narrow_layout_uses_the_overlay() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.is_narrow(), "440px should resolve to the overlay layout");
    }

    #[test]
    fn shot_flag_opens_overlay_when_narrow() {
        let mut app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(440.0)
            .with_sidebar_open_shot();
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.is_narrow());
        assert!(app.sidebar_open(), "the shot flag forces the overlay flap open");
    }

    #[test]
    fn toggle_key_collapses_then_expands_sidebar() {
        use automerge::transaction::Transactable;
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
        assert!(!app.sidebar_collapsed(), "starts expanded");
        assert!(app.sidebar_open(), "docked sidebar is open by default");

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;

        // Simulate the header collapse button writing its key, then sync.
        let _ = doc.put(&root, TOGGLE_SIDEBAR_KEY, true);
        app.sync(&mut doc, &root);
        assert!(app.sidebar_collapsed(), "first toggle collapses the sidebar");
        assert!(
            app.take_structure_dirty(),
            "collapsing requests a rebuild so the content reclaims the space"
        );
        // Rebuild + re-layout as the host would; the collapsed split hides
        // the sidebar.
        layout_at(&mut app, 1200.0, 760.0);
        assert!(!app.sidebar_open(), "collapsed docked sidebar reads as closed");

        // Toggle again → expands.
        let _ = doc.put(&root, TOGGLE_SIDEBAR_KEY, true);
        app.sync(&mut doc, &root);
        assert!(!app.sidebar_collapsed(), "second toggle re-expands");
        layout_at(&mut app, 1200.0, 760.0);
        assert!(app.sidebar_open(), "re-expanded sidebar is open again");
    }

    #[test]
    fn search_query_updates_from_document_and_flags_rebuild() {
        use automerge::transaction::Transactable;
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
        assert_eq!(app.search_query(), "", "starts with no query");

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        // The sidebar search field writes SEARCH_KEY; sync adopts it.
        let _ = doc.put(&root, sidebar::SEARCH_KEY, "night light");
        app.sync(&mut doc, &root);
        assert_eq!(app.search_query(), "night light");
        assert!(
            app.take_structure_dirty(),
            "a new query must flag a rebuild so the filtered sidebar recomposes"
        );
        // Re-syncing the same query is a no-op (no spurious rebuild).
        app.sync(&mut doc, &root);
        assert!(!app.take_structure_dirty());
    }

    #[test]
    fn seeded_search_shot_query_reaches_the_sidebar_and_field() {
        use automerge::{transaction::Transactable, ReadDoc};
        // The `--shots` seed path mirrors a pre-set query into the field key so
        // the rendered field shows it, and drives the filter.
        let mut app = SettingsApp::with_selected(Category::Displays).with_search_shot("light");
        assert_eq!(app.search_query(), "light");
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        // The field key was seeded so the TextInput renders the query.
        assert!(
            doc.get(&root, sidebar::SEARCH_KEY).unwrap().is_some(),
            "the seeded query is written to the field key"
        );
        // And it still lays out (the filtered sidebar path).
        let _ = { layout_at(&mut app, 1200.0, 760.0); };
    }

    #[test]
    fn wide_and_narrow_both_lay_out_without_panicking() {
        let mut wide = SettingsApp::with_selected(Category::ExternalAccounts);
        layout_at(&mut wide, 1200.0, 720.0);
        assert!(wide.get_rect().width() > 0.0);

        let mut narrow = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(440.0)
            .with_sidebar_open_shot();
        layout_at(&mut narrow, 440.0, 820.0);
        assert!(narrow.get_rect().width() > 0.0);
    }
}
