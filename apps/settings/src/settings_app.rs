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
use ui::widget::{Button, Column, Container, NavSplit, Row, Scrollable, Spacer, SplitLayout, SplitMode, TextInput, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// CRDT key the header's collapse/expand button writes `true` to. Read in
/// [`SettingsApp::sync`], it flips [`SettingsApp::sidebar_collapsed`].
pub const TOGGLE_SIDEBAR_KEY: &str = "settings.toggle_sidebar";

/// At or above this allocated width (logical px) the sidebar docks beside the
/// content; below it the sidebar becomes an overlay flap. Sized so a typical
/// settings window is docked while a narrow / split-screen placement gets the
/// overlay.
pub const NAV_BREAKPOINT: f64 = 720.0;

/// Height (logical px) of the top control strip that holds the sidebar toggle
/// and the search field.
const TOP_BAR_HEIGHT: f32 = 54.0;

/// Width (logical px) the search field is pinned to in the top strip — sized
/// so, with the toggle button and the row gap, it sits over the sidebar
/// column rather than stretching across the content pane.
const SEARCH_FIELD_WIDTH: f32 = 200.0;

/// Left inset (logical px) of the top strip's content from the window edge.
const TOP_BAR_INSET_X: f32 = 14.0;

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
    /// The top control strip (sidebar toggle + search field), rebuilt each
    /// `build_layout`. It lives at the app's **top level** — in the host's
    /// main taffy tree, NOT inside the `NavSplit` — so the search field is
    /// reachable by the App's focus machinery. (`NavSplit` lays its children
    /// out in an isolated inner tree the focus walk can't see, which is why
    /// a search field nested in the sidebar could never take or keep keyboard
    /// focus; hoisting it here is what makes clicking the field focus it and
    /// keeps that focus across the filter rebuild.)
    top_bar: Option<Box<dyn Widget>>,
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
            top_bar: None,
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

    /// Build the top control strip: a single, obvious sidebar toggle on the
    /// left and the settings search field beside it. This is the app's only
    /// top chrome and it holds **controls only** — no app-name / page-title
    /// header (the OS draws the window-group pill from [`AppWidget::app_title`],
    /// so an app-drawn title would duplicate it).
    ///
    /// The strip is built here, at the app's top level, so it lands in the
    /// host's **main** taffy tree. That is essential for the search field:
    /// widgets inside the `NavSplit` are laid out in an isolated inner tree
    /// the App's focus walk never visits, so a field nested there can neither
    /// take a click's focus reliably nor keep it across the per-keystroke
    /// filter rebuild. Hoisted here, the field is in `focusable_order`, so a
    /// click focuses it and `autofocus_on_structure_change` restores that
    /// focus (and the caret) after the sidebar rebuilds to show the filtered
    /// results.
    fn build_top_bar(&self) -> Box<dyn Widget> {
        let theme = ui::Theme::dark();

        // Single, obvious sidebar toggle (hamburger), on the LEFT — the
        // conventional place. Writes a one-shot CRDT key `sync` reads to flip
        // the docked sidebar's collapsed state.
        let toggle_btn = Button::new(
            "settings.toggle_sidebar",
            self.collapse_glyph(),
            Some(TOGGLE_SIDEBAR_KEY),
        )
        .with_size(38.0, 38.0);

        // The search field. Bound to `SEARCH_KEY`; its value is re-read from
        // the document on every `sync`, so the text survives the sidebar's
        // filter rebuild. `SettingsApp::sync` reads the same key to filter the
        // category list below.
        let search = TextInput::new(Vec::new())
            .with_id("settings.search")
            .with_on_change_path(sidebar::SEARCH_KEY)
            .with_placeholder("\u{f0349}  Search settings")
            .with_font_size(13.0);
        // Pin the field to a comfortable width so it sits over the sidebar
        // column rather than stretching across the content, and give it a
        // field-height box so it reads as an input, not a bare line.
        let search_box = Container::new()
            .with_no_padding()
            .with_size(SEARCH_FIELD_WIDTH, 36.0)
            .with_child(Box::new(search));

        // A leading inset keeps the toggle off the window edge; the row's own
        // inter-child gap spaces the toggle from the field. `Spacer` soaks up
        // the trailing width so the field stays left, over the sidebar.
        let row = Row::new()
            .push(crate::widgets::hgap(TOP_BAR_INSET_X))
            .push(Box::new(toggle_btn))
            .push(Box::new(search_box))
            .push(Box::new(Spacer::new()));

        // A fixed-height strip tinted a step below the content so it reads as a
        // header band — without ever naming the app. No inner padding: the row
        // owns its own inset and the toggle/field are taller than a padded box
        // would allow.
        let surface = theme.tokens.colors.surfaces.base;
        Box::new(
            Container::new()
                .with_no_padding()
                .with_height(TOP_BAR_HEIGHT)
                .with_fill(surface)
                .with_child(Box::new(row)),
        )
    }

    /// Build the content pane: the selected category's body, wrapped in a
    /// vertical [`Scrollable`] so tall panes (many section cards on a short
    /// window) scroll with a real overlay scrollbar — thumb drag, track
    /// paging, PageUp/Down + Home/End, wheel — instead of being clipped.
    ///
    /// There is **no app-drawn title header** here: the category's identity is
    /// carried by its active sidebar row and the OS window-group pill, not by a
    /// duplicated in-pane title. The pane starts directly with its first
    /// section card, inset from the top so it has breathing room.
    ///
    /// The intrinsic-height inner `Column` provides the content extent; the
    /// `Scrollable`'s `percent(1.0)` height resolves against the filling
    /// `Container` (a definite height under the live `ui::App`, which drives
    /// the measure pass), so the viewport doesn't collapse. When the content
    /// fits, the bar hides itself and nothing changes visually.
    fn build_content(&self) -> Box<dyn Widget> {
        let body = self.build_pane_body();
        let inner = Column::new()
            .with_intrinsic_height()
            .push(crate::widgets::vgap(6.0))
            .push(body);
        let scroll = Scrollable::new_vertical().with_child(Box::new(inner));
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
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

        // The search field edits an Automerge `Text` object; ensure it exists
        // so typed characters actually persist to the document (a `TextInput`
        // only writes through an existing Text object — without this the field
        // would update just its local buffer and the filter would never see the
        // query, and the text would vanish on the next rebuild).
        let search_obj = crate::ensure_text_object(state, path, sidebar::SEARCH_KEY);
        // In `--shots` there is no event loop to type into the field, so a
        // pre-seeded query (via `with_search_shot`) is spliced into the object
        // once so the field visibly shows the query it filtered by. Harmless
        // live: `search_query` starts empty there.
        if !self.search_query.is_empty() {
            if let Ok(existing) = state.text(&search_obj) {
                if existing.is_empty() {
                    let _ = state.splice_text(&search_obj, 0, 0, &self.search_query);
                }
            }
        }

        // Search query: the top strip's search field writes `settings.search`.
        // Reading a changed value here updates the filter and flags a rebuild
        // so the sidebar's filtered results list is recomposed. The field
        // re-reads the same key on its own `sync`, so the query text survives
        // that rebuild; because the field is a top-level focusable (not buried
        // in the `NavSplit`), `autofocus_on_structure_change` restores focus +
        // caret to it by ordinal after the rebuild, so typing keeps flowing.
        if let Some(q) = read_search(state, path) {
            if q != self.search_query {
                self.search_query = q;
                self.structure_dirty = true;
            }
        }

        // Sync the top strip so the search field reads its bound value back.
        if let Some(top) = &mut self.top_bar {
            top.sync(state, path);
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
        // The app is a vertical stack: a fixed-height top control strip
        // (toggle + search) over the responsive split. The strip is built into
        // the *main* taffy tree here (not inside the `NavSplit`) so its search
        // field is reachable by the host's focus machinery — see
        // [`Self::build_top_bar`].
        let mut top_bar = self.build_top_bar();
        let top_node = top_bar.build_layout(taffy, theme, node_map);
        self.top_bar = Some(top_bar);

        // Rebuild the split for the current selection. The host re-runs this
        // pass whenever `take_structure_dirty` reports true (i.e. after a
        // category change), so the freshly-built content pane is what reaches
        // the screen.
        let mut split = self.build_split();
        let split_node = split.build_layout(taffy, theme, node_map);
        self.split = Some(split);
        // The split fills the height left under the fixed strip. `NavSplit`
        // builds a `percent(1.0)`-height leaf; override it to grow into the
        // remaining space instead of over-running past the strip.
        let _ = taffy.set_style(
            split_node,
            taffy::Style {
                flex_grow: 1.0,
                flex_basis: taffy::Dimension::length(0.0),
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                min_size: taffy::Size {
                    width: taffy::Dimension::auto(),
                    height: taffy::Dimension::length(0.0),
                },
                ..Default::default()
            },
        );

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[top_node, split_node]).unwrap();
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
        let origin = vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            // children[0] = top strip, children[1] = split.
            if let (Some(&top_node), Some(top)) = (children.first(), &mut self.top_bar) {
                top.apply_layout(taffy, top_node, origin);
            }
            if let (Some(&split_node), Some(split)) = (children.get(1), &mut self.split) {
                split.apply_layout(taffy, split_node, origin);
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
        // Draw the strip after the split so it is never occluded by the
        // content pane's background.
        if let Some(top) = &mut self.top_bar {
            top.draw(scene, transform, text_renderer, theme, presence);
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
        if let Some(top) = &mut self.top_bar {
            top.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
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
        if let Ok(children) = taffy.children(node_id) {
            // Route to the top strip first (search + toggle), then the split.
            if let (Some(&top_node), Some(top)) = (children.first(), &mut self.top_bar) {
                top.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, top_node,
                    taffy,
                );
            }
            if let (Some(&split_node), Some(split)) = (children.get(1), &mut self.split) {
                split.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, split_node,
                    taffy,
                );
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        // Order MUST match the taffy children order [top_bar, split] so the
        // host's focus walk (which zips `children()` with `taffy.children`)
        // reaches the search field in the top strip.
        let mut out: Vec<&dyn Widget> = Vec::new();
        if let Some(top) = self.top_bar.as_ref() {
            out.push(top.as_ref());
        }
        if let Some(split) = self.split.as_ref() {
            out.push(split as &dyn Widget);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if let Some(top) = self.top_bar.as_mut() {
            out.push(top.as_mut());
        }
        if let Some(split) = self.split.as_mut() {
            out.push(split as &mut (dyn Widget + '_));
        }
        out
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
        let split_child = self
            .split
            .as_mut()
            .map(|s| s.take_structure_dirty())
            .unwrap_or(false);
        let top_child = self
            .top_bar
            .as_mut()
            .map(|t| t.take_structure_dirty())
            .unwrap_or(false);
        own || split_child || top_child
    }

    fn needs_animation_frame(&self) -> bool {
        self.split.as_ref().is_some_and(|s| s.needs_animation_frame())
            || self.top_bar.as_ref().is_some_and(|t| t.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.split.as_ref().is_some_and(|s| s.needs_idle_sync())
            || self.top_bar.as_ref().is_some_and(|t| t.needs_idle_sync())
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
        use automerge::ReadDoc;
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
