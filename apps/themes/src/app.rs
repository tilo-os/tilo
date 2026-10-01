//! Root widget for the Themes app. Owns the theme catalog, the three
//! panels (gallery / preview / customizer) and the top bar, and runs the
//! per-frame reconciliation that keeps the durable CRDT catalog separate
//! from per-window editor controls and the selected [`ThemeSpec`].
//!
//! ## Responsive layout
//!
//! The gallery is the app's **primary navigation sidebar**. On a wide window
//! it stays docked beside the preview + customizer. When the window is narrower
//! than `BREAKPOINT` the gallery collapses into an overlay *flap*: the preview +
//! customizer take the full width and the gallery, closed by default, slides
//! in over a dimming scrim when summoned by the top-bar hamburger. The
//! breakpoint is driven by the app's own allocated width and the eased
//! reveal uses the shared [`FlapState`] kernel.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_actions::EncodedAction;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing};
use ui::widget::toggle_group::Segment;
use ui::widget::{Button, FlapState, SplitLayout, ToggleGroup, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::customizer::Customizer;
use crate::doc;
use crate::draw;
use crate::gallery::Gallery;
use crate::model::{FONTS, ThemeSpec, TokenId, default_catalog, demo_catalog};
use crate::preview::PreviewPane;
use crate::store;

/// Height of the top bar (logical px) at 1×: one `bar_height` control with an
/// `s2` inset above and below it. The bar grows when its actions wrap onto a
/// second line under large text; the live height is measured in
/// `apply_topbar_layout`. Public so geometry tests assert the relationship
/// "content starts one gutter below the bar" rather than retyping the number.
pub const TOPBAR_H: f32 = Metrics::BASE.bar_height + 2.0 * Spacing::BASE.s2;
/// Height of every top-bar control — the shared `Button` default
/// (`bar_height`), so the bar's actions, the gallery toggle and the
/// customizer's controls are one height across the app.
const TOPBAR_CONTROL_H: f32 = Metrics::BASE.bar_height;
const GALLERY_W: f32 = 272.0;
const CUSTOMIZER_W: f32 = 344.0;
/// Inset (logical px) of the collapsed layout's content from the window edge
/// and from the top bar — the toolkit's `s4` step, which the top bar also
/// uses for its own horizontal inset so the toggle and the content share an
/// edge.
pub const NARROW_CONTENT_GUTTER: f32 = Spacing::BASE.s4;
/// Allocated-width breakpoint (logical px). At or above this the gallery is a
/// docked sidebar beside the preview + customizer; below it the gallery
/// becomes an overlay flap. Chosen so the gallery, a usable preview, and the
/// customizer stop fitting side by side (`GALLERY_W + CUSTOMIZER_W` plus a
/// readable preview column).
const BREAKPOINT: f64 = 840.0;

/// Panic-free node construction for [`Widget::build_layout`].
/// `TaffyTree::new_with_children` is typed `-> TaffyResult<NodeId>` for API
/// generality, but only fails for a child `NodeId` belonging to a
/// *different* tree — never the case here, since every child passed in was
/// just built in this same `taffy` tree by the call immediately above each
/// call site. `Widget::build_layout` returns a bare `NodeId` (not a
/// `Result`) across every implementor, so surfacing a theoretical error here
/// would mean threading a `Result` through the whole toolkit for a branch
/// that can't be taken.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent view flags of the app root, not a state machine"
)]
pub struct ThemesApp {
    catalog: Vec<ThemeSpec>,
    topbar_buttons: Vec<Button>,
    top_commands: Rc<RefCell<Vec<String>>>,
    gallery: Gallery,
    preview: PreviewPane,
    narrow_preview: bool,
    narrow_view_chosen: bool,
    narrow_view_request: Rc<Cell<Option<bool>>>,
    narrow_tabs: ToggleGroup,
    narrow_tabs_node: Option<NodeId>,
    customize_scroll: f64,
    preview_scroll: f64,
    customizer: Customizer,
    /// Per-window navigation/editor state. Never placed in the collaborative
    /// Automerge document or exposed to presence/history.
    selected: String,
    edit_token: TokenId,

    rect: Rect,
    topbar_node: Option<NodeId>,
    topbar_actions_node: Option<NodeId>,
    content_node: Option<NodeId>,
    gallery_node: Option<NodeId>,
    preview_node: Option<NodeId>,
    customizer_node: Option<NodeId>,

    /// Eased open/closed state of the gallery sidebar (shared toolkit kernel).
    gallery_flap: FlapState,
    /// Layout class resolved last frame, used to detect breakpoint crossings
    /// and to know whether to rebuild the taffy tree.
    last_layout: Option<SplitLayout>,
    /// Width fed into the *next* `build_layout`. Comes from the previous
    /// frame's allocated width (live) or a shots viewport seed (headless), so
    /// the taffy structure can collapse the gallery column before its rect is
    /// known.
    build_width: f64,
    /// Wall-clock of the last reveal tick (frame-rate-independent easing).
    last_tick: Instant,
    /// The control that summons / dismisses the overlay gallery: a shared
    /// icon-only [`Button`] in the top bar's leading slot (overlay mode only),
    /// so pointer, keyboard and assistive activation all route through the
    /// toolkit's control rather than a hand-painted rect. Always the button
    /// for the *current* state — the hamburger while the gallery is hidden,
    /// the close glyph once it is shown; `gallery_toggle_hidden` holds the
    /// other and the two swap when the flap crosses half-open (`Button` has
    /// no `set_icon`, so the glyph changes by swapping widgets and rebuilding
    /// the tree once).
    gallery_toggle: Button,
    gallery_toggle_hidden: Button,
    /// Whether `gallery_toggle` currently shows the close glyph.
    toggle_shows_close: bool,
    /// Set by either toggle's click handler, consumed in `sync`.
    gallery_toggle_request: Rc<Cell<bool>>,
    topbar_toggle_node: Option<NodeId>,
    /// The top bar's laid-out rect. Its actions may wrap onto a second line
    /// under large text, so the bar's live height is measured here rather
    /// than read from [`TOPBAR_H`].
    topbar_rect: Rect,
    /// On-screen rect of the gallery flap while it overlays the content.
    gallery_overlay_rect: Rect,
    /// Last known cursor position, for hit-testing the scrim.
    last_cursor: Point,

    bound_key: Option<(String, TokenId)>,
    status: String,
    last_export: Option<String>,
    structure_dirty: bool,

    /// Framework dispatcher when hosted; the local action host otherwise.
    actions: crate::resource::ActionSink,

    /// Theme id accepted through the canonical external-operation route. It is
    /// drained by `sync`, where mutable document access is available, so the
    /// store write and active-id action remain one semantic transaction.
    pending_apply: Option<(String, tilo_actions::InvocationId)>,
    completed_apply: Option<tilo_actions::InvocationUpdate>,
    pending_new: bool,

    /// Vertical scroll of the collapsed (narrow) preview+customizer stack.
    scroll: f64,
    /// Total stacked content height + its viewport, cached in `apply_layout`
    /// so the wheel handler and scrollbar can clamp / size against them.
    scroll_content_h: f64,
    scroll_viewport_h: f64,
    /// Scene-space viewport rect of the scrolling content region (below the
    /// top bar), in the narrow layout.
    content_rect: Rect,

    /// `--shots` / demo-walkthrough scenario seeds, grouped so the struct
    /// doesn't collect a pile of same-shaped bools that are only ever
    /// meaningful together (see [`ShotSeed`]).
    shot: ShotSeed,
}

/// `--shots` scenario seeds (headless single-frame capture) and the
/// demo-walkthrough catalog switch. Never touched on the live path.
#[derive(Default)]
struct ShotSeed {
    /// Force a selection (e.g. a preset with a distinctive font) for a shot.
    select: Option<String>,
    /// Open the sidebar actions menu on the selected row for a shot.
    menu: bool,
    /// Scroll the narrow stack partway for the screenshot.
    scroll: bool,
    /// When set, first-run seeding uses the demo catalog (presets + one
    /// editable custom theme) instead of the live presets-only default — for
    /// `--shots` and "try it" walkthroughs, never the live path.
    demo: bool,
}

impl Default for ThemesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemesApp {
    pub fn new() -> Self {
        let top_commands = Rc::new(RefCell::new(Vec::new()));
        let narrow_view_request = Rc::new(Cell::new(None));
        let preview_request = narrow_view_request.clone();
        // The overlay gallery's toggle is the shared icon-only Button, sized
        // to the bar control height, with its spoken name carried by the
        // tooltip (an icon-only Button announces its tooltip). It is a
        // layout participant of the top bar, never painted over. One button
        // per state — hamburger to show, close glyph to hide — both posting
        // the same request; the app swaps them as the flap opens and closes.
        let gallery_toggle_request = Rc::new(Cell::new(false));
        let gallery_toggle_button = |icon: Icon, name: &str| {
            let toggle_request = gallery_toggle_request.clone();
            Button::new("themes.gallery_toggle", "", None)
                .with_icon(icon)
                .with_size(TOPBAR_CONTROL_H, TOPBAR_CONTROL_H)
                .tooltip(name)
                .on_view(move || toggle_request.set(true))
        };
        let gallery_toggle = gallery_toggle_button(Icon::Menu, "Show themes");
        let gallery_toggle_hidden = gallery_toggle_button(Icon::X, "Hide themes");
        let narrow_tabs = ToggleGroup::new(
            vec![Segment::new("Customize"), Segment::new("Full preview")],
            0,
        )
        .on_view("themes.view", move |index| {
            preview_request.set(Some(index == 1));
        });
        Self {
            narrow_preview: false,
            narrow_view_chosen: false,
            narrow_view_request,
            narrow_tabs,
            narrow_tabs_node: None,
            customize_scroll: 0.0,
            preview_scroll: 0.0,
            catalog: default_catalog(),
            topbar_buttons: vec![
                Button::new("themes.new", "New", None)
                    .on_action(crate::actions::new_default_binding())
                    .fit_content(TOPBAR_CONTROL_H),
                Button::new("themes.export", "Export JSON", None)
                    .on_action(crate::actions::export_selected_binding())
                    .fit_content(TOPBAR_CONTROL_H),
                Button::new("themes.apply", "Apply theme", None)
                    .on_action(crate::actions::apply_selected_binding())
                    .fit_content(TOPBAR_CONTROL_H)
                    .primary(),
            ],
            top_commands,
            gallery: Gallery::new(),
            preview: PreviewPane::new(),
            customizer: Customizer::new(),
            selected: String::new(),
            edit_token: TokenId::Accent,
            rect: Rect::ZERO,
            topbar_node: None,
            topbar_actions_node: None,
            content_node: None,
            gallery_node: None,
            preview_node: None,
            customizer_node: None,
            // Open-when-docked is the natural navigation-sidebar default: the
            // gallery shows on a wide window and is closed (overlay) when narrow.
            gallery_flap: FlapState::open_when_docked(),
            last_layout: None,
            // Seed wide so the first frame (and the default 1280px shot) builds
            // the docked layout without a structure round-trip.
            build_width: BREAKPOINT,
            last_tick: ui::time::now(),
            gallery_toggle,
            gallery_toggle_hidden,
            toggle_shows_close: false,
            gallery_toggle_request,
            topbar_toggle_node: None,
            topbar_rect: Rect::ZERO,
            gallery_overlay_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            bound_key: None,
            status: String::new(),
            last_export: None,
            structure_dirty: false,
            actions: crate::resource::ActionSink::default(),
            pending_apply: None,
            completed_apply: None,
            pending_new: false,
            scroll: 0.0,
            scroll_content_h: 0.0,
            scroll_viewport_h: 0.0,
            content_rect: Rect::ZERO,
            shot: ShotSeed::default(),
        }
    }

    /// Like [`Self::new`] but seeds the demo catalog (presets + one editable
    /// custom theme) on first run — for `--shots` and demo walkthroughs so a
    /// capture shows the customizer editing a real custom theme. Never the live
    /// first-run path.
    pub fn new_demo() -> Self {
        let mut app = Self::new();
        app.shot.demo = true;
        app.catalog = demo_catalog();
        app
    }

    /// Seed the responsive state for a deterministic headless screenshot.
    /// `--shots` renders a single frame with no event loop, so the layout
    /// class (which normally settles over the first live frame) is seeded from
    /// the requested viewport width here, and the flap is snapped to its
    /// endpoint so the overlay shows without an in-flight animation.
    ///
    /// * width `>= BREAKPOINT` → docked gallery (the wide studio layout).
    /// * width `<  BREAKPOINT` → overlay gallery, snapped **open** so the shot
    ///   captures the collapsed sidebar floating over the scrim.
    pub fn seed_shot_viewport(&mut self, width: f64) {
        self.seed_shot(width, "");
    }

    /// Configure a deterministic headless screenshot for one of the named
    /// scenarios (empty = default). The single-frame renderer has no live
    /// first frame, so the layout class + flap endpoint + any scenario state
    /// are seeded here.
    ///
    /// * `""`      — default: docked (wide) or the revealed overlay flap (narrow).
    /// * `"live"`      — narrow live preset preview; short views settle at its tail.
    /// * `"preview"`   — narrow custom preview; short views settle at its tail.
    /// * `"customize"` — narrow, flap closed, editable controls at the top.
    /// * `"menu"`      — open the sidebar actions menu on the selected custom row.
    /// * `"scroll"`    — narrow, flap closed, scrolled partway to prove the stack scrolls.
    /// * `"font"`      — select a preset with a distinctive (monospace) font so the
    ///   preview specimen visibly renders in it.
    pub fn seed_shot(&mut self, width: f64, scenario: &str) {
        self.build_width = width;
        let layout = SplitLayout::from_width(width, BREAKPOINT);
        self.last_layout = Some(layout);
        match scenario {
            "live" | "scroll" => {
                self.gallery_flap.snap(false);
                self.shot.scroll = true;
            }
            "customize" => self.gallery_flap.snap(false),
            "preview" => {
                self.gallery_flap.snap(false);
                self.narrow_view_chosen = true;
                self.set_narrow_preview(true);
                self.shot.scroll = true;
            }
            "menu" => {
                self.gallery_flap.snap(true);
                self.shot.menu = true;
                self.shot.select = Some("sunset".to_string());
            }
            "font" => {
                self.gallery_flap.snap(true);
                self.shot.select = Some("dracula".to_string());
            }
            _ => self.gallery_flap.snap(true),
        }
    }

    /// Resolve the current layout class purely from allocated width.
    fn layout_class(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BREAKPOINT)
    }

    /// Whether the hamburger toggle should be shown: only when the gallery is
    /// collapsible (overlay mode).
    fn show_toggle(&self) -> bool {
        SplitLayout::from_width(self.build_width, BREAKPOINT) == SplitLayout::Overlay
    }

    /// The top bar's on-screen band: its measured rect once laid out, else
    /// the 1× constant for a frame drawn before any layout.
    fn topbar_band(&self) -> Rect {
        if self.topbar_rect.height() > 0.0 {
            self.topbar_rect
        } else {
            Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x1,
                self.rect.y0 + f64::from(TOPBAR_H),
            )
        }
    }

    /// Show the toggle for the flap's state: the hamburger while the gallery
    /// is hidden, the close glyph once it is more than half revealed. The two
    /// buttons swap places and the tree is rebuilt once, so the glyph and
    /// the spoken name ("Show themes" / "Hide themes") change together.
    fn reconcile_gallery_toggle(&mut self) {
        let open = self.gallery_flap.reveal() > 0.5;
        if open != self.toggle_shows_close {
            std::mem::swap(&mut self.gallery_toggle, &mut self.gallery_toggle_hidden);
            self.toggle_shows_close = open;
            self.structure_dirty = true;
        }
    }

    /// Draw the vertical scroll indicator for the collapsed content stack,
    /// on the right edge of the content viewport. No-op when content fits.
    fn draw_scroll_indicator(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        if self.scroll_content_h <= self.scroll_viewport_h + 1.0 {
            return;
        }
        let vp = self.content_rect;
        let x1 = vp.x1 - 3.0;
        let x0 = x1 - 6.0;
        let track = Rect::new(x0, vp.y0 + 4.0, x1, vp.y1 - 4.0);
        // The track is a half lift of the content ground — measured against
        // it, so it shows on dark and light alike — rather than a declared
        // border colour washed to 35 %.
        draw::fill_rrect(scene, t, track, 3.0, theme.tokens.colors.lift(0.5));
        let ratio = (self.scroll_viewport_h / self.scroll_content_h).clamp(0.08, 1.0);
        let thumb_h = (track.height() * ratio).max(28.0);
        let max_scroll = (self.scroll_content_h - self.scroll_viewport_h).max(1.0);
        let frac = (self.scroll / max_scroll).clamp(0.0, 1.0);
        let thumb_y = track.y0 + (track.height() - thumb_h) * frac;
        let thumb = Rect::new(x0, thumb_y, x1, thumb_y + thumb_h);
        draw::fill_rrect(scene, t, thumb, 3.0, theme.text_dim.multiply_alpha(0.85));
    }

    fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Dispatch one [`ThemesAction`](crate::actions::ThemesAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. The action is applied
    /// through the Tilo action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// document heads captured just before the first action of the batch are
    /// stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)); Ctrl+Z then reverts it.
    fn perform_action(&mut self, state: &mut AutoCommit, action: &crate::actions::ThemesAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::THEMES_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.themes", "Themes", action),
        );
    }

    /// Route one command-bus signal. Presentation-only commands (selection, the
    /// edited colour slot, rename-focus) write view-state directly; every
    /// document-mutating command dispatches the matching
    /// [`ThemesAction`](crate::actions::ThemesAction) through
    /// [`Self::perform_action`], so a button and its Ctrl+K twin share one
    /// dispatch + undo path.
    fn process_command(&mut self, cmd: &str, state: &mut AutoCommit, path: &ObjId) {
        // --- Presentation: view-state only, never an action. ---
        if let Some(id) = cmd.strip_prefix("select:") {
            self.selected = id.to_string();
            self.status.clear();
            return;
        }
        if let Some(key) = cmd.strip_prefix("edittoken:") {
            self.edit_token = TokenId::from_key(key).unwrap_or(TokenId::Accent);
            return;
        }
        if let Some(id) = cmd.strip_prefix("rename:") {
            // Select the theme, then hand focus to the sidebar's inline editor.
            // The committed name change rides the customizer reconcile into an
            // upsert; the focus handoff itself is presentation.
            self.selected = id.to_string();
            self.gallery.request_name_focus();
            self.status.clear();
            return;
        }

        // --- Mutating: dispatch a ThemesAction. Context-menu commands carry the
        // target id explicitly (the right-clicked row need not be selected). ---
        if let Some(id) = cmd.strip_prefix("duplicate:") {
            self.duplicate_theme(id, state, path);
            return;
        }
        if let Some(id) = cmd.strip_prefix("delete:") {
            self.delete_theme(id, state, path);
            return;
        }
        let selected = self.selected.clone();
        match cmd {
            "new" => self.new_theme(state, path),
            "duplicate" => self.duplicate_theme(&selected, state, path),
            "delete" => self.delete_theme(&selected, state, path),
            "reset" => self.reset_theme(&selected, state),
            _ => {}
        }
    }

    /// Create a new editable theme (from the base preset), dispatched as an
    /// upsert, then select it (presentation).
    fn new_theme(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let catalog = doc::load_catalog(state, path).unwrap_or_default();
        let id = Self::new_id();
        let order = crate::actions::max_order(&catalog) + 1;
        let spec = crate::actions::new_theme_spec(&id, "New theme", order);
        self.perform_action(state, &crate::actions::upsert_custom_theme_action(&spec));
        self.selected = id;
        self.status = "Created a new theme".to_string();
        self.structure_dirty = true;
    }

    /// Duplicate the theme `id` into a new editable custom theme (dispatched as
    /// an upsert) and select it.
    fn duplicate_theme(&mut self, id: &str, state: &mut AutoCommit, path: &ObjId) {
        let catalog = doc::load_catalog(state, path).unwrap_or_default();
        if let Some(src) = catalog.iter().find(|s| s.id == id).cloned() {
            let new_id = Self::new_id();
            let order = crate::actions::max_order(&catalog) + 1;
            let spec = crate::actions::duplicate_spec(&src, &new_id, order);
            self.perform_action(state, &crate::actions::upsert_custom_theme_action(&spec));
            self.selected = new_id;
            self.status = format!("Duplicated “{}”", src.name);
            self.structure_dirty = true;
        }
    }

    /// Delete the custom theme `id` (presets are protected; never empties the
    /// catalog) via a delete action, then move the selection to a surviving
    /// theme. The builtin / last-theme guard is enforced both here (so the
    /// selection + status only move for a real delete) and in
    /// [`ThemesAction::apply`](crate::actions::ThemesAction) (so a replay is a
    /// safe no-op).
    fn delete_theme(&mut self, id: &str, state: &mut AutoCommit, path: &ObjId) {
        let catalog = doc::load_catalog(state, path).unwrap_or_default();
        let Some(idx) = catalog.iter().position(|s| s.id == id) else {
            return;
        };
        let Some(entry) = catalog.get(idx) else {
            return;
        };
        if entry.builtin || catalog.len() <= 1 {
            return;
        }
        let name = entry.name.clone();
        self.perform_action(state, &crate::actions::delete_custom_theme_action(id));
        // Move the selection onto a surviving theme: the row that slides
        // into `idx`, or the new last row when `idx` was the tail.
        let survivors: Vec<String> = catalog
            .iter()
            .filter(|s| s.id != id)
            .map(|s| s.id.clone())
            .collect();
        let next = survivors
            .get(idx.min(survivors.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        self.selected = next;
        self.status = format!("Deleted “{name}”");
        self.structure_dirty = true;
    }

    /// The last action an affordance dispatched through `Self::perform_action`.
    /// Rising state: it is the most recent dispatch, not cleared on read.
    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Apply `id` as the active OS theme: persist the full spec to the shared
    /// settings store (the on-disk contract the compositor + apps read), and —
    /// only when that write succeeds — dispatch the [`SetActiveTheme`] action
    /// that records the active id in the shared document.
    ///
    /// [`SetActiveTheme`](crate::actions::ThemesAction::SetActiveTheme)
    fn apply_theme(&mut self, id: &str, state: &mut AutoCommit) -> Result<(), String> {
        let catalog = doc::load_catalog(state, &automerge::ROOT).unwrap_or_default();
        if let Some(src) = catalog.iter().find(|s| s.id == id).cloned() {
            match store::save(&src) {
                Ok(()) => {
                    self.perform_action(state, &crate::actions::set_active_theme_action(&src.id));
                    self.status = format!("Applied “{}” as the system theme", src.name);
                    Ok(())
                }
                Err(e) => {
                    self.status = format!("Couldn't apply theme: {e}");
                    Err(e.to_string())
                }
            }
        } else {
            Err("Theme is no longer available".into())
        }
    }

    /// Reset the custom theme `id` back to the base preset's tokens (keeping its
    /// identity), dispatched as an upsert.
    fn reset_theme(&mut self, id: &str, state: &mut AutoCommit) {
        let catalog = doc::load_catalog(state, &automerge::ROOT).unwrap_or_default();
        if let Some(current) = catalog.iter().find(|s| s.id == id && !s.builtin).cloned() {
            self.perform_action(
                state,
                &crate::actions::reset_custom_theme_action(current.id),
            );
            self.status = "Reset to defaults".to_string();
            // Force a control reload next frame.
            self.bound_key = None;
        }
    }

    /// Seed the document on first run. Live seeds
    /// the presets-only default (the OS's shipped themes, never a
    /// fabricated "your theme"); a demo/`--shots` app seeds the demo catalog
    /// with its editable custom theme. The demo focuses its editable custom
    /// theme (so a capture shows real editing); the live install focuses the
    /// first preset.
    fn seed_document_if_needed(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Persisted libraries are read-only at startup, including a restored
        // version with no catalog roots. Explicit edits touch only their
        // addressed record inside the canonical action.
        if !self.selected.is_empty() {
            return;
        }
        let cat = if self.shot.demo {
            demo_catalog()
        } else {
            default_catalog()
        };
        // Built-ins are product data and custom demo content is inserted as
        // independently keyed native records (never a JSON catalog blob).
        if self.shot.demo {
            doc::save_catalog(state, path, &cat);
        }
        let sel = if self.shot.demo {
            cat.last().map(|s| s.id.clone()).unwrap_or_default()
        } else {
            cat.first().map(|s| s.id.clone()).unwrap_or_default()
        };
        self.selected.clone_from(&sel);
        self.edit_token = TokenId::Accent;
        // The applied OS theme: adopt whatever is already persisted in the
        // shared store, else default to the seeded selection.
        if ui::doc::read_str(state, path, doc::KEY_ACTIVE).is_none() {
            let active = store::load_active_id().unwrap_or_else(|| sel.clone());
            if self.actions.is_hosted() {
                self.perform_action(state, &crate::actions::set_active_theme_action(&active));
            } else {
                doc::put_str(state, path, doc::KEY_ACTIVE, &active);
            }
        }
    }

    /// Show the catalog and active theme the framework resource already holds.
    /// An empty projection does not wipe a catalog that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ThemesState) {
        if state.catalog.is_empty() && !self.catalog.is_empty() {
            return;
        }
        self.catalog.clone_from(&state.catalog);
        if self.selected.is_empty()
            && let Some(active) = &state.active
        {
            self.selected.clone_from(active);
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Themes>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// In-flight apply job label for hosted live extras.
    pub(crate) fn live_apply_job_label(&self) -> Option<String> {
        self.pending_apply
            .as_ref()
            .map(|(id, _)| format!("Applying theme {id}"))
    }

    /// Last export JSON preview when one is live (not document state).
    pub(crate) fn live_export_preview_resource(&self) -> Option<tilo_actions::Resource> {
        self.last_export.as_ref()?;
        Some(tilo_actions::Resource::new(
            "themes.export-preview",
            "themes.export-preview",
            "Last export",
        ))
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Steps 3-5 of [`Widget::sync`]: load the persisted catalog, resolve the
    /// selection + edit token, and two-way reconcile the editor controls
    /// against the selected theme. Returns `None` only if the catalog is
    /// unexpectedly empty, so the caller can skip the frame instead of
    /// indexing into it.
    fn reconcile_catalog(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> Option<(Vec<ThemeSpec>, usize, String, TokenId, bool)> {
        let mut catalog = doc::load_catalog(state, path).unwrap_or_else(default_catalog);

        let mut selected = self.selected.clone();
        if !catalog.iter().any(|s| s.id == selected) {
            selected = catalog.first().map(|s| s.id.clone()).unwrap_or_default();
            self.selected.clone_from(&selected);
        }
        let edit_token = self.edit_token;
        let idx = catalog.iter().position(|s| s.id == selected).unwrap_or(0);
        let editable = !catalog.get(idx)?.builtin;

        // Two-way reconcile the editor controls.
        let bound_key = (selected.clone(), edit_token);
        let changed = self.bound_key.as_ref() != Some(&bound_key)
            || self.catalog.iter().find(|s| s.id == selected) != catalog.get(idx);
        if changed {
            // LOAD: spec → controls; rebuild the colour picker.
            let spec = catalog.get(idx)?.clone();
            self.customizer.load_controls(&spec);
            self.customizer.load_picker(spec.token(edit_token));
            self.bound_key = Some(bound_key);
        } else if editable {
            // SAVE: controls → granular canonical actions.  Do not turn this
            // cached `ThemeSpec` into a whole-record write: another peer can be
            // editing a different token at exactly the same time.
            let current = catalog.get(idx)?.clone();
            {
                let r = self.customizer.radius() as f32;
                if (r - current.radius_norm).abs() > f32::EPSILON {
                    self.perform_action(
                        state,
                        &crate::actions::set_custom_theme_radius_action(&selected, r),
                    );
                }
            }
            {
                let s = self.customizer.spacing() as f32;
                if (s - current.spacing_norm).abs() > f32::EPSILON {
                    self.perform_action(
                        state,
                        &crate::actions::set_custom_theme_spacing_action(&selected, s),
                    );
                }
            }
            let f = self.customizer.font();
            if FONTS.contains(&f.as_str()) && f != current.font {
                self.perform_action(
                    state,
                    &crate::actions::set_custom_theme_font_action(&selected, f),
                );
            }
            // Inline sidebar rename: a non-empty edited name replaces the
            // theme's name (empty is ignored so a mid-edit clear can't wipe it).
            if let Some(n) = self.gallery.take_edited_name() {
                let n = n.trim();
                if !n.is_empty() && n != current.name {
                    self.perform_action(
                        state,
                        &crate::actions::set_custom_theme_name_action(&selected, n),
                    );
                }
            }
            let pc = self.customizer.picker_color();
            if pc != current.token(edit_token) {
                self.perform_action(
                    state,
                    &crate::actions::set_custom_theme_token_action(&selected, edit_token, pc),
                );
            }
            // Reflect this frame's action writes without a second whole-list
            // persistence pass. The next frame also re-resolves remote merges.
            catalog = doc::load_catalog(state, path).unwrap_or_else(default_catalog);
        }

        Some((catalog, idx, selected, edit_token, editable))
    }

    /// Steps 7-8 of [`Widget::sync`]: push the resolved selection into the
    /// panels and forward `sync` to the embedded controls that read their own
    /// local controls. No-op if `idx` no longer indexes
    /// `self.catalog` (defensive; catalog was just written from the same
    /// vector this index was resolved against).
    fn push_view_state(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        idx: usize,
        selected: &str,
        edit_token: TokenId,
        editable: bool,
    ) {
        let Some(sel_spec) = self.catalog.get(idx).cloned() else {
            return;
        };
        let active_id = ui::doc::read_str(state, path, doc::KEY_ACTIVE).unwrap_or_default();
        self.gallery
            .set_view(&self.catalog, selected, &active_id, editable);
        self.preview.set_spec(&sel_spec);
        self.customizer.set_view(sel_spec, edit_token, editable);

        // Rebind durable controls from the exact currently-exported resource
        // specs whenever the selected theme changes. Pointer, keyboard, a11y
        // and agent invocation therefore share one action envelope.
        if let Some(theme_resource) = crate::actions::provide_resources(state)
            .into_iter()
            .flat_map(|library| library.children)
            .find(|resource| resource.id.as_str() == format!("themes.theme:{selected}"))
        {
            let binding = |id| tilo_actions::ActionBinding::unit_for(&theme_resource, id).ok();
            if let Some(duplicate) = binding("duplicate") {
                self.customizer.bind_actions(
                    selected,
                    duplicate,
                    binding("reset"),
                    binding("delete"),
                );
            }
        }

        self.customizer.sync(state, path);
        self.gallery.sync(state, path);
        for b in &mut self.topbar_buttons {
            b.sync(state, path);
        }
        self.gallery_toggle.sync(state, path);
        // The toggle summons / dismisses the overlay gallery. Only meaningful
        // while the gallery is collapsible; a docked gallery has no toggle.
        if self.gallery_toggle_request.take() && self.show_toggle() {
            self.gallery_flap.toggle(SplitLayout::Overlay);
        }
        self.reconcile_gallery_toggle();
    }

    /// Lay out the top bar (part of [`Widget::build_layout`]): the gallery
    /// toggle in the leading slot (overlay mode only), a flexible spacer, and
    /// the right-aligned action buttons. Records `topbar_toggle_node`,
    /// `topbar_actions_node` and `topbar_node` and returns the top bar's node.
    ///
    /// The bar is [`TOPBAR_H`] tall at 1× and wraps rather than overflows:
    /// when the actions no longer fit beside the toggle (large text on a
    /// phone-width window) they drop to a second right-aligned line, and the
    /// actions themselves wrap once even a line of their own is too narrow,
    /// so no button is ever painted over the toggle or clipped at the edge.
    fn build_topbar_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let sp = &theme.tokens.spacing;
        let btn_nodes: Vec<NodeId> = self
            .topbar_buttons
            .iter_mut()
            .map(|b| b.build_layout(taffy, theme, node_map))
            .collect();
        let actions = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: taffy::FlexWrap::Wrap,
                justify_content: Some(taffy::JustifyContent::FlexEnd),
                align_items: Some(taffy::AlignItems::Center),
                gap: Size {
                    width: length(sp.s2),
                    height: length(sp.s2),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    height: Dimension::auto(),
                },
                flex_shrink: 1.0,
                ..Default::default()
            },
            &btn_nodes,
        );
        self.topbar_actions_node = Some(actions);

        let mut children = Vec::with_capacity(3);
        self.topbar_toggle_node = None;
        if self.show_toggle() {
            let toggle = self.gallery_toggle.build_layout(taffy, theme, node_map);
            self.topbar_toggle_node = Some(toggle);
            children.push(toggle);
        }
        // Pushes the actions to the trailing edge; basis 0 so it never forces
        // a wrap by itself.
        children.push(parent_node(
            taffy,
            Style {
                flex_grow: 1.0,
                flex_basis: length(0.0_f32),
                ..Default::default()
            },
            &[],
        ));
        children.push(actions);

        let topbar = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: taffy::FlexWrap::Wrap,
                justify_content: Some(taffy::JustifyContent::FlexEnd),
                align_items: Some(taffy::AlignItems::Center),
                align_content: Some(taffy::AlignContent::Center),
                gap: Size {
                    width: length(0.0_f32),
                    height: length(sp.s2),
                },
                padding: taffy::Rect {
                    left: length(sp.s4),
                    right: length(sp.s4),
                    top: length(sp.s2),
                    bottom: length(sp.s2),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(TOPBAR_H),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &children,
        );
        self.topbar_node = Some(topbar);
        topbar
    }

    fn set_narrow_preview(&mut self, preview: bool) {
        fn clear_focus(widget: &mut dyn Widget) {
            widget.set_focus(false);
            for child in widget.children_mut() {
                clear_focus(child);
            }
        }
        self.narrow_tabs.set_selected(usize::from(preview));
        if self.narrow_preview == preview {
            return;
        }
        if self.narrow_preview {
            self.preview_scroll = self.scroll;
        } else {
            self.customize_scroll = self.scroll;
        }
        self.narrow_preview = preview;
        self.scroll = if preview {
            self.preview_scroll
        } else {
            self.customize_scroll
        };
        if preview {
            clear_focus(&mut self.customizer);
        } else {
            clear_focus(&mut self.preview);
        }
        self.structure_dirty = true;
    }

    /// Lay out the gallery / preview / customizer panes and size each for
    /// the current layout class (part of [`Widget::build_layout`]). Records
    /// the three `*_node` fields and returns their nodes.
    fn build_panes_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        overlay: bool,
    ) -> (NodeId, NodeId, NodeId) {
        let gallery = self.gallery.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            gallery,
            Style {
                size: Size {
                    width: Dimension::length(GALLERY_W),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                ..taffy.style(gallery).cloned().unwrap_or_default()
            },
        );
        self.gallery_node = Some(gallery);

        let preview = if !overlay || self.narrow_preview {
            self.preview.build_layout(taffy, theme, node_map)
        } else {
            parent_node(taffy, Style::default(), &[])
        };
        if overlay {
            // Collapsed: the preview takes the full width at the height its
            // specimen owns, so every drawn section participates in scroll.
            // `flex_basis: auto` is required — the docked style pins it to 0
            // (grow-from-zero), which would collapse the fixed height here.
            let _ = taffy.set_style(
                preview,
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(
                            self.preview.narrow_intrinsic_height(theme.text_scale),
                        ),
                    },
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    flex_basis: Dimension::auto(),
                    ..taffy.style(preview).cloned().unwrap_or_default()
                },
            );
        }
        self.preview_node = (!overlay || self.narrow_preview).then_some(preview);

        // The customizer sizes its colour-chip grid to the width it will be
        // given: the docked column, or the full window when collapsed.
        let customizer_width = if overlay {
            self.build_width as f32
        } else {
            CUSTOMIZER_W
        };
        self.customizer.set_layout_width(customizer_width);
        let customizer = if !overlay || !self.narrow_preview {
            self.customizer.build_layout(taffy, theme, node_map)
        } else {
            parent_node(taffy, Style::default(), &[])
        };
        let cust_style = if overlay {
            // Collapsed: full width, intrinsic height, no shrink — a scrolling
            // stack member rather than a fixed side column.
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(self.customizer.intrinsic_height(theme.text_scale)),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                ..taffy.style(customizer).cloned().unwrap_or_default()
            }
        } else {
            Style {
                size: Size {
                    width: Dimension::length(CUSTOMIZER_W),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                ..taffy.style(customizer).cloned().unwrap_or_default()
            }
        };
        let _ = taffy.set_style(customizer, cust_style);
        self.customizer_node = (!overlay || !self.narrow_preview).then_some(customizer);

        (gallery, preview, customizer)
    }

    /// Assemble the content row (part of [`Widget::build_layout`]). Docked
    /// is `[gallery | preview | customizer]` in a row; collapsed (overlay) is
    /// `[preview / customizer]` stacked in a column the app scrolls
    /// vertically, with the gallery pulled out as an overlay flap.
    fn build_content_layout(
        taffy: &mut TaffyTree<()>,
        overlay: bool,
        gallery: NodeId,
        preview: NodeId,
        customizer: NodeId,
        narrow: Option<(NodeId, bool)>,
    ) -> NodeId {
        if overlay {
            parent_node(
                taffy,
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: length(0.0_f32),
                    overflow: taffy::Point {
                        x: taffy::Overflow::Hidden,
                        y: taffy::Overflow::Hidden,
                    },
                    ..Default::default()
                },
                &narrow.map_or_else(Vec::new, |(tabs, full)| {
                    vec![tabs, if full { preview } else { customizer }]
                }),
            )
        } else {
            parent_node(
                taffy,
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: length(0.0_f32),
                    ..Default::default()
                },
                &[gallery, preview, customizer],
            )
        }
    }

    /// Assemble the root (part of [`Widget::build_layout`]): the top bar over
    /// the content row, with the overlay gallery pulled out as a root
    /// sibling (fixed-width, absolutely positioned) so it isn't constrained
    /// by the content row — [`Self::apply_gallery_overlay`] slides it on/off
    /// the left edge.
    fn build_root_layout(
        taffy: &mut TaffyTree<()>,
        overlay: bool,
        topbar: NodeId,
        content: NodeId,
        gallery: NodeId,
    ) -> NodeId {
        let root_children: Vec<NodeId> = if overlay {
            let _ = taffy.set_style(
                gallery,
                Style {
                    size: Size {
                        width: Dimension::length(GALLERY_W),
                        height: Dimension::percent(1.0),
                    },
                    position: taffy::Position::Absolute,
                    flex_shrink: 0.0,
                    ..taffy.style(gallery).cloned().unwrap_or_default()
                },
            );
            vec![topbar, content, gallery]
        } else {
            vec![topbar, content]
        };
        parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &root_children,
        )
    }

    /// Resolve this frame's layout class and reconcile the flap (part of
    /// [`Widget::apply_layout`]). A breakpoint crossing drops a stale user
    /// toggle (back to the natural default) and rebuilds the taffy tree next
    /// frame so the gallery moves between the docked column and the overlay
    /// subtree.
    fn reconcile_layout_class(&mut self) -> SplitLayout {
        let layout = self.layout_class();
        self.gallery_flap.reconcile(layout);
        let new_build_width = self.rect.width();
        let class_changed = SplitLayout::from_width(self.build_width, BREAKPOINT) != layout
            && self.rect.width() > 0.0;
        if class_changed {
            self.structure_dirty = true;
        }
        if self.rect.width() > 0.0 {
            self.build_width = new_build_width;
        }
        self.last_layout = Some(layout);
        layout
    }

    /// Place the top bar's toggle and action buttons and record the bar's
    /// measured rect (part of [`Widget::apply_layout`]).
    fn apply_topbar_layout(&mut self, taffy: &TaffyTree<()>, origin: Point) {
        let Some(tb) = self.topbar_node else { return };
        let Some(actions) = self.topbar_actions_node else {
            return;
        };
        let Ok(tbl) = taffy.layout(tb) else { return };
        let tb_o = Point::new(
            origin.x + f64::from(tbl.location.x),
            origin.y + f64::from(tbl.location.y),
        );
        self.topbar_rect = Rect::new(
            tb_o.x,
            tb_o.y,
            tb_o.x + f64::from(tbl.size.width),
            tb_o.y + f64::from(tbl.size.height),
        );
        if let Some(n) = self.topbar_toggle_node {
            self.gallery_toggle.apply_layout(taffy, n, tb_o);
        }
        let Ok(al) = taffy.layout(actions) else {
            return;
        };
        let a_o = Point::new(
            tb_o.x + f64::from(al.location.x),
            tb_o.y + f64::from(al.location.y),
        );
        if let Ok(children) = taffy.children(actions) {
            for (b, c) in self.topbar_buttons.iter_mut().zip(children.iter()) {
                b.apply_layout(taffy, *c, a_o);
            }
        }
    }

    /// Place the preview + customizer (and, when docked, the gallery) inside
    /// the content row (part of [`Widget::apply_layout`]).
    fn apply_content_layout(&mut self, taffy: &TaffyTree<()>, origin: Point, layout: SplitLayout) {
        let Some(content) = self.content_node else {
            return;
        };
        let Ok(cl) = taffy.layout(content) else {
            return;
        };
        let c_o = Point::new(
            origin.x + f64::from(cl.location.x),
            origin.y + f64::from(cl.location.y),
        );
        if layout == SplitLayout::Overlay {
            self.apply_narrow_stack_layout(taffy, c_o, cl.size.width, cl.size.height);
        } else {
            if let Some(n) = self.gallery_node {
                self.gallery.apply_layout(taffy, n, c_o);
            }
            if let Some(n) = self.preview_node {
                self.preview.apply_layout(taffy, n, c_o);
            }
            if let Some(n) = self.customizer_node {
                self.customizer.apply_layout(taffy, n, c_o);
            }
        }
    }

    /// Collapsed (overlay) layout: preview stacked above customizer, both
    /// offset up by the scroll so taller-than-viewport content is fully
    /// reachable (part of [`Widget::apply_layout`], via
    /// [`Self::apply_content_layout`]). `content_rect` is the clip/viewport
    /// the wheel and scrollbar work against.
    fn apply_narrow_stack_layout(
        &mut self,
        taffy: &TaffyTree<()>,
        c_o: Point,
        width: f32,
        height: f32,
    ) {
        let vp = Rect::new(
            c_o.x,
            c_o.y,
            c_o.x + f64::from(width),
            c_o.y + f64::from(height),
        );
        self.content_rect = vp;
        self.scroll_viewport_h = vp.height();
        let ph = self
            .preview_node
            .and_then(|n| taffy.layout(n).ok())
            .map_or(0.0, |l| f64::from(l.size.height));
        let ch = self
            .customizer_node
            .and_then(|n| taffy.layout(n).ok())
            .map_or(0.0, |l| f64::from(l.size.height));
        // The selector's top margin is part of the scrollable extent, so take
        // its bottom edge rather than its bare height.
        let tabs_height = self
            .narrow_tabs_node
            .and_then(|n| taffy.layout(n).ok())
            .map_or(0.0, |l| f64::from(l.location.y + l.size.height));
        self.scroll_content_h = ph + ch + tabs_height;
        if self.shot.scroll {
            self.scroll = 260.0;
        }
        let max_scroll = (self.scroll_content_h - self.scroll_viewport_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let scrolled = Point::new(c_o.x, c_o.y - self.scroll);
        if let Some(n) = self.narrow_tabs_node {
            self.narrow_tabs.apply_layout(taffy, n, scrolled);
        }
        if let Some(n) = self.preview_node {
            self.preview.apply_layout(taffy, n, scrolled);
        }
        if let Some(n) = self.customizer_node {
            self.customizer.apply_layout(taffy, n, scrolled);
        }
    }

    /// Slide the overlay gallery flap in from the left edge by the eased
    /// reveal (part of [`Widget::apply_layout`]; fully off-screen at reveal
    /// 0). Its origin sits below the top bar so the hamburger stays
    /// reachable.
    fn apply_gallery_overlay(&mut self, taffy: &TaffyTree<()>, layout: SplitLayout) {
        if layout != SplitLayout::Overlay {
            self.gallery_overlay_rect = Rect::ZERO;
            return;
        }
        let slide = self.gallery_flap.slide();
        let gw = f64::from(GALLERY_W);
        let x0 = self.rect.x0 - gw * (1.0 - slide);
        let y0 = self.topbar_band().y1;
        self.gallery_overlay_rect = Rect::new(x0, y0, x0 + gw, self.rect.y1);
        if let Some(n) = self.gallery_node {
            self.gallery.apply_layout(
                taffy,
                n,
                Point::new(self.gallery_overlay_rect.x0, self.gallery_overlay_rect.y0),
            );
        }
    }

    /// Overlay-only global shortcuts checked before any child widget sees
    /// the event (part of [`Widget::handle_event`]): the wheel scrolls the
    /// collapsed stack, Esc dismisses the open flap, and a scrim tap
    /// dismisses it. (The hamburger is a real [`Button`] in the top bar and
    /// takes its own click.) Returns `true` if the event was consumed (the
    /// caller should stop dispatching).
    fn handle_overlay_shortcuts(
        &mut self,
        event: &WindowEvent,
        overlay: bool,
        gallery_open: bool,
        handled: &mut bool,
    ) -> bool {
        // Collapsed layout: the wheel scrolls the preview+customizer stack
        // (unless the overlay gallery is open, which owns input while up).
        if overlay
            && !gallery_open
            && let WindowEvent::MouseWheel { delta, .. } = event
        {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => -*y * 60.0,
                MouseScrollDelta::PixelDelta(pp) => -pp.y as f32,
            };
            let max = (self.scroll_content_h - self.scroll_viewport_h).max(0.0);
            self.scroll = (self.scroll + f64::from(dy)).clamp(0.0, max);
            *handled = true;
            return true;
        }

        // Esc dismisses an open overlay gallery before anything else sees it.
        if overlay
            && gallery_open
            && let WindowEvent::KeyboardInput {
                event: key_event, ..
            } = event
            && key_event.state == ElementState::Pressed
            && key_event.logical_key == Key::Named(NamedKey::Escape)
        {
            self.gallery_flap.close();
            *handled = true;
            return true;
        }

        // Tapping the scrim (content, outside the flap) dismisses an open
        // gallery. The toggle Button is outside the flap too; its click is
        // its own, so a press on it is left for the button to handle.
        if overlay
            && gallery_open
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            && !self.gallery_overlay_rect.contains(self.last_cursor)
            && !self.gallery_toggle.get_rect().contains(self.last_cursor)
        {
            self.gallery_flap.close();
            *handled = true;
            return true;
        }
        false
    }

    /// Forward the event into each top-bar button (part of
    /// [`Widget::handle_event`]).
    fn handle_topbar_buttons(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(n) = self.topbar_toggle_node {
            self.gallery_toggle
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        for (b, c) in self.topbar_buttons.iter_mut().zip(
            self.topbar_actions_node
                .and_then(|n| ctx.taffy.children(n).ok())
                .unwrap_or_default()
                .iter(),
        ) {
            b.handle_event(event, &mut ctx.reborrow(ctx.path, *c));
            if *ctx.handled {
                return;
            }
        }
    }

    /// Forward the event into the gallery when it's docked, or when its
    /// overlay flap is open (part of [`Widget::handle_event`]). Returns
    /// `true` if the caller should stop dispatching — either the gallery
    /// consumed the event, or a pointer press inside the open flap is
    /// swallowed so it can't fall through to the content beneath.
    fn handle_gallery_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        overlay: bool,
        gallery_open: bool,
    ) -> bool {
        // The gallery only receives events when docked, or when its overlay
        // flap is open. While open, any pointer press landing inside the flap
        // is consumed so it can't fall through to the content beneath.
        if overlay && !gallery_open {
            return false;
        }
        let Some(n) = self.gallery_node else {
            return false;
        };
        self.gallery
            .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        if *ctx.handled {
            // Selecting a theme from the open flap dismisses it so the
            // narrow window returns to the preview + customizer (the
            // master→detail return).
            if overlay {
                self.gallery_flap.close();
            }
            return true;
        }
        if overlay
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            && self.gallery_overlay_rect.contains(self.last_cursor)
        {
            *ctx.handled = true;
            return true;
        }
        false
    }
}

impl Widget for ThemesApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["themes.apply", "themes.export", "themes.new"]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested
            && self
                .pending_apply
                .as_ref()
                .is_some_and(|(_, pending)| *pending == invocation)
        {
            self.pending_apply = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before the theme store was changed".into()),
            });
        }
        if self
            .completed_apply
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_apply.take();
        }
        None
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if let Some(preview) = self.narrow_view_request.take() {
            self.narrow_view_chosen = true;
            self.set_narrow_preview(preview);
        }
        // 1. Seed the document on first run.
        self.seed_document_if_needed(state, path);

        if std::mem::take(&mut self.pending_new) {
            self.new_theme(state, path);
        }

        if let Some((id, invocation)) = self.pending_apply.take() {
            self.completed_apply = Some(match self.apply_theme(&id, state) {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "themes.apply_failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            });
        }

        // `--shots` scenario: force a specific selection for the capture.
        if let Some(id) = self.shot.select.take() {
            self.selected = id;
        }

        // 2. Drain local presentation queues. Only the command handlers for
        // committed mutations dispatch an action into `state`.
        let mut commands: Vec<String> = self.top_commands.borrow_mut().drain(..).collect();
        while let Some(command) = self.gallery.take_command() {
            commands.push(command);
        }
        if let Some(token) = self.customizer.take_edit_token() {
            self.edit_token = token;
        }
        for command in commands {
            self.process_command(&command, state, path);
        }

        // 3-5. Load the persisted catalog, resolve the selection + edit
        // token, and two-way reconcile the editor controls. `None` only if
        // the catalog is unexpectedly empty (never happens on the live or
        // demo seed path, but `sync` must not panic on any document a peer
        // could hand us) — skip this frame's sync rather than panic.
        let Some((catalog, idx, selected, edit_token, editable)) =
            self.reconcile_catalog(state, path)
        else {
            return;
        };

        // A live preset is primarily a specimen; an editable theme starts at
        // its controls. Once a view is chosen, preserve that local preference.
        if !self.narrow_view_chosen {
            self.set_narrow_preview(!editable);
        }

        // 6. Cache. Durable custom mutations above are dispatched as granular
        // ThemesAction values; never write this rendered snapshot wholesale.
        self.catalog = catalog;

        // 7-8. Push the resolved selection into the panels and forward sync
        // to the embedded controls' local state.
        self.push_view_state(state, path, idx, &selected, edit_token, editable);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let topbar = self.build_topbar_layout(taffy, theme, node_map);

        // Panes. The gallery is the primary sidebar: docked in the content row
        // on a wide window, or pulled out into a fixed-width overlay subtree on
        // a narrow one so the preview + customizer can take the full width.
        let overlay = SplitLayout::from_width(self.build_width, BREAKPOINT) == SplitLayout::Overlay;
        let (gallery, preview, customizer) =
            self.build_panes_layout(taffy, theme, node_map, overlay);

        let narrow = if overlay {
            let tabs = self.narrow_tabs.build_layout(taffy, theme, node_map);
            // The view selector keeps the narrow content gutter on the left
            // and right, and the same gutter above it so it does not sit flush
            // on the top bar's hairline. Below it the customizer / preview
            // supply their own top inset.
            let _ = taffy.set_style(
                tabs,
                Style {
                    margin: taffy::Rect {
                        left: length(NARROW_CONTENT_GUTTER),
                        right: length(NARROW_CONTENT_GUTTER),
                        top: length(NARROW_CONTENT_GUTTER),
                        bottom: length(0.0_f32),
                    },
                    ..taffy.style(tabs).cloned().unwrap_or_default()
                },
            );
            self.narrow_tabs_node = Some(tabs);
            Some((tabs, self.narrow_preview))
        } else {
            self.narrow_tabs_node = None;
            None
        };
        let content =
            Self::build_content_layout(taffy, overlay, gallery, preview, customizer, narrow);
        self.content_node = Some(content);

        let root = Self::build_root_layout(taffy, overlay, topbar, content, gallery);
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else { return };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let o = Point::new(self.rect.x0, self.rect.y0);

        let layout = self.reconcile_layout_class();
        self.apply_topbar_layout(taffy, o);
        self.apply_content_layout(taffy, o, layout);
        self.apply_gallery_overlay(taffy, layout);

        // `--shots` menu scenario: open the sidebar actions menu now that the
        // rows have real rects to anchor it to.
        if self.shot.menu {
            self.shot.menu = false;
            self.gallery.open_menu_for_shot();
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        // Advance the reveal animation (frame-rate-independent; `needs_redraw`
        // keeps frames flowing while it settles).
        let layout = self.layout_class();
        let now = ui::time::now();
        let dt = (now - self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.gallery_flap.tick(layout, dt);

        draw::fill_rect(scene, t, self.rect, theme.bg_app);

        // Content panes. In overlay mode the gallery floats above the scrim, so
        // it's drawn after the scrim below.
        if layout == SplitLayout::Overlay {
            // Clip the scrolling preview+customizer stack to its viewport so
            // scrolled-out content can't bleed over the top bar; then draw the
            // scroll indicator over the clipped content.
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &self.content_rect,
            );
            self.narrow_tabs.draw(scene, t, tr, theme, p);
            if self.narrow_preview {
                self.preview.draw(scene, t, tr, theme, p);
            } else {
                self.customizer.draw(scene, t, tr, theme, p);
            }
            scene.pop_layer();
            self.draw_scroll_indicator(scene, t, theme);
        } else {
            self.preview.draw(scene, t, tr, theme, p);
            self.customizer.draw(scene, t, tr, theme, p);
            self.gallery.draw(scene, t, tr, theme, p);
        }

        // Overlay: a dimming scrim fades in with the reveal, then the gallery
        // flap slides over it. The scrim covers the content below the top bar.
        if layout == SplitLayout::Overlay {
            let reveal = self.gallery_flap.reveal();
            if reveal > 0.001 {
                let scrim_rect = Rect::new(
                    self.rect.x0,
                    self.topbar_band().y1,
                    self.rect.x1,
                    self.rect.y1,
                );
                let scrim = theme
                    .tokens
                    .colors
                    .surfaces
                    .overlay
                    .with_alpha_factor((reveal * 0.6) as f32);
                scene.fill(Fill::NonZero, t, scrim, None, &scrim_rect);
                self.gallery.draw(scene, t, tr, theme, p);
            }
        }

        // Top bar over the panes, at its measured height.
        let tb = self.topbar_band();
        draw::fill_rect(scene, t, tb, theme.bg_primary);
        // Bar role: one hairline derived from the bar's own fill (HIG §9.5),
        // half a pixel up so the rule occupies the bar's last pixel row.
        draw::line(
            scene,
            t,
            Point::new(tb.x0, tb.y1 - 0.5),
            Point::new(tb.x1, tb.y1 - 0.5),
            ui::surface::edge_on(theme.bg_primary),
            1.0,
        );

        // The gallery toggle (overlay only) is a laid-out Button in the bar's
        // leading slot. The OS owns the window title/app-name/logo, so the
        // app draws no wordmark.
        if self.topbar_toggle_node.is_some() {
            self.gallery_toggle.draw(scene, t, tr, theme, p);
        }

        for b in &mut self.topbar_buttons {
            b.draw(scene, t, tr, theme, p);
        }

        // Transient status toast (centre of the top bar).
        if !self.status.is_empty() {
            let (w, _) = tr.measure(&self.status, 12.0, None);
            let pad = 14.0;
            let tw = f64::from(w) + pad * 2.0;
            let cx = tb.center().x - tw / 2.0;
            let pill = Rect::new(cx, tb.y0 + 14.0, cx + tw, tb.y0 + 40.0);
            let fill = theme.tokens.colors.semantic.success;
            draw::fill_rrect(scene, t, pill, 13.0, fill);
            draw::text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                &self.status,
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                // Ink measured against the pill, not a literal white: a light
                // preset's success green needs dark ink to stay legible.
                theme.on_surface(fill).ink,
                pill,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Pointer-capture priority: a live picker / slider drag in the
        // customizer must keep receiving events wherever the cursor goes.
        if self.customizer.wants_pointer_capture() {
            if let Some(n) = self.customizer_node {
                self.customizer
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return;
        }

        // Track the cursor for the toggle + scrim hit tests.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        let overlay = self.layout_class() == SplitLayout::Overlay;
        let gallery_open = overlay && self.gallery_flap.is_open(SplitLayout::Overlay);

        if overlay
            && !gallery_open
            && self.narrow_preview
            && let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && key.logical_key == Key::Named(NamedKey::Escape)
        {
            self.narrow_view_chosen = true;
            self.set_narrow_preview(false);
            self.narrow_tabs.set_focus(true);
            *ctx.focus_target = self.narrow_tabs_node;
            *ctx.handled = true;
            return;
        }
        if overlay
            && !gallery_open
            && let Some(node) = self.narrow_tabs_node
        {
            self.narrow_tabs
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                return;
            }
        }

        if self.handle_overlay_shortcuts(event, overlay, gallery_open, ctx.handled) {
            return;
        }

        self.handle_topbar_buttons(event, ctx);
        if *ctx.handled {
            return;
        }

        if self.handle_gallery_event(event, ctx, overlay, gallery_open) {
            return;
        }

        if let Some(n) = self.customizer_node {
            self.customizer
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return;
            }
        }
        if let Some(n) = self.preview_node {
            self.preview
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }

    fn name(&self) -> &'static str {
        "ThemesApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.topbar_toggle_node.is_some() {
            children.push(&self.gallery_toggle);
        }
        children.extend(self.topbar_buttons.iter().map(|b| b as &dyn Widget));
        if self.layout_class() == SplitLayout::Overlay {
            if self.gallery_flap.is_open(SplitLayout::Overlay) {
                children.push(&self.gallery);
            } else {
                children.push(&self.narrow_tabs);
                if self.narrow_preview {
                    children.push(&self.preview);
                } else {
                    children.push(&self.customizer);
                }
            }
        } else {
            children.extend([
                &self.gallery as &dyn Widget,
                &self.preview,
                &self.customizer,
            ]);
        }
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let overlay = self.layout_class() == SplitLayout::Overlay;
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if self.topbar_toggle_node.is_some() {
            children.push(&mut self.gallery_toggle);
        }
        children.extend(self.topbar_buttons.iter_mut().map(|b| b as &mut dyn Widget));
        if overlay {
            if self.gallery_flap.is_open(SplitLayout::Overlay) {
                children.push(&mut self.gallery);
            } else {
                children.push(&mut self.narrow_tabs);
                if self.narrow_preview {
                    children.push(&mut self.preview);
                } else {
                    children.push(&mut self.customizer);
                }
            }
        } else {
            children.extend([
                &mut self.gallery as &mut dyn Widget,
                &mut self.preview,
                &mut self.customizer,
            ]);
        }
        children
    }

    fn needs_redraw(&self) -> bool {
        // A static theme preview used to keep the compositor repainting at
        // vsync forever. Only request work while a child changed, a gallery
        // reveal is still easing, or the composed structure needs rebuilding.
        self.structure_dirty
            || self.gallery_flap.is_animating(self.layout_class())
            || self.gallery.needs_redraw()
            || self.preview.needs_redraw()
            || self.customizer.needs_redraw()
            || self.narrow_tabs.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.gallery_flap.is_animating(self.layout_class())
            || self.gallery.needs_animation_frame()
            || self.preview.needs_animation_frame()
            || self.customizer.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.gallery.mark_drawn();
        self.narrow_tabs.mark_drawn();
        self.preview.mark_drawn();
        self.customizer.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        let d = self.structure_dirty
            | self.gallery.take_structure_dirty()
            | self.customizer.take_structure_dirty();
        self.structure_dirty = false;
        d
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl ui::resources::PublishesResources for ThemesApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the library door — keep only live apply jobs and export previews.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "themes.library" {
            return None;
        }
        if let Some(found) = self.publish_resource(doc, id) {
            return Some(found);
        }
        if self.is_hosted() {
            return crate::resource::live_extras(self)
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }
        None
    }
}

impl ui::history_mode::capability::DocumentHistory for ThemesApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for ThemesApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its Themes action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ThemesApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(false);
        }
        if type_id == "themes.new" {
            self.pending_new = true;
            return Ok(true);
        }
        if type_id == "themes.apply" {
            return Err("theme application requires the lifecycle-capable operation host".into());
        }
        if type_id != "themes.export" {
            return Ok(false);
        }
        let id = serde_json::from_slice::<Option<String>>(payload)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| self.selected.clone());
        let Some(theme) = doc::load_catalog(doc, &automerge::ROOT)
            .unwrap_or_default()
            .into_iter()
            .find(|theme| theme.id == id)
        else {
            return Err("Theme is no longer available".into());
        };
        self.last_export = Some(theme.to_json());
        self.status = format!("Exported “{}” as JSON", theme.name);
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect || type_id != "themes.apply" {
            return Ok(None);
        }
        if self.pending_apply.is_some() {
            return Err("another theme is still being applied".into());
        }
        let id = serde_json::from_slice::<Option<String>>(payload)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| self.selected.clone());
        let exists = doc::load_catalog(doc, &automerge::ROOT)
            .unwrap_or_default()
            .iter()
            .any(|theme| theme.id == id);
        if !exists {
            return Err("Theme is no longer available".into());
        }
        self.pending_apply = Some((id, invocation));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;
    use tilo_test_env::ScopedEnv;
    use ui::headless::Framed;

    /// Point the shared theme store at a throwaway config dir for as long as
    /// the returned scope lives, so Apply never writes the developer's real
    /// `~/.config/tilo/settings.json`.
    fn isolate_config() -> ScopedEnv {
        let dir = std::env::temp_dir().join(format!("tilo-themes-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut env = ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &dir);
        env
    }

    /// A freshly-seeded **live** app + its CRDT state (first-run seed done),
    /// with the config isolation scope that must outlive both.
    /// Live = presets only, no fabricated custom theme.
    fn seeded() -> (ThemesApp, ui::State, ScopedEnv) {
        let env = isolate_config();
        let mut app = ThemesApp::new();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        (app, st, env)
    }

    /// A freshly-seeded **demo** app (presets + the editable custom "sunset"),
    /// for the tests that exercise editing / deleting a custom theme.
    fn seeded_demo() -> (ThemesApp, ui::State, ScopedEnv) {
        let env = isolate_config();
        let mut app = ThemesApp::new_demo();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        (app, st, env)
    }

    fn catalog(st: &ui::State) -> Vec<ThemeSpec> {
        doc::load_catalog(&st.doc, &st.root).unwrap()
    }
    fn selected(app: &ThemesApp) -> String {
        app.selected.clone()
    }
    fn run(app: &mut ThemesApp, st: &mut ui::State, cmd: &str) {
        app.process_command(cmd, &mut st.doc, &st.root);
        app.sync(&mut st.doc, &st.root);
    }

    /// Mount `app` over `st` in the shared frame harness at `width`x`height`.
    fn framed((app, st): (ThemesApp, ui::State), width: u32, height: u32) -> Framed<ThemesApp> {
        Framed::new(st, app, (width, height), Theme::dark())
    }

    fn layout_at(app: &mut ThemesApp, width: f32, height: f32) -> (TaffyTree<()>, NodeId) {
        app.build_width = f64::from(width);
        let mut tree = TaffyTree::new();
        let mut map = HashMap::new();
        let node = app.build_layout(&mut tree, &Theme::dark(), &mut map);
        let mut text = TextRenderer::new();
        tree.compute_layout_with_measure(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            |known, available, id, _, _| {
                map.get(&id).map_or(Size::ZERO, |ptr| unsafe {
                    (**ptr).measure(known, available, &mut text)
                })
            },
        )
        .unwrap();
        app.apply_layout(&tree, node, Point::ZERO);
        (tree, node)
    }

    fn layout_narrow(app: &mut ThemesApp, width: f32) -> (TaffyTree<()>, NodeId) {
        layout_at(app, width, 900.0)
    }

    fn dispatch(
        app: &mut Framed<ThemesApp>,
        event: &WindowEvent,
        focus: &mut Option<NodeId>,
    ) -> bool {
        let outcome = Framed::harness_mut(app)
            .dispatch_to_root_outcome(event, &winit::event::Modifiers::default());
        if outcome.focus_target.is_some() {
            *focus = outcome.focus_target;
        }
        outcome.handled
    }

    fn narrow_view_state(app: &ThemesApp) -> (String, [bool; 2]) {
        let node = app
            .narrow_tabs
            .accessibility()
            .expect("narrow view selector has accessibility semantics");
        assert_eq!(node.children.len(), 2);
        (
            node.value.expect("selected view label"),
            [
                node.children[0]
                    .state
                    .checked
                    .expect("Customize checked state"),
                node.children[1]
                    .state
                    .checked
                    .expect("Full preview checked state"),
            ],
        )
    }

    fn contains_rect(outer: Rect, inner: Rect) -> bool {
        outer.x0 <= inner.x0 && outer.y0 <= inner.y0 && outer.x1 >= inner.x1 && outer.y1 >= inner.y1
    }

    fn route_narrow_view_invocation(app: &mut ThemesApp, state: &mut ui::State) {
        let invocation = ui::widget::controls::take_control_invocation(&mut app.narrow_tabs)
            .expect("view selection emitted its canonical invocation");
        let index = invocation.arguments["index"]
            .as_u64()
            .expect("view index is an integer");
        let payload = serde_json::to_vec(&serde_json::json!({
            "control": "themes.view",
            "index": index,
        }))
        .unwrap();
        assert!(
            app.narrow_tabs
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        "tilo.ui.segmented.select",
                        &payload,
                    ),
                    &mut state.doc,
                    &state.root,
                )
                .unwrap()
                .is_some()
        );
        app.sync(&mut state.doc, &state.root);
    }

    /// Deliver one event at `node` with default modifiers; returns whether it was handled.
    fn send(
        app: &mut ThemesApp,
        event: &WindowEvent,
        state: &mut ui::State,
        (node, tree): (NodeId, &TaffyTree<()>),
        focus: &mut Option<NodeId>,
    ) -> bool {
        let mut handled = false;
        app.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: focus,
                node_id: node,
                taffy: tree,
            },
        );
        handled
    }

    fn key_press(logical_key: Key) -> WindowEvent {
        let mut key: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        key.logical_key = logical_key;
        key.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        key.location = winit::keyboard::KeyLocation::Standard;
        key.state = ElementState::Pressed;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key,
            is_synthetic: false,
        }
    }

    /// Narrow layout: editor first, full specimen seeds, and scrolling to the specimen tail.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the gutter is an exact token offset, not a computed value"
    )]
    fn narrow_layout_shows_the_editor_and_a_scrollable_full_preview() {
        // Case `narrow_editor_is_visible_before_full_preview`.
        {
            let (app, st, _env) = seeded_demo();
            let app = framed((app, st), 480, 900);
            assert_eq!(
                narrow_view_state(&app),
                ("Customize".to_owned(), [true, false])
            );
            assert_eq!(
                app.narrow_tabs.get_rect().x0,
                16.0,
                "view selector follows the narrow content gutter"
            );
            assert!(
                app.customizer.get_rect().y0 < 160.0,
                "{:?}",
                app.customizer.get_rect()
            );
            assert!(
                app.children()
                    .iter()
                    .all(|child| child.name() != "PreviewPane")
            );
        }

        // Case `live_preset_starts_with_its_full_specimen`.
        {
            let (app, st, _env) = seeded();
            let app = framed((app, st), 480, 900);
            assert!(app.narrow_preview);
            assert_eq!(
                narrow_view_state(&app),
                ("Full preview".to_owned(), [false, true])
            );
            assert!(app.preview_node.is_some());
            assert!(app.customizer_node.is_none());
        }

        // Case `custom_preview_capture_seed_selects_full_preview`.
        {
            let _env = isolate_config();
            let mut app = ThemesApp::new_demo();
            app.seed_shot(480.0, "preview");
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            let app = framed((app, state), 480, 900);
            assert!(app.narrow_preview);
            assert_eq!(
                narrow_view_state(&app),
                ("Full preview".to_owned(), [false, true])
            );
        }

        // Case `narrow_full_preview_contains_and_can_scroll_to_the_specimen_tail`.
        {
            let (app, st, _env) = seeded();
            let mut app = framed((app, st), 480, 480);
            let card = app.preview.specimen_card_rect();
            let tail = app.preview.specimen_tail_rect();
            let preview = app.preview.get_rect();
            assert!(contains_rect(preview, card));
            assert!(contains_rect(card, tail));
            assert!(
                app.scroll_content_h > app.scroll_viewport_h,
                "short preview must expose a scroll extent"
            );

            let event = WindowEvent::MouseWheel {
                device_id: unsafe { std::mem::zeroed() },
                delta: MouseScrollDelta::LineDelta(0.0, -100.0),
                phase: winit::event::TouchPhase::Moved,
            };
            let handled = dispatch(&mut app, &event, &mut None);
            Framed::harness_mut(&mut app)
                .relayout_at((480, 480))
                .expect("layout settles");
            let max_scroll = app.scroll_content_h - app.scroll_viewport_h;
            assert!(handled);
            assert!((app.scroll - max_scroll).abs() < f64::EPSILON);
            assert!(
                app.preview.specimen_tail_rect().y1 <= app.content_rect.y1,
                "maximum scroll must reveal the final chart"
            );
        }
    }

    #[test]
    fn narrow_view_pointer_keyboard_resize_and_escape_preserve_edits() {
        let (mut app, mut state, _env) = seeded_demo();
        app.customizer.load_picker([1, 2, 3]);
        app.sync(&mut state.doc, &state.root);
        let (mut tree, mut node) = layout_narrow(&mut app, 480.0);
        state.doc.commit();
        let heads = state.doc.get_heads();
        assert_eq!(
            narrow_view_state(&app),
            ("Customize".to_owned(), [true, false])
        );

        // Pointer selection enters through the segmented control's typed view
        // operation, then reconciliation updates both the visible and AT state.
        let tabs = app.narrow_tabs.get_rect();
        let cursor = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(tabs.x1 - 10.0, tabs.center().y),
        };
        let click = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        for event in [&cursor, &click] {
            send(&mut app, event, &mut state, (node, &tree), &mut None);
        }
        route_narrow_view_invocation(&mut app, &mut state);
        assert!(app.narrow_preview);
        assert_eq!(
            narrow_view_state(&app),
            ("Full preview".to_owned(), [false, true])
        );
        assert_eq!(state.doc.get_heads(), heads);
        layout_narrow(&mut app, 480.0);
        assert!(app.customizer_node.is_none());
        assert!(app.preview_node.is_some());
        // Crossing the breakpoint must not reset the local selected view or edit.
        layout_narrow(&mut app, 1200.0);
        assert!(app.narrow_preview);
        (tree, node) = layout_narrow(&mut app, 480.0);
        let event = key_press(Key::Named(NamedKey::Escape));
        let mut focus = None;
        let handled = send(&mut app, &event, &mut state, (node, &tree), &mut focus);
        assert!(handled && !app.narrow_preview);
        assert_eq!(focus, app.narrow_tabs_node);
        assert!(app.narrow_tabs.is_focused());
        assert_eq!(
            narrow_view_state(&app),
            ("Customize".to_owned(), [true, false])
        );

        // Focus stays on the compound selector, so ArrowRight can re-open
        // Full preview without exposing or focusing the hidden customizer.
        let event = key_press(Key::Named(NamedKey::ArrowRight));
        let handled = send(&mut app, &event, &mut state, (node, &tree), &mut focus);
        assert!(handled);
        route_narrow_view_invocation(&mut app, &mut state);
        assert!(app.narrow_preview);
        assert_eq!(
            narrow_view_state(&app),
            ("Full preview".to_owned(), [false, true])
        );
        app.sync(&mut state.doc, &state.root);
        assert_eq!(
            catalog(&state)
                .iter()
                .find(|t| t.id == "sunset")
                .unwrap()
                .accent,
            [1, 2, 3]
        );
        assert_eq!(state.doc.get_heads(), heads);
    }

    /// Library commands and the live preset-only seed.
    #[test]
    fn library_commands_duplicate_delete_rename_and_seed_presets() {
        // Case `duplicate_command_adds_editable_copy_and_selects_it`.
        {
            let (mut app, mut st, _env) = seeded();
            let before = catalog(&st).len();
            run(&mut app, &mut st, "duplicate:tilo-blue");
            let cat = catalog(&st);
            assert_eq!(cat.len(), before + 1, "one theme added");
            let copy = cat
                .iter()
                .find(|s| s.name == "Tilo Blue copy")
                .expect("named copy exists");
            assert!(!copy.builtin, "the copy is editable");
            assert_eq!(selected(&app), copy.id, "the copy becomes the selection");
        }

        // Case `live_seed_is_presets_only_and_selects_a_preset`.
        {
            // Honest live default: only built-in presets, and the focused theme is
            // one of them — no fabricated "your custom theme".
            let (app, st, _env) = seeded();
            let cat = catalog(&st);
            assert!(
                cat.iter().all(|s| s.builtin),
                "live catalog is presets only"
            );
            let sel = selected(&app);
            assert!(cat.iter().find(|s| s.id == sel).is_some_and(|s| s.builtin));
            assert!(
                !cat.iter().any(|s| s.id == "sunset"),
                "no seeded custom theme on the live path"
            );
        }

        // Case `delete_command_removes_a_custom_theme_and_reselects`.
        {
            let (mut app, mut st, _env) = seeded_demo();
            assert!(
                catalog(&st).iter().any(|s| s.id == "sunset"),
                "seeded custom present"
            );
            run(&mut app, &mut st, "delete:sunset");
            assert!(
                !catalog(&st).iter().any(|s| s.id == "sunset"),
                "custom theme removed"
            );
            assert!(
                catalog(&st).iter().any(|s| s.id == selected(&app)),
                "selection stays valid"
            );
        }

        // Case `delete_command_refuses_builtin_presets`.
        {
            let (mut app, mut st, _env) = seeded();
            let before = catalog(&st).len();
            run(&mut app, &mut st, "delete:tilo-blue");
            assert_eq!(catalog(&st).len(), before, "a builtin preset is protected");
            assert!(catalog(&st).iter().any(|s| s.id == "tilo-blue"));
        }

        // Case `inline_name_edit_persists_into_the_selected_custom_theme`.
        {
            let (mut app, mut st, _env) = seeded_demo();
            // The demo's editable custom "sunset" is selected out of the box.
            assert_eq!(selected(&app), "sunset");
            // Inline text stays local until reconciliation dispatches the canonical
            // field action.
            app.gallery.stage_edited_name("My Theme");
            app.sync(&mut st.doc, &st.root);
            assert_eq!(
                catalog(&st).iter().find(|s| s.id == "sunset").unwrap().name,
                "My Theme"
            );
        }

        // Case `rename_command_selects_its_target`.
        {
            let (mut app, mut st, _env) = seeded();
            run(&mut app, &mut st, "duplicate:nord");
            let copy_id = catalog(&st)
                .iter()
                .find(|s| s.name == "Nord copy")
                .unwrap()
                .id
                .clone();
            run(&mut app, &mut st, "select:tilo-blue");
            run(&mut app, &mut st, &format!("rename:{copy_id}"));
            assert_eq!(
                selected(&app),
                copy_id,
                "rename selects the target so its name is editable"
            );
        }
    }

    /// Local navigation, export, New, apply and affordances dispatch the canonical actions and effects; an affordance delete is one undo boundary.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn operations_dispatch_canonical_actions_with_one_undo_boundary() {
        // Case `local_navigation_never_writes_or_overwrites_a_peer_theme_edit`.
        {
            let (mut app, mut state, _env) = seeded_demo();
            state.doc.commit();
            let mut peer = AutoCommit::load(&state.doc.save()).unwrap();
            assert!(doc::set_token(
                &mut peer,
                &automerge::ROOT,
                "sunset",
                TokenId::Accent,
                [1, 2, 3]
            ));
            state.doc.merge_incoming(&mut peer).unwrap();
            let heads_after_peer_edit = state.doc.get_heads();

            // Selection, token navigation, and opening the inline name editor are
            // all per-window presentation. None is a catalog mutation or an undo
            // boundary, and none may replay stale editor values over the peer edit.
            run(&mut app, &mut state, "select:tilo-blue");
            run(&mut app, &mut state, "edittoken:border");
            run(&mut app, &mut state, "rename:sunset");

            assert_eq!(state.doc.get_heads(), heads_after_peer_edit);
            assert_eq!(
                catalog(&state)
                    .iter()
                    .find(|theme| theme.id == "sunset")
                    .unwrap()
                    .accent,
                [1, 2, 3]
            );
            assert_eq!(selected(&app), "sunset");
            assert_eq!(app.edit_token, TokenId::Border);
        }

        // Case `export_is_a_brokered_local_effect_not_document_scratch`.
        {
            let (mut app, mut state, _env) = seeded_demo();
            state.doc.commit();
            let before_heads = state.doc.get_heads();
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        "themes.export",
                        &serde_json::to_vec(&()).unwrap()
                    ),
                    &mut state.doc,
                    &state.root
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(state.doc.get_heads(), before_heads);
            assert!(
                app.last_export
                    .as_deref()
                    .is_some_and(|json| json.contains("sunset"))
            );
            assert!(app.status.contains("Exported"));

            let resources = ui::resources::resources(&app, &state.doc);
            assert!(
                resources[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "export-selected")
            );
            assert!(
                resources[0]
                    .children
                    .iter()
                    .all(|theme| theme.actions.iter().any(|action| action.id == "export"))
            );
        }

        // Case `visible_new_operation_dispatches_the_canonical_create_action`.
        {
            let (mut app, mut state, _env) = seeded_demo();
            let before = catalog(&state).len();
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        "themes.new",
                        &serde_json::to_vec(&()).unwrap()
                    ),
                    &mut state.doc,
                    &state.root
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            app.sync(&mut state.doc, &state.root);
            assert_eq!(catalog(&state).len(), before + 1);
            let dispatched = app.last_dispatched().expect("create action dispatched");
            assert_eq!(dispatched.type_id, crate::actions::THEMES_ACTION_TYPE);
            assert!(matches!(
                serde_json::from_slice::<crate::actions::ThemesAction>(&dispatched.payload)
                    .unwrap(),
                crate::actions::ThemesAction::CreateCustomTheme { .. }
            ));
            assert!(
                ui::resources::resources(&app, &state.doc)[0]
                    .actions
                    .iter()
                    .any(|action| {
                        action.id == "new" && action.kind == tilo_actions::OperationKind::Effect
                    })
            );
        }

        // Case `selected_apply_operation_writes_the_active_theme_to_the_shared_store`.
        {
            let (mut app, mut st, _env) = seeded();
            run(&mut app, &mut st, "select:dracula");
            let invocation = tilo_actions::InvocationId::new();
            assert_eq!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        "themes.apply",
                        &serde_json::to_vec(&()).unwrap()
                    ),
                    &mut st.doc,
                    &st.root
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            app.sync(&mut st.doc, &st.root);
            // The in-app "Active" badge id updates immediately…
            assert_eq!(
                ui::doc::read_str(&st.doc, &st.root, doc::KEY_ACTIVE).as_deref(),
                Some("dracula")
            );
            // …and the shared settings store on disk now names it the OS theme.
            assert_eq!(store::load_active_id().as_deref(), Some("dracula"));
            let back =
                store::active_spec(&std::fs::read_to_string(store::store_path().unwrap()).unwrap())
                    .expect("the full applied spec round-trips out of the store");
            assert_eq!(back.id, "dracula");
        }

        // Case `affordance_dispatches_the_same_encoded_action_as_the_palette_spec`.
        // One source of truth on the **live widget path**: the delete a sidebar
        // affordance dispatches (drained through `process_command` →
        // `perform_action`) is the byte-identical [`EncodedAction`] the theme's
        // Ctrl+K "Delete theme" [`ActionSpec`] exports.
        {
            use tilo_actions::ActionInput;
            let (mut app, mut st, _env) = seeded_demo();
            assert_eq!(selected(&app), "sunset");

            // The affordance path: the sidebar's "Delete" command for the sunset row.
            run(&mut app, &mut st, "delete:sunset");
            let from_affordance = app
                .last_dispatched()
                .expect("a delete was dispatched")
                .clone();

            // The palette path: the sunset resource's exported delete spec. Rebuilt
            // from a fresh doc that still holds sunset (the affordance already
            // removed it from `st`), so both encode the same target.
            let mut probe = AutoCommit::new();
            doc::save_catalog(&mut probe, &automerge::ROOT, &demo_catalog());
            let resources = app.publish_resources(&probe);
            let sunset = resources[0]
                .children
                .iter()
                .find(|r| r.id.as_str() == "themes.theme:sunset")
                .expect("sunset resource");
            let spec = sunset
                .actions
                .iter()
                .find(|a| a.id == "delete")
                .expect("delete spec");
            let from_spec = match &spec.input {
                ActionInput::Immediate(enc) => enc.clone(),
                ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                    panic!("delete is an immediate action")
                }
            };

            assert_eq!(from_affordance.type_id, from_spec.type_id);
            assert_eq!(
                from_affordance.payload, from_spec.payload,
                "button and palette build the identical delete action"
            );
        }

        // Case `affordance_delete_is_one_undo_boundary_reverted_by_ctrl_z`.
        // The full undo covenant on the **live widget path**: a delete dispatched by
        // an affordance mutates the document, the host's `take_committed_action`
        // reports exactly one undo boundary bracketing it, and the host's Ctrl+Z
        // (`State::undo`) reverts precisely that delete.
        {
            let (mut app, mut st, _env) = seeded_demo();
            let before_len = catalog(&st).len();
            assert!(
                catalog(&st).iter().any(|s| s.id == "sunset"),
                "custom present"
            );

            run(&mut app, &mut st, "delete:sunset");
            assert!(
                !catalog(&st).iter().any(|s| s.id == "sunset"),
                "the delete landed"
            );

            // The host drains the affordance's captured heads (taken just before the
            // dispatch, so the command-bus churn is outside the bracket) and records
            // ONE boundary — exactly the `about_to_wait` path in `ui::App`.
            let local_author = ui::collaboration::local_user_id();
            let root: *mut dyn ui::widget::Widget = &mut app;
            assert!(
                ui::undo::record_dispatched_action(
                    &mut st,
                    root,
                    &std::collections::HashMap::new(),
                    &local_author,
                ),
                "the host recorded an undo boundary for the affordance's delete"
            );
            let entry = st
                .peek_last_undo()
                .expect("exactly one undo boundary recorded");
            assert_eq!(
                entry.author.as_ref(),
                Some(&local_author),
                "attributed to the local user, so their Ctrl+Z can reach it"
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "rising-edge: the boundary is consumed once"
            );

            // Ctrl+Z reverts the delete.
            st.undo_as(Some(&local_author), None, None)
                .expect("an undo entry to pop");
            app.sync(&mut st.doc, &st.root);
            assert_eq!(
                catalog(&st).len(),
                before_len,
                "undo restored the theme count"
            );
            assert!(
                catalog(&st).iter().any(|s| s.id == "sunset"),
                "sunset is back after undo"
            );
        }
    }

    /// Exported apply runs a cancellable background lifecycle, and a static gallery stops requesting frames.
    #[test]
    fn apply_lifecycle_is_cancellable_and_the_gallery_goes_idle() {
        // Case `exported_apply_uses_cancellable_background_lifecycle`.
        {
            let (mut app, mut st, _env) = seeded();
            let theme_id = catalog(&st)[0].id.clone();
            let invocation = tilo_actions::InvocationId::new();
            let started = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        "themes.apply",
                        &serde_json::to_vec(&theme_id).unwrap(),
                    ),
                    &mut st.doc,
                    &st.root,
                )
                .unwrap();
            assert_eq!(started, Some(ui::widget::ResourceOperationStart::Accepted));
            assert!(matches!(
                app.poll_resource_operation(invocation, true, &st.doc, &st.root),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));

            let apply = ui::resources::resources(&app, &st.doc)
                .into_iter()
                .flat_map(|resource| resource.children)
                .flat_map(|theme| theme.actions)
                .find(|action| action.id == "apply")
                .unwrap();
            assert!(matches!(
                apply.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(apply.contract.result, tilo_actions::ResultSpec::Job);
        }

        // Case `static_theme_gallery_stops_requesting_frames_after_draw`.
        {
            let (mut app, _state, _env) = seeded();
            let layout = app.layout_class();
            app.gallery_flap.snap(app.gallery_flap.is_open(layout));
            app.mark_drawn();
            assert!(
                !app.needs_animation_frame(),
                "an idle theme gallery must not keep the compositor at vsync"
            );
        }
    }
}

#[cfg(test)]
mod history_reconcile_tests {
    use super::*;
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one live cache fixture covers restore, retained rename commit/cancel, and deletion"
    )]
    fn historical_library_change_reloads_controls_and_preserves_active() {
        let mut d = AutoCommit::new();
        let mut spec = crate::model::seed_custom();
        doc::save_catalog(&mut d, &automerge::ROOT, &[spec.clone()]);
        doc::put_str(
            &mut d,
            &automerge::ROOT,
            doc::KEY_ACTIVE,
            "saved-applied-identity",
        );
        let mut app = ThemesApp::new();
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(
            ui::doc::read_str(&d, &automerge::ROOT, doc::KEY_ACTIVE).as_deref(),
            Some("saved-applied-identity")
        );
        app.selected = spec.id.clone();
        app.sync(&mut d, &automerge::ROOT);
        app.gallery.stage_edited_name("Queued stale name");
        spec.name = "Restored name".into();
        doc::set_name(&mut d, &automerge::ROOT, &spec.id, &spec.name);
        spec.accent = [1, 2, 3];
        spec.radius_norm = 0.13;
        spec.spacing_norm = 0.91;
        spec.font = "Source Serif".into();
        doc::set_token(
            &mut d,
            &automerge::ROOT,
            &spec.id,
            TokenId::Accent,
            spec.accent,
        );
        doc::set_radius(&mut d, &automerge::ROOT, &spec.id, spec.radius_norm);
        doc::set_spacing(&mut d, &automerge::ROOT, &spec.id, spec.spacing_norm);
        doc::set_font(&mut d, &automerge::ROOT, &spec.id, &spec.font);
        let heads = d.get_heads();
        app.sync(&mut d, &automerge::ROOT);
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(
            d.get_heads(),
            heads,
            "stale controls must never rewrite restored content"
        );
        assert_eq!(app.customizer.picker_color(), [1, 2, 3]);
        assert!((app.customizer.radius() - 0.13).abs() < 0.001);
        assert_eq!(app.gallery.displayed_name(), "Queued stale name");
        assert!(format!("{:?}", app.gallery.accessibility()).contains("Enter: apply draft"));
        assert!(
            app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Escape))
        );
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(app.gallery.displayed_name(), "Restored name");
        doc::set_name(&mut d, &automerge::ROOT, &spec.id, "Clean replacement");
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(app.gallery.displayed_name(), "Clean replacement");
        app.gallery.stage_edited_name("Retained draft");
        doc::set_name(&mut d, &automerge::ROOT, &spec.id, "Second restoration");
        let before = d.get_heads();
        app.sync(&mut d, &automerge::ROOT);
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(d.get_heads(), before);
        assert_eq!(app.gallery.displayed_name(), "Retained draft");
        assert!(
            app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Enter))
        );
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(
            doc::load_catalog(&d, &automerge::ROOT)
                .unwrap()
                .iter()
                .find(|s| s.id == spec.id)
                .unwrap()
                .name,
            "Retained draft"
        );
        let target = d.get_heads();
        doc::set_name(&mut d, &automerge::ROOT, &spec.id, "Before history");
        app.sync(&mut d, &automerge::ROOT);
        app.gallery.stage_edited_name("Draft across restore");
        let heads = d.get_heads();
        let plan = ui::history_mode::restore::prepare_content_restore(
            &d,
            &crate::framework_history::SCHEMA,
            &heads,
            &target,
        )
        .unwrap();
        ui::history_mode::restore::apply_prepared_content_restore(&mut d, &plan).unwrap();
        let restored = d.get_heads();
        app.sync(&mut d, &automerge::ROOT);
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(d.get_heads(), restored);
        assert_eq!(app.gallery.displayed_name(), "Draft across restore");
        assert!(
            app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Escape))
        );
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(app.gallery.displayed_name(), "Retained draft");
        app.gallery.stage_edited_name("Never recreate");
        doc::delete_custom_theme(&mut d, &automerge::ROOT, &spec.id);
        let heads = d.get_heads();
        app.sync(&mut d, &automerge::ROOT);
        assert!(
            !app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Enter))
        );
        app.sync(&mut d, &automerge::ROOT);
        assert_eq!(d.get_heads(), heads);
        assert!(
            !doc::load_catalog(&d, &automerge::ROOT)
                .unwrap()
                .iter()
                .any(|s| s.id == spec.id)
        );
    }
}
