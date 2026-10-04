//! Root widget for the Themes app: the surface over the [`Themes`] library.
//! It shows the library the framework presents in three panels (gallery /
//! preview / customizer) under a top bar, keeps the selection and the editor
//! controls as per-window view state, and turns what a person does on the
//! controls into [`ThemesAction`]s submitted through the framework's
//! dispatcher. A slider or colour drag applies every value it passes through
//! and is one undo step; letting go settles it.
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

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::tilo_actions;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::AutoCommit;
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::framework::{Dispatcher, EffectAnswer, EffectCx, Effected, ResourceSurface, SurfaceCx};
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

use crate::actions::ThemesAction;
use crate::customizer::{ControlEdit, Customizer};
use crate::draw;
use crate::gallery::Gallery;
use crate::model::{FONTS, ThemeSpec, TokenId, default_catalog};
use crate::preview::PreviewPane;
use crate::resource::{Themes, ThemesEffect, ThemesState, scenario};
use crate::store;
use ui::storage::SettingsDocument;

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
/// How far the scrolled screenshot scenarios scroll the narrow stack.
const SCENARIO_SCROLL: f64 = 260.0;

/// A top-bar button invoking one of the studio's effects through the same
/// door as the palette, or disabled when the effect is not published.
fn effect_button(id: &str, label: &str, effect: &ThemesEffect) -> Button {
    let button = Button::new(id, label, None).fit_content(TOPBAR_CONTROL_H);
    match crate::actions::effect_binding(effect) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

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
    /// The library as last presented, and the theme applied as the system's.
    catalog: Vec<ThemeSpec>,
    active: String,
    topbar_buttons: Vec<Button>,
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

    /// The theme and colour slot the controls show, and the theme as they
    /// last loaded it: a different theme, or the same one changed by anyone,
    /// reloads them.
    bound_key: Option<(String, TokenId)>,
    loaded: Option<ThemeSpec>,
    status: String,
    structure_dirty: bool,

    /// Where edits go; `None` until the framework attaches the surface.
    dispatch: Option<Dispatcher<Themes>>,
    /// The person may only look: a past version, or no write permission.
    read_only: bool,
    /// The settings document the host gave the app, where Apply records the
    /// device's theme.
    settings: SettingsDocument,
    /// A theme this window just created, selected once the library holds it.
    pending_select: Option<String>,

    /// Vertical scroll of the collapsed (narrow) preview+customizer stack.
    scroll: f64,
    /// Total stacked content height + its viewport, cached in `apply_layout`
    /// so the wheel handler and scrollbar can clamp / size against them.
    scroll_content_h: f64,
    scroll_viewport_h: f64,
    /// Scene-space viewport rect of the scrolling content region (below the
    /// top bar), in the narrow layout.
    content_rect: Rect,

    /// View state a screenshot scenario stages (see
    /// [`ResourceSurface::stage`]). Never set in a live window.
    staged: Staged,
}

/// What a screenshot scenario holds in place once the rows and the stack
/// have their geometry.
#[derive(Default)]
struct Staged {
    /// Open the library's actions menu on the selected row.
    menu: bool,
    /// Scroll the narrow stack to this offset.
    scroll: Option<f64>,
}

impl Default for ThemesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemesApp {
    pub fn new() -> Self {
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
            active: String::new(),
            topbar_buttons: vec![
                effect_button("themes.new", "New", &ThemesEffect::New { name: None }),
                effect_button(
                    "themes.export",
                    "Export JSON",
                    &ThemesEffect::Export { theme: None },
                ),
                effect_button(
                    "themes.apply",
                    "Apply theme",
                    &ThemesEffect::ApplyTheme { theme: None },
                )
                .primary(),
            ],
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
            loaded: None,
            status: String::new(),
            structure_dirty: false,
            dispatch: None,
            read_only: false,
            settings: SettingsDocument::default(),
            pending_select: None,
            scroll: 0.0,
            scroll_content_h: 0.0,
            scroll_viewport_h: 0.0,
            content_rect: Rect::ZERO,
            staged: Staged::default(),
        }
    }

    /// Stage the view a screenshot scenario shows; see [`scenario`].
    fn stage_scenario(&mut self, name: Option<&str>) {
        let custom = crate::model::seed_custom().id;
        match name {
            Some(scenario::LIVE | scenario::SCROLL) => {
                if name == Some(scenario::SCROLL) {
                    self.select(&custom);
                }
                self.gallery_flap.snap(false);
                self.staged.scroll = Some(SCENARIO_SCROLL);
            }
            Some(scenario::CUSTOMIZE) => {
                self.select(&custom);
                self.gallery_flap.snap(false);
            }
            Some(scenario::PREVIEW) => {
                self.select(&custom);
                self.gallery_flap.snap(false);
                self.narrow_view_chosen = true;
                self.set_narrow_preview(true);
                self.staged.scroll = Some(SCENARIO_SCROLL);
            }
            Some(scenario::MENU) => {
                self.select(&custom);
                self.gallery_flap.snap(true);
                self.staged.menu = true;
            }
            Some(scenario::FONT) => {
                self.select("dracula");
                self.gallery_flap.snap(true);
            }
            _ => {
                self.select(&custom);
                self.gallery_flap.snap(true);
            }
        }
        self.structure_dirty = true;
    }

    /// Show the theme `id` (presentation only).
    fn select(&mut self, id: &str) {
        id.clone_into(&mut self.selected);
        self.status.clear();
        self.refresh_view();
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

    /// The theme the controls edit.
    fn selected_spec(&self) -> Option<&ThemeSpec> {
        self.catalog.iter().find(|spec| spec.id == self.selected)
    }

    /// Submit one edit through the framework door: applied now when the
    /// input event's document is at hand, so a drag shows every value it
    /// passes through and continues one undo step; queued for the host
    /// otherwise, as a bound control's action is.
    fn submit(&self, doc: Option<&mut AutoCommit>, action: &ThemesAction) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        match doc {
            Some(doc) => {
                let _ = dispatch.apply_now(doc, action);
            }
            None => dispatch.apply(action),
        }
    }

    /// Turn what the person did on the controls since the last call into
    /// edits: library commands, the colour slot, values set on the controls
    /// and a committed inline name.
    fn commit(&mut self, mut doc: Option<&mut AutoCommit>) {
        while let Some(command) = self.gallery.take_command() {
            if let Some(action) = self.process_command(&command) {
                self.submit(doc.as_deref_mut(), &action);
            }
        }
        if let Some(token) = self.customizer.take_edit_token() {
            self.edit_token = token;
        }
        for edit in self.customizer.take_edits() {
            if let Some(action) = self.edit_action(edit) {
                self.submit(doc.as_deref_mut(), &action);
            }
        }
        if let Some(name) = self.gallery.take_edited_name() {
            let name = name.trim();
            if let Some(current) = self.selected_spec()
                && !current.builtin
                && !name.is_empty()
                && name != current.name
            {
                let action = crate::actions::set_custom_theme_name_action(&current.id, name);
                self.submit(doc, &action);
            }
        }
    }

    /// The edit a value set on one of the customizer's controls makes to the
    /// selected custom theme, unless it already holds that value.
    fn edit_action(&self, edit: ControlEdit) -> Option<ThemesAction> {
        use crate::actions as a;
        use crate::model as m;
        let current = self.selected_spec().filter(|spec| !spec.builtin)?;
        let id = current.id.as_str();
        let changed = |value: f32, stored: f32| (value - stored).abs() > f32::EPSILON;
        match edit {
            ControlEdit::Color(rgb) => (rgb != current.token(self.edit_token))
                .then(|| a::set_custom_theme_token_action(id, self.edit_token, rgb)),
            ControlEdit::Radius(v) => changed(v as f32, current.radius_norm)
                .then(|| a::set_custom_theme_radius_action(id, v as f32)),
            ControlEdit::Spacing(v) => changed(v as f32, current.spacing_norm)
                .then(|| a::set_custom_theme_spacing_action(id, v as f32)),
            ControlEdit::Motion(v) => {
                let scale = m::motion_scale_from_norm(v as f32);
                changed(scale, current.motion_scale)
                    .then(|| a::set_custom_theme_motion_action(id, scale))
            }
            ControlEdit::Material(v) => {
                let scale = m::material_scale_from_norm(v as f32);
                changed(scale, current.material_scale)
                    .then(|| a::set_custom_theme_material_action(id, scale))
            }
            ControlEdit::Icon(v) => {
                let scale = m::icon_scale_from_norm(v as f32);
                changed(scale, current.icon_scale)
                    .then(|| a::set_custom_theme_icon_action(id, scale))
            }
            ControlEdit::Chrome(v) => {
                let weight = m::chrome_weight_from_norm(v as f32);
                changed(weight, current.chrome_weight)
                    .then(|| a::set_custom_theme_chrome_action(id, weight))
            }
            ControlEdit::Character(v) => {
                let character = m::motion_character_from_norm(v as f32);
                changed(character, current.motion_character)
                    .then(|| a::set_custom_theme_motion_character_action(id, character))
            }
            ControlEdit::Tint(v) => {
                let tint = m::material_tint_from_norm(v as f32);
                changed(tint, current.material_tint)
                    .then(|| a::set_custom_theme_material_tint_action(id, tint))
            }
            ControlEdit::Font(font) => (FONTS.contains(&font.as_str()) && font != current.font)
                .then(|| a::set_custom_theme_font_action(id, font)),
        }
    }

    /// Route one library command. Selection, the edited colour slot and
    /// rename-focus are presentation; duplicate and delete are the edit they
    /// return. Commands carry their target id (the right-clicked row need not
    /// be selected).
    fn process_command(&mut self, cmd: &str) -> Option<ThemesAction> {
        if let Some(id) = cmd.strip_prefix("select:") {
            self.select(id);
            return None;
        }
        if let Some(key) = cmd.strip_prefix("edittoken:") {
            self.edit_token = TokenId::from_key(key).unwrap_or(TokenId::Accent);
            return None;
        }
        if let Some(id) = cmd.strip_prefix("rename:") {
            // Select the theme, then hand focus to the sidebar's inline
            // editor; the committed name is an edit of its own.
            self.select(id);
            self.gallery.request_name_focus();
            return None;
        }
        if let Some(id) = cmd.strip_prefix("duplicate:") {
            return self.duplicate_theme(id);
        }
        if let Some(id) = cmd.strip_prefix("delete:") {
            return self.delete_theme(id);
        }
        None
    }

    /// An editable copy of the theme `id`, selected once the library holds it.
    fn duplicate_theme(&mut self, id: &str) -> Option<ThemesAction> {
        let src = self.catalog.iter().find(|s| s.id == id)?.clone();
        let new_id = Self::new_id();
        let order = crate::actions::max_order(&self.catalog) + 1;
        let spec = crate::actions::duplicate_spec(&src, &new_id, order);
        self.pending_select = Some(new_id);
        self.status = format!("Duplicated “{}”", src.name);
        self.structure_dirty = true;
        Some(crate::actions::upsert_custom_theme_action(&spec))
    }

    /// Delete the custom theme `id` and move the selection to a surviving
    /// theme. Presets are protected and the library is never emptied, here
    /// (so the selection only moves for a real delete) and in
    /// [`ThemesAction::apply`](crate::actions::ThemesAction) (so a replay is
    /// a safe no-op).
    fn delete_theme(&mut self, id: &str) -> Option<ThemesAction> {
        let idx = self.catalog.iter().position(|s| s.id == id)?;
        let entry = self.catalog.get(idx)?;
        if entry.builtin || self.catalog.len() <= 1 {
            return None;
        }
        let name = entry.name.clone();
        // The row that slides into `idx`, or the new last row when `idx`
        // was the tail.
        let survivors: Vec<&ThemeSpec> = self.catalog.iter().filter(|s| s.id != id).collect();
        if self.selected == id {
            self.selected = survivors
                .get(idx.min(survivors.len().saturating_sub(1)))
                .map(|s| s.id.clone())
                .unwrap_or_default();
        }
        self.status = format!("Deleted “{name}”");
        self.structure_dirty = true;
        Some(crate::actions::delete_custom_theme_action(id))
    }

    /// Show the library the framework presents: a local edit, a
    /// collaborator's, an undo, or a past version in History.
    fn present_state(&mut self, state: &ThemesState) {
        self.catalog = if state.catalog.is_empty() {
            default_catalog()
        } else {
            state.catalog.clone()
        };
        self.active = state.active.clone().unwrap_or_default();
        if let Some(id) = self.pending_select.take()
            && self.catalog.iter().any(|s| s.id == id)
        {
            self.selected = id;
        }
        self.refresh_view();
    }

    /// Push the selection into the panels: resolve it against the library,
    /// reload the controls when the theme they show changed, and bind the
    /// customizer's buttons to the selected theme's published actions.
    fn refresh_view(&mut self) {
        if !self.catalog.iter().any(|s| s.id == self.selected) {
            let fallback = self
                .catalog
                .iter()
                .find(|s| s.id == self.active)
                .or_else(|| self.catalog.first());
            self.selected = fallback.map(|s| s.id.clone()).unwrap_or_default();
        }
        let Some(spec) = self.selected_spec().cloned() else {
            return;
        };
        let key = (spec.id.clone(), self.edit_token);
        if self.bound_key.as_ref() != Some(&key) || self.loaded.as_ref() != Some(&spec) {
            self.customizer.load_controls(&spec);
            self.customizer.load_picker(spec.token(self.edit_token));
            self.bound_key = Some(key);
            self.loaded = Some(spec.clone());
        }
        // A live preset is primarily a specimen; an editable theme starts at
        // its controls. Once a view is chosen, keep that local preference.
        if !self.narrow_view_chosen {
            self.set_narrow_preview(spec.builtin);
        }
        let editable = !spec.builtin && !self.read_only;
        self.gallery
            .set_view(&self.catalog, &self.selected, &self.active, editable);
        self.preview.set_spec(&spec);
        // A duplicate names the copy it creates, so the buttons are bound
        // again whenever the library grows.
        let next_order = crate::actions::max_order(&self.catalog) + 1;
        let theme = crate::actions::theme_resource(&spec, next_order);
        let binding = |id| tilo_actions::ActionBinding::unit_for(&theme, id).ok();
        if let Some(duplicate) = binding("duplicate") {
            self.customizer.bind_actions(
                &format!("{}@{next_order}", spec.id),
                duplicate,
                binding("reset"),
                binding("delete"),
            );
        }
        self.customizer.set_view(spec, self.edit_token, editable);
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
        if let Some(scroll) = self.staged.scroll {
            self.scroll = scroll;
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
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if let Some(preview) = self.narrow_view_request.take() {
            self.narrow_view_chosen = true;
            self.set_narrow_preview(preview);
        }
        // What changed outside an input event (an assistive technology
        // setting a slider) goes through the door as a bound control's
        // action does.
        self.commit(None);
        self.refresh_view();

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

        // The menu scenario opens the library's actions menu once the rows
        // have rects to anchor it to.
        if std::mem::take(&mut self.staged.menu) {
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
        self.route_event(event, ctx);
        self.commit(Some(&mut *ctx.state));
        // Letting go of a slider or the colour picker ends its gesture: the
        // values one drag passed through are one undo step, the next drag is
        // another.
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
            && let Some(dispatch) = &self.dispatch
        {
            dispatch.settle();
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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

impl ThemesApp {
    /// Deliver one input event to the control it belongs to.
    fn route_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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
}

impl ResourceSurface<Themes, ThemesEffect> for ThemesApp {
    fn present(&mut self, state: &Rc<ThemesState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Themes>) {
        self.read_only = cx.read_only();
        self.settings = cx.storage().settings_document().clone();
        self.dispatch = Some(cx.dispatcher());
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.read_only = read_only;
        self.refresh_view();
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: ThemesEffect, cx: &mut EffectCx<Themes, Self>) -> EffectAnswer {
        match effect {
            ThemesEffect::New { name } => {
                let id = Self::new_id();
                let name = name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or("New theme");
                let order = crate::actions::max_order(&self.catalog) + 1;
                let spec = crate::actions::new_theme_spec(&id, name, order);
                cx.apply(&crate::actions::upsert_custom_theme_action(&spec));
                self.pending_select = Some(id);
                self.status = "Created a new theme".to_string();
                self.structure_dirty = true;
                Ok(Effected::done())
            }
            ThemesEffect::ApplyTheme { theme } => {
                let spec = self.effect_target(theme)?;
                let settings = self.settings.clone();
                let stored = spec.clone();
                Ok(cx.thread(
                    move |_| store::save(&settings, &stored).map_err(|error| error.to_string()),
                    move |app: &mut Self, saved, cx| {
                        match saved {
                            Ok(()) => {
                                cx.apply(&crate::actions::set_active_theme_action(&spec.id));
                                app.status = format!("Applied “{}” as the system theme", spec.name);
                            }
                            Err(error) => {
                                app.status = format!("Couldn't apply theme: {error}");
                                return Err(error);
                            }
                        }
                        Ok(Effected::done())
                    },
                ))
            }
            ThemesEffect::Export { theme } => {
                let spec = self.effect_target(theme)?;
                self.status = format!("Exported “{}” as JSON", spec.name);
                Ok(Effected::value(&spec))
            }
        }
    }
}

impl ThemesApp {
    #[cfg(test)]
    pub(crate) fn gallery_menu_is_open(&self) -> bool {
        self.gallery.menu_is_open()
    }

    /// The theme an effect names, else the selected one.
    fn effect_target(&self, theme: Option<String>) -> Result<ThemeSpec, String> {
        let id = theme.unwrap_or_else(|| self.selected.clone());
        self.catalog
            .iter()
            .find(|spec| spec.id == id)
            .cloned()
            .ok_or_else(|| "Theme is no longer available".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::demo_catalog;
    use ui::headless::Framed;
    use ui::prelude::TestApp;
    use ui::tilo_resource::Invoke;

    /// The surface showing `catalog`, with the first preset applied.
    fn presented(catalog: Vec<ThemeSpec>) -> ThemesApp {
        let mut app = ThemesApp::new();
        app.present_state(&ThemesState {
            catalog,
            active: default_catalog().first().map(|theme| theme.id.clone()),
        });
        app
    }

    /// The presets only, showing the applied one.
    fn seeded() -> ThemesApp {
        presented(default_catalog())
    }

    /// The presets and the custom "sunset", showing the custom theme.
    fn seeded_demo() -> ThemesApp {
        let mut app = presented(demo_catalog());
        app.select("sunset");
        app
    }

    /// Mount `app` in the shared frame harness at `width`x`height`.
    fn framed(app: ThemesApp, width: u32, height: u32) -> Framed<ThemesApp> {
        Framed::new(ui::State::new(), app, (width, height), Theme::dark())
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
            let app = framed(seeded_demo(), 480, 900);
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
            let app = framed(seeded(), 480, 900);
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
            let mut app = seeded_demo();
            app.stage_scenario(Some(scenario::PREVIEW));
            let app = framed(app, 480, 900);
            assert!(app.narrow_preview);
            assert_eq!(
                narrow_view_state(&app),
                ("Full preview".to_owned(), [false, true])
            );
        }

        // Case `narrow_full_preview_contains_and_can_scroll_to_the_specimen_tail`.
        {
            let mut app = framed(seeded(), 480, 480);
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
    fn narrow_view_follows_pointer_keyboard_resize_and_escape() {
        let mut app = seeded_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let (mut tree, mut node) = layout_narrow(&mut app, 480.0);
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
    }

    /// The running app with the custom theme "sunset" beside the presets.
    fn running_demo() -> TestApp<crate::resource::ThemesShell> {
        let mut app = TestApp::<crate::resource::ThemesShell>::new().unwrap();
        for invoke in <crate::resource::ThemesShell as ui::framework::ResourceApp>::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        app
    }

    /// Run `visit` on the app's surface, then the frames that follow.
    fn on_surface<O>(
        app: &mut TestApp<crate::resource::ThemesShell>,
        visit: impl FnOnce(&mut ThemesApp) -> O,
    ) -> O {
        let output = app.surface(visit).expect("the themes surface");
        app.settle();
        output
    }

    fn command(app: &mut TestApp<crate::resource::ThemesShell>, command: &str) {
        on_surface(app, |surface| surface.gallery.queue_command(command));
    }

    fn selected(app: &mut TestApp<crate::resource::ThemesShell>) -> String {
        on_surface(app, |surface| surface.selected.clone())
    }

    fn theme(app: &TestApp<crate::resource::ThemesShell>, id: &str) -> Option<ThemeSpec> {
        app.state().catalog.into_iter().find(|theme| theme.id == id)
    }

    /// Library commands: duplicate, delete (never a preset), rename, and an
    /// inline name edit, each through the framework door.
    #[test]
    fn library_commands_duplicate_delete_and_rename_through_the_door() {
        let mut app = running_demo();
        assert_eq!(selected(&mut app), default_catalog()[0].id);

        // Duplicating a preset adds an editable copy and shows it.
        let before = app.state().catalog.len();
        command(&mut app, "duplicate:tilo-blue");
        let catalog = app.state().catalog;
        assert_eq!(catalog.len(), before + 1);
        let copy = catalog
            .iter()
            .find(|theme| theme.name == "Tilo Blue copy")
            .expect("the copy");
        assert!(!copy.builtin, "the copy is editable");
        assert_eq!(selected(&mut app), copy.id);

        // A preset is protected.
        command(&mut app, "delete:tilo-blue");
        assert!(theme(&app, "tilo-blue").is_some());

        // Rename selects its target; the committed name is an edit.
        command(&mut app, "rename:sunset");
        assert_eq!(selected(&mut app), "sunset");
        on_surface(&mut app, |surface| {
            surface.gallery.stage_edited_name("My Theme");
        });
        assert_eq!(theme(&app, "sunset").unwrap().name, "My Theme");

        // Deleting the shown custom theme moves the selection to a survivor,
        // and one undo brings it back.
        command(&mut app, "delete:sunset");
        assert!(theme(&app, "sunset").is_none());
        let shown = selected(&mut app);
        assert!(theme(&app, &shown).is_some(), "selection stays valid");
        assert!(app.undo());
        assert_eq!(theme(&app, "sunset").unwrap().name, "My Theme");
    }

    /// Selection, the colour slot and opening the inline name editor are
    /// presentation: none writes the library, nor replays stale controls
    /// over a collaborator's edit.
    #[test]
    fn local_navigation_never_writes_or_overwrites_a_peer_theme_edit() {
        let mut app = running_demo();
        command(&mut app, "select:sunset");
        app.merge_peer_edit(|doc| {
            assert!(crate::doc::set_token(
                doc,
                &automerge::ROOT,
                "sunset",
                TokenId::Accent,
                [1, 2, 3]
            ));
        });
        app.settle();
        let heads = app.read_document(|doc| doc.clone().get_heads());

        command(&mut app, "select:tilo-blue");
        command(&mut app, "edittoken:border");
        command(&mut app, "rename:sunset");

        assert_eq!(app.read_document(|doc| doc.clone().get_heads()), heads);
        assert_eq!(theme(&app, "sunset").unwrap().accent, [1, 2, 3]);
        assert_eq!(selected(&mut app), "sunset");
        assert_eq!(
            on_surface(&mut app, |surface| surface.edit_token),
            TokenId::Border
        );
        // The controls show the collaborator's colour.
        assert_eq!(
            on_surface(&mut app, |surface| {
                surface.edit_token = TokenId::Accent;
                surface.refresh_view();
                surface.customizer.picker_color()
            }),
            [1, 2, 3]
        );
    }

    /// Every value one drag of a slider passes through is applied as it
    /// goes and the whole drag is one undo step; the next drag is another.
    #[test]
    fn a_slider_drag_is_one_undo_step() {
        let radius = |app: &TestApp<crate::resource::ThemesShell>| {
            theme(app, "sunset").expect("the custom theme").radius_norm
        };
        let mut app = running_demo();
        command(&mut app, "select:sunset");
        let original = radius(&app);
        let slider = app.harness().targets()["radius_slider"];
        let at = |fraction: f64| (slider.x0 + slider.width() * fraction, slider.center().y);

        app.harness().drag_at(at(0.9), at(0.2));
        app.settle();
        let first = radius(&app);
        assert!(first < 0.3, "{original} -> {first}");

        app.harness().drag_at(at(0.2), at(0.95));
        app.settle();
        assert!(radius(&app) > 0.9);

        assert!(app.undo());
        assert!(
            (radius(&app) - first).abs() < 1e-6,
            "one undo takes back one drag"
        );
        assert!(app.undo());
        assert!(
            (radius(&app) - original).abs() < 1e-6,
            "and the drag before it"
        );
    }

    /// The customizer's Duplicate button invokes the theme's published
    /// action, and a second press makes a second copy.
    #[test]
    fn the_duplicate_button_makes_a_new_copy_every_time() {
        let mut app = running_demo();
        command(&mut app, "select:sunset");
        let before = app.state().catalog.len();
        for _ in 0..2 {
            let target = app.harness().targets()["themes.duplicate"];
            app.harness().click_at(target.center().x, target.center().y);
            app.settle();
        }
        assert_eq!(app.state().catalog.len(), before + 2);
    }

    /// History draws the library as it was: the view over a past version.
    #[test]
    fn a_past_version_shows_the_library_as_it_was() {
        let mut app = running_demo();
        let before = app.read_document(|doc| doc.clone().get_heads());
        app.invoke(
            Invoke::apply(&crate::actions::set_custom_theme_name_action(
                "sunset", "Later",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(theme(&app, "sunset").unwrap().name, "Later");
        let past = app.past_version(&before).unwrap();
        let sunset = past
            .state()
            .catalog
            .iter()
            .find(|theme| theme.id == "sunset")
            .cloned()
            .unwrap();
        assert_eq!(sunset.name, crate::model::seed_custom().name);
    }

    /// A static gallery stops requesting frames.
    #[test]
    fn a_static_gallery_goes_idle() {
        // Case `static_theme_gallery_stops_requesting_frames_after_draw`.
        {
            let mut app = seeded();
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
mod reload_tests {
    use super::*;

    /// A change to the shown theme — a collaborator's, an undo, a past
    /// version — reloads the controls, while an unfinished local rename is
    /// kept until the person applies or drops it.
    #[test]
    fn a_changed_theme_reloads_the_controls_and_keeps_a_draft_name() {
        let mut spec = crate::model::seed_custom();
        let mut catalog = crate::model::presets();
        catalog.push(spec.clone());
        let mut app = ThemesApp::new();
        let present = |app: &mut ThemesApp, spec: &ThemeSpec| {
            let mut catalog = crate::model::presets();
            catalog.push(spec.clone());
            app.present_state(&ThemesState {
                catalog,
                active: Some("saved-applied-identity".into()),
            });
        };
        app.present_state(&ThemesState {
            catalog,
            active: Some("saved-applied-identity".into()),
        });
        app.select(&spec.id);
        app.gallery.stage_edited_name("Queued stale name");

        spec.name = "Restored name".into();
        spec.accent = [1, 2, 3];
        spec.radius_norm = 0.13;
        spec.spacing_norm = 0.91;
        spec.font = "Source Serif".into();
        present(&mut app, &spec);
        assert_eq!(app.customizer.picker_color(), [1, 2, 3]);
        assert!((app.customizer.radius() - 0.13).abs() < 0.001);
        assert!(app.customizer.take_edits().is_empty(), "loading is no edit");
        assert_eq!(app.gallery.displayed_name(), "Queued stale name");
        assert!(format!("{:?}", app.gallery.accessibility()).contains("Enter: apply draft"));
        assert!(
            app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Escape))
        );
        present(&mut app, &spec);
        assert_eq!(app.gallery.displayed_name(), "Restored name");

        spec.name = "Clean replacement".into();
        present(&mut app, &spec);
        assert_eq!(app.gallery.displayed_name(), "Clean replacement");

        app.gallery.stage_edited_name("Retained draft");
        spec.name = "Second restoration".into();
        present(&mut app, &spec);
        assert_eq!(app.gallery.displayed_name(), "Retained draft");
        assert!(
            app.gallery
                .resolve_name_conflict(&Key::Named(NamedKey::Enter))
        );
        assert_eq!(
            app.gallery.take_edited_name().as_deref(),
            Some("Retained draft"),
            "applying the draft commits it"
        );
    }
}
