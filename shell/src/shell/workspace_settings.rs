//! `WorkspaceSettings` — the contextual lens opened from the `TopBar`'s
//! workspace-name button, with General / Members / Privacy & Security /
//! Apps & Files tabs.
//!
//! Members and Permissions are both views of [`Workspace::capabilities`]
//! — the single list `decide_capability` consults, the sandbox turns
//! into bwrap/Landlock rules, and the room replicates. A member's role
//! *is* the verb on their membership capability
//! ([`Workspace::role_of`]); nothing here keeps a parallel vocabulary
//! that could drift from what is enforced.
//!
//! Every mutation routes through [`Workspace::grant_as`] /
//! [`Workspace::revoke_as`], which refuse callers failing
//! [`Workspace::may_administer`]. Non-owners see the same information
//! with controls painted disabled, not hidden.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::ColorCompat;
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::overlay::{FocusTrap, Modal};
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::shell::state::{
    CapOverride, CapRole, Effect, Expiry, Principal, Scope, Subject, UserId, Verb, Workspace,
    WorkspaceId,
};

/// The right-pane drawer [`WorkspaceSettings::draw_body`] dispatches to.
pub type PaneDrawer<'a> = dyn FnMut(&mut Scene, Affine, Rect, &mut TextRenderer, &Theme) + 'a;

/// Compose `transform` with a translation to `(dx, dy)`. `Affine`'s `Mul`
/// is pure `f64` matrix composition — it cannot overflow or panic — but
/// `arithmetic_side_effects` can't see that for a non-primitive `Mul`
/// impl, so every call site funnels through this one function instead of
/// an `#[allow]` at each of them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// A colour paired with the alpha to paint it at, bundled so a caller of
/// [`draw_pill`] doesn't pass a bare `f32` beside a `Color`.
#[derive(Clone, Copy)]
struct Tint {
    color: Color,
    alpha: f32,
}

/// Width of the modal's left sidebar in CSS px.
pub const SIDEBAR_WIDTH: f64 = 256.0;

const SIDEBAR_IDENTITY_TOP: f64 = 24.0;
const NAV_ROW_HEIGHT: f64 = 40.0;
const NAV_ROW_GAP: f64 = 4.0;
/// Y origin (relative to the sidebar's top edge) of the first nav row —
/// reserves room for the identity badge + caption.
const NAV_ROWS_TOP: f64 = 120.0;

/// Tab selector for the shell-owned `WorkspaceSettings` lens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WsTab {
    /// Workspace identity, invite link, collaborators.
    #[default]
    General,
    /// Who is in the workspace, their role, and how many capabilities
    /// they hold. Rendered from [`Workspace::roster`].
    Members,
    /// Every capability the workspace has granted, and the affordance
    /// to grant another. Rendered from [`Workspace::capabilities`].
    Permissions,
    /// Shared default-app policy plus this person's explicit override.
    Apps,
}

impl WsTab {
    pub fn label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Members => "Members",
            WsTab::Permissions => "Privacy & Security",
            WsTab::Apps => "Apps & Files",
        }
    }

    /// Short labels for the same destinations when the lens promotes to a
    /// narrow-screen sheet. This is responsive copy, not a second nav model.
    pub fn compact_label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Members => "People",
            WsTab::Permissions => "Privacy",
            WsTab::Apps => "Apps",
        }
    }

    /// Iteration order for the sidebar nav and tab strip. Every tab is
    /// always reachable — a host must reach Permissions before any
    /// grant exists.
    pub fn all() -> [WsTab; 4] {
        [
            WsTab::General,
            WsTab::Members,
            WsTab::Permissions,
            WsTab::Apps,
        ]
    }
}

/// The workspace's invite URL, kept in one place so this and the
/// compositor's inline copy can't drift.
pub fn invite_link(ws: &Workspace) -> String {
    format!("quickdesktop://invite/{}", u64::from(ws.id))
}

/// The `WorkspaceSettings` lens state.
#[derive(Debug)]
pub struct WorkspaceSettings {
    pub ws_id: WorkspaceId,
    pub active_tab: WsTab,
    modal: Modal,
    focus_trap: FocusTrap,
}

impl WorkspaceSettings {
    pub fn new(ws_id: WorkspaceId) -> Self {
        let members = WsTab::all().into_iter().map(tab_focus_id).collect();
        Self {
            ws_id,
            active_tab: WsTab::General,
            modal: Modal::new("workspace-settings"),
            focus_trap: FocusTrap::new(members),
        }
    }

    pub fn modal(&self) -> &Modal {
        &self.modal
    }

    pub fn focus_trap(&self) -> &FocusTrap {
        &self.focus_trap
    }

    pub fn select_tab(&mut self, tab: WsTab) {
        self.active_tab = tab;
    }

    /// Synonym for [`Self::select_tab`]. Retained for callers that
    /// already use the imperative `set_*` form.
    pub fn set_tab(&mut self, tab: WsTab) {
        self.select_tab(tab);
    }

    pub fn active_tab(&self) -> WsTab {
        self.active_tab
    }

    /// Does not check `ws.id == self.ws_id`, so the caller can rename
    /// any workspace it holds a reference to.
    pub fn rename(&mut self, ws: &mut Workspace, new_name: &str) {
        ws.name = new_name.to_string();
    }

    pub fn remove_collaborator(&mut self, ws: &mut Workspace, collab_id: &str) {
        ws.collaborators.retain(|c| c.id != collab_id);
    }

    pub fn invite_link(&self, ws: &Workspace) -> String {
        invite_link(ws)
    }

    // -----------------------------------------------------------------
    // Sidebar nav layout
    // -----------------------------------------------------------------

    /// The sidebar's bounds. `Rect::ZERO` when the modal isn't wide
    /// enough to host the full sidebar.
    pub fn sidebar_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return Rect::ZERO;
        }
        Rect::new(
            modal_rect.x0,
            modal_rect.y0,
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y1,
        )
    }

    pub fn pane_rect(&self, modal_rect: Rect) -> Rect {
        if modal_rect.width() <= SIDEBAR_WIDTH {
            return modal_rect;
        }
        Rect::new(
            modal_rect.x0 + SIDEBAR_WIDTH,
            modal_rect.y0,
            modal_rect.x1,
            modal_rect.y1,
        )
    }

    /// Per-row hit-test rects, one per [`WsTab`] in [`WsTab::all`] order.
    pub fn nav_rows(&self, modal_rect: Rect) -> [(WsTab, Rect); 4] {
        let sidebar = self.sidebar_rect(modal_rect);
        let inset_x0 = sidebar.x0 + 16.0;
        let inset_x1 = sidebar.x1 - 16.0;
        let tabs = WsTab::all();
        std::array::from_fn(|i| {
            // `i` is a `std::array::from_fn::<_, 3>` index, always 0..=2.
            #[allow(
                clippy::cast_precision_loss,
                reason = "i is a fixed 3-element array index, always 0..=2"
            )]
            let y0 = sidebar.y0 + NAV_ROWS_TOP + i as f64 * (NAV_ROW_HEIGHT + NAV_ROW_GAP);
            let y1 = y0 + NAV_ROW_HEIGHT;
            let tab = tabs.get(i).copied().unwrap_or_default();
            (tab, Rect::new(inset_x0, y0, inset_x1, y1))
        })
    }

    /// Tab whose nav-row contains `point`, if any.
    pub fn nav_hit_test(&self, modal_rect: Rect, point: Point) -> Option<WsTab> {
        self.nav_rows(modal_rect)
            .into_iter()
            .find_map(|(tab, row)| row.contains(point).then_some(tab))
    }

    /// Paint the sidebar nav (always visible) and dispatch the right
    /// pane to the per-tab drawer. `None` paints nothing on the right.
    pub fn draw_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        pane_drawer: Option<&mut PaneDrawer<'_>>,
    ) {
        self.draw_sidebar(scene, transform, modal_rect, text_renderer, theme);

        if let Some(drawer) = pane_drawer {
            let pane = self.pane_rect(modal_rect);
            drawer(scene, transform, pane, text_renderer, theme);
        }
    }

    /// Paint the sidebar — identity card, nav rows, footer.
    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        modal_rect: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sidebar = self.sidebar_rect(modal_rect);
        if sidebar.is_zero_area() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Sidebar surface reads distinct from the right pane, separated
        // by a hairline rather than a heavy border.
        let bg = colors.neutral[1].with_alpha_factor(0.40);
        scene.fill(Fill::NonZero, transform, bg, None, &sidebar);
        let border_x = sidebar.x1 - 0.5;
        let border = Rect::new(border_x, sidebar.y0, border_x + 1.0, sidebar.y1);
        scene.fill(Fill::NonZero, transform, colors.neutral[3], None, &border);

        // Identity card.
        let identity_size = 56.0_f64;
        let identity_rect = Rect::new(
            sidebar.x0 + 24.0,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP,
            sidebar.x0 + 24.0 + identity_size,
            sidebar.y0 + SIDEBAR_IDENTITY_TOP + identity_size,
        );
        let identity_card = RoundedRect::from_rect(identity_rect, 16.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5],
            None,
            &identity_card,
        );

        text_renderer.draw_text(
            scene,
            "Workspace Settings",
            tokens.typography.scale.xs,
            colors.text_dim,
            at(transform, sidebar.x0 + 24.0, identity_rect.y1 + 12.0),
            None,
        );

        for (tab, row) in self.nav_rows(modal_rect) {
            let active = self.active_tab == tab;
            let row_rr = RoundedRect::from_rect(row, 12.0);
            if active {
                scene.fill(Fill::NonZero, transform, colors.accent[5], None, &row_rr);
            }
            let label_color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            text_renderer.draw_text(
                scene,
                tab.label(),
                tokens.typography.scale.base,
                label_color,
                at(transform, row.x0 + 12.0, row.y0 + 10.0),
                None,
            );
        }

        // Footer "Close Settings" row pinned to the bottom of the sidebar.
        let footer_h = 40.0_f64;
        let footer_y = sidebar.y1 - footer_h - 16.0;
        let footer = Rect::new(
            sidebar.x0 + 16.0,
            footer_y,
            sidebar.x1 - 16.0,
            footer_y + footer_h,
        );
        let footer_rr = RoundedRect::from_rect(footer, 12.0);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &footer_rr,
        );
        let glyph_size = 16.0;
        let glyph_rect = Rect::new(
            footer.x0 + 12.0,
            footer.y0 + 12.0,
            footer.x0 + 12.0 + glyph_size,
            footer.y0 + 12.0 + glyph_size,
        );
        draw_icon(
            scene,
            transform,
            glyph_rect,
            Icon::X,
            colors.text_dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            "Close Settings",
            tokens.typography.scale.sm,
            colors.text_dim,
            at(transform, footer.x0 + 36.0, footer.y0 + 12.0),
            None,
        );
    }
}

/// Stable focus id for the tab button representing `tab`, used by the
/// dialog's `FocusTrap`.
fn tab_focus_id(tab: WsTab) -> u64 {
    // Reserve a small id range so the modal's trap members can't collide
    // with shell-level focusables.
    const BASE: u64 = 0xFFFF_0000;
    match tab {
        WsTab::General => BASE,
        WsTab::Members => BASE + 1,
        WsTab::Permissions => BASE + 2,
        WsTab::Apps => BASE + 3,
    }
}

// Shared chrome — cards, chips, section headers

const PANE_PAD_X: f64 = 16.0;
const PANE_PAD_TOP: f64 = 12.0;
const PANE_INTRO_HEIGHT: f64 = 64.0;
const PANE_INTRO_GAP: f64 = 12.0;
/// Two lines of text plus breathing room.
const ROW_HEIGHT: f64 = 56.0;
const ROW_GAP: f64 = 8.0;
const SECTION_HEADER_HEIGHT: f64 = 26.0;
const CONTROL_HEIGHT: f64 = 30.0;
const COMPOSER_HEIGHT: f64 = 78.0;
const REVOKE_WIDTH: f64 = 80.0;
const REVOKE_HEIGHT: f64 = 26.0;
const SUBMIT_WIDTH: f64 = 92.0;
const EXPIRY_SEGMENT_WIDTH: f64 = 58.0;
/// Alpha for a control the user has no authority to use — matches the
/// permission prompt's dimmed-Revoke treatment.
const DISABLED_ALPHA: f32 = 0.40;

/// Paint a section header — a small upper-case caption plus a count.
fn draw_section_header(
    scene: &mut Scene,
    transform: Affine,
    origin: Point,
    label: &str,
    count: Option<usize>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let text = match count {
        Some(n) => format!("{label} · {n}"),
        None => label.to_string(),
    };
    text_renderer.draw_text(
        scene,
        &text,
        tokens.typography.scale.xs,
        tokens.colors.text_dim,
        at(transform, origin.x, origin.y),
        None,
    );
}

/// Paint a row card — the surface every subject row sits on.
fn draw_row_card(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let card = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.lg));
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &card,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &card,
    );
}

/// A concise explanation and live status at the top of each policy pane.
/// It gives normal users orientation without hiding the full controls.
fn draw_pane_intro(
    scene: &mut Scene,
    transform: Affine,
    pane: Rect,
    icon: Icon,
    title: &str,
    detail: &str,
    status: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let rect = Rect::new(
        pane.x0 + PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP,
        pane.x1 - PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT,
    );
    let surface = RoundedRect::from_rect(rect, f64::from(tokens.radii.lg));
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised,
        None,
        &surface,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.border_standard,
        None,
        &surface,
    );

    let icon_rect = Rect::new(
        rect.x0 + 14.0,
        rect.y0 + 14.0,
        rect.x0 + 50.0,
        rect.y1 - 14.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5].with_alpha_factor(0.16),
        None,
        &RoundedRect::from_rect(icon_rect, f64::from(tokens.radii.md)),
    );
    draw_icon(
        scene,
        transform,
        icon_rect.inset(9.0),
        icon,
        colors.accent[4],
        LUCIDE_DEFAULT_STROKE_PX,
    );
    text_renderer.draw_text(
        scene,
        title,
        tokens.typography.scale.sm,
        colors.text_primary,
        transform * Affine::translate((rect.x0 + 62.0, rect.y0 + 12.0)),
        None,
    );
    let detail_max = (rect.width() - 62.0 - 124.0).max(80.0);
    let shown = elide(
        detail,
        tokens.typography.scale.xs,
        detail_max,
        text_renderer,
    );
    text_renderer.draw_text(
        scene,
        &shown,
        tokens.typography.scale.xs,
        colors.text_dim,
        transform * Affine::translate((rect.x0 + 62.0, rect.y0 + 35.0)),
        None,
    );
    draw_pill(
        scene,
        transform,
        Point::new(rect.x1 - 14.0, rect.center().y),
        status,
        Tint {
            color: colors.accent[5],
            alpha: 1.0,
        },
        text_renderer,
        theme,
    );
}

/// Paint a small pill with `label` centred, tinted `tint`. Returns the
/// pill's rect so callers can lay out rightwards from its left edge.
fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    anchor: Point,
    label: &str,
    tint: Tint,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> Rect {
    let tokens = &theme.tokens;
    let size = tokens.typography.scale.xs;
    let (tw, th) = text_renderer.measure(label, size, None);
    let pad_x = 8.0;
    let pad_y = 4.0;
    let w = f64::from(tw) + pad_x * 2.0;
    let h = f64::from(th) + pad_y * 2.0;
    let rect = Rect::new(
        anchor.x - w,
        anchor.y - h / 2.0,
        anchor.x,
        anchor.y + h / 2.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        tint.color.with_alpha_factor(0.20 * tint.alpha),
        None,
        &RoundedRect::from_rect(rect, f64::from(tokens.radii.md)),
    );
    text_renderer.draw_text(
        scene,
        label,
        size,
        tint.color.with_alpha_factor(tint.alpha),
        at(transform, rect.x0 + pad_x, rect.y0 + pad_y),
        None,
    );
    rect
}

/// Paint an avatar disc carrying `label`'s first character. The tint is
/// derived from the label so the same subject keeps the same colour
/// across frames without storing one.
fn draw_avatar(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    square: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let colors = &theme.tokens.colors;
    let tint = subject_tint(label, theme);
    let radius = if square {
        f64::from(theme.tokens.radii.md)
    } else {
        rect.width() / 2.0
    };
    scene.fill(
        Fill::NonZero,
        transform,
        tint.with_alpha_factor(0.30),
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    let initial: String = label
        .chars()
        .next()
        .map_or_else(|| "?".to_string(), |c| c.to_uppercase().to_string());
    let size = theme.tokens.typography.scale.sm;
    let (tw, th) = text_renderer.measure(&initial, size, None);
    text_renderer.draw_text(
        scene,
        &initial,
        size,
        colors.text_primary,
        at(
            transform,
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
    );
}

/// Deterministic per-subject tint drawn from the theme's accent ramp, so
/// two subjects rarely share a colour and none is hard-coded.
fn subject_tint(label: &str, theme: &Theme) -> Color {
    let colors = &theme.tokens.colors;
    let bucket = label
        .bytes()
        .fold(0u32, |a, b| a.wrapping_add(u32::from(b))) as usize
        % 4;
    match bucket {
        0 => colors.accent[5],
        1 => colors.accent[4],
        2 => colors.semantic.info,
        _ => colors.semantic.success,
    }
}

/// Paint a "cycler" control: a bordered field whose label advances to
/// the next value on click, with a chevron affording that it cycles.
fn draw_cycler(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let alpha = if enabled { 1.0 } else { DISABLED_ALPHA };
    let track = RoundedRect::from_rect(rect, f64::from(tokens.radii.md));
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised.with_alpha_factor(alpha),
        None,
        &track,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.border_standard.with_alpha_factor(alpha),
        None,
        &track,
    );
    let size = tokens.typography.scale.sm;
    let chevron = 14.0_f64;
    let text_max = rect.width() - 12.0 - chevron - 10.0;
    let shown = elide(label, size, text_max, text_renderer);
    let (_, th) = text_renderer.measure(&shown, size, None);
    text_renderer.draw_text(
        scene,
        &shown,
        size,
        colors.text_primary.with_alpha_factor(alpha),
        at(
            transform,
            rect.x0 + 10.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
    );
    let glyph = Rect::new(
        rect.x1 - 8.0 - chevron,
        rect.center().y - chevron / 2.0,
        rect.x1 - 8.0,
        rect.center().y + chevron / 2.0,
    );
    draw_icon(
        scene,
        transform,
        glyph,
        Icon::ChevronDown,
        colors.text_dim.with_alpha_factor(alpha),
        LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Truncate `text` with an ellipsis so it fits `max_w` at `size`. Text
/// that would overflow its field is a craft defect; clipping it is not
/// a fix, so we measure and elide.
fn elide(text: &str, size: f32, max_w: f64, text_renderer: &mut TextRenderer) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = text_renderer.measure(text, size, None);
    if f64::from(w) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "…";
        let (w, _) = text_renderer.measure(&candidate, size, None);
        if f64::from(w) <= max_w {
            return candidate;
        }
    }
    String::new()
}

/// Paint a labelled micro-caption above a control.
fn draw_control_caption(
    scene: &mut Scene,
    transform: Affine,
    origin: Point,
    label: &str,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    text_renderer.draw_text(
        scene,
        label,
        theme.tokens.typography.scale.xs,
        theme.tokens.colors.text_dim,
        at(transform, origin.x, origin.y),
        None,
    );
}

/// Paint a destructive button (Revoke). Dimmed when `enabled` is false
/// so a viewer sees the control exists but cannot be used.
fn draw_danger_button(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let danger = tokens.colors.semantic.danger;
    let alpha = if enabled { 1.0 } else { DISABLED_ALPHA };
    scene.fill(
        Fill::NonZero,
        transform,
        danger.with_alpha_factor(0.20 * alpha),
        None,
        &RoundedRect::from_rect(rect, f64::from(tokens.radii.md)),
    );
    let size = tokens.typography.scale.sm;
    let (tw, th) = text_renderer.measure(label, size, None);
    text_renderer.draw_text(
        scene,
        label,
        size,
        danger.with_alpha_factor(alpha),
        at(
            transform,
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
    );
}

/// Paint the pane's one primary action.
fn draw_primary_button(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    enabled: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let alpha = if enabled { 1.0 } else { DISABLED_ALPHA };
    scene.fill(
        Fill::NonZero,
        transform,
        tokens.colors.accent[5].with_alpha_factor(alpha),
        None,
        &RoundedRect::from_rect(rect, f64::from(tokens.radii.md)),
    );
    let size = tokens.typography.scale.sm;
    let (tw, th) = text_renderer.measure(label, size, None);
    text_renderer.draw_text(
        scene,
        label,
        size,
        tokens.colors.text_primary.with_alpha_factor(alpha),
        at(
            transform,
            rect.center().x - f64::from(tw) / 2.0,
            rect.center().y - f64::from(th) / 2.0,
        ),
        None,
    );
}

/// Paint a composed empty state — glyph, headline, caption.
fn draw_empty_state(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    text: (&str, &str),
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let (headline, caption) = text;
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let glyph = 28.0_f64;
    let cx = rect.center().x;
    let top = rect.y0 + (rect.height() * 0.28).min(96.0);
    draw_icon(
        scene,
        transform,
        Rect::new(cx - glyph / 2.0, top, cx + glyph / 2.0, top + glyph),
        icon,
        colors.text_dim,
        LUCIDE_DEFAULT_STROKE_PX,
    );
    let h_size = tokens.typography.scale.base;
    let (hw, _) = text_renderer.measure(headline, h_size, None);
    text_renderer.draw_text(
        scene,
        headline,
        h_size,
        colors.text_primary,
        at(transform, cx - f64::from(hw) / 2.0, top + glyph + 14.0),
        None,
    );
    let c_size = tokens.typography.scale.sm;
    let (cw, _) = text_renderer.measure(caption, c_size, None);
    text_renderer.draw_text(
        scene,
        caption,
        c_size,
        colors.text_dim,
        at(transform, cx - f64::from(cw) / 2.0, top + glyph + 36.0),
        None,
    );
}

// Capability vocabulary — labels

pub fn role_label(role: CapRole) -> &'static str {
    match role {
        CapRole::Owner => "Owner",
        CapRole::Editor => "Editor",
        CapRole::Commenter => "Commenter",
        CapRole::Viewer => "Viewer",
        CapRole::Denied => "Blocked",
    }
}

/// The thing a device-scoped capability names.
fn device_label(device: &str) -> &str {
    match device {
        "camera" => "the camera",
        "microphone" => "the microphone",
        "screencast" => "screen capture",
        "remote-desktop" => "remote pointer + keyboard",
        other => other,
    }
}

pub fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Workspace => "this workspace".to_string(),
        Scope::Room(_) => "this room".to_string(),
        Scope::App(app) => format!("the {app} app"),
        Scope::FsPath(path) => fs_scope_label(path),
        Scope::Device(device) => device_label(device).to_string(),
        Scope::NetHost(host) if host == "*" => "any network host".to_string(),
        Scope::NetHost(host) => format!("network host {host}"),
        Scope::Action { app, action } => format!("“{action}” in {app}"),
        Scope::ResourceType(kind) => format!("every {}", artifact_kind_label(*kind)),
        Scope::ResourceInstance(id) => format!("one document ({})", short_id(&id.0.to_string())),
    }
}

/// A filesystem scope, phrased the way a person would say it. The
/// [`WORKSPACE_FILES_PATH`] mount point never appears in the label — it's
/// a fact about where the OS keeps the bytes, not what the user is
/// agreeing to (HIG §4).
fn fs_scope_label(path: &str) -> String {
    // Trailing slash: `/var/workspace/` is the root, not an unnamed child.
    let trimmed = path.trim_end_matches('/');
    if trimmed == WORKSPACE_FILES_PATH {
        return "workspace files".to_string();
    }
    match trimmed.strip_prefix(&format!("{WORKSPACE_FILES_PATH}/")) {
        Some(rest) if !rest.is_empty() => format!("\u{201c}{rest}\u{201d} in this workspace"),
        _ => format!("files under {trimmed}"),
    }
}

fn artifact_kind_label(kind: collab_types::role::ArtifactKind) -> &'static str {
    use collab_types::role::ArtifactKind;
    match kind {
        ArtifactKind::Note => "note",
        ArtifactKind::File => "file",
        ArtifactKind::Cluster => "saved cluster",
        ArtifactKind::Kanban => "board",
        ArtifactKind::ChatLog => "chat log",
    }
}

/// Shorten a long opaque identifier for display — person ids are
/// 64-char hex keys, too much to show in full on a row.
pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 14 {
        return id.to_string();
    }
    let head: String = id.chars().take(10).collect();
    format!("{head}…")
}

/// One line describing what a capability allows, built per scope kind
/// rather than concatenated — "verb · scope" would read "View · the
/// microphone" instead of "Use the microphone".
pub fn capability_allows(cap: &CapOverride) -> String {
    let what = scope_label(&cap.scope);
    match (&cap.scope, cap.verb) {
        (Scope::Device(_), Verb::Write | Verb::Admin) => format!("Control {what}"),
        (Scope::Device(_), _) => format!("Use {what}"),
        (Scope::NetHost(_), _) => format!("Reach {what}"),
        (Scope::Action { .. }, _) => format!("Run {what}"),
        (_, Verb::Admin) => format!("Administer {what}"),
        (_, Verb::Write) => format!("Edit {what}"),
        (_, Verb::Comment) => format!("Comment on {what}"),
        (_, Verb::Invite) => format!("Invite others to {what}"),
        (_, Verb::Read) => format!("View {what}"),
    }
}

pub fn expiry_label(expiry: Expiry) -> &'static str {
    match expiry {
        Expiry::AfterUse => "Once",
        Expiry::EndOfSession => "Session",
        Expiry::AtUnix(_) => "Until",
        Expiry::Never => "Always",
    }
}

/// Who granted a capability, as a user-facing phrase, or `None` for a
/// capability with no recorded grantor.
pub fn granted_by_label(cap: &CapOverride) -> Option<String> {
    cap.granted_by.as_ref().map(|p| match p {
        Principal::Person(id) => format!("granted by {}", short_id(id)),
        Principal::App(id) => format!("granted by the {id} app"),
        Principal::Agent(id) => format!("granted by agent {}", short_id(id)),
        Principal::Token(id) => format!("granted by token {}", short_id(id)),
        Principal::Room(id) => format!("granted by room {}", short_id(id)),
    })
}

// Grant composer

/// A subject the grant composer can target, and the subject axis the
/// Permissions list groups by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubjectKind {
    App,
    Member,
    Role,
    /// Agent, bearer token or room — surfaced so no live capability is
    /// invisible in the panel, even though the composer can't mint one.
    Other,
}

impl SubjectKind {
    pub fn section_label(self) -> &'static str {
        match self {
            SubjectKind::App => "Apps",
            SubjectKind::Member => "People",
            SubjectKind::Role => "Roles",
            SubjectKind::Other => "Other",
        }
    }
}

/// A resolved subject: what kind of thing it is and how it is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSubject {
    pub kind: SubjectKind,
    pub id: String,
}

impl GrantSubject {
    pub fn to_subject(&self) -> Option<Subject> {
        match self.kind {
            SubjectKind::App => Some(Subject::app(self.id.clone())),
            SubjectKind::Member => Some(Subject::person(self.id.clone())),
            // A role-wide grant is a policy decision, not a per-workspace click.
            SubjectKind::Role | SubjectKind::Other => None,
        }
    }

    pub fn display(&self) -> String {
        match self.kind {
            SubjectKind::Member => short_id(&self.id),
            _ => self.id.clone(),
        }
    }
}

/// What the composer's scope picker maps to. Kept as a small enum
/// rather than a pre-built `Scope` so the table can stay `const`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantTarget {
    Device(&'static str),
    AnyHost,
    WorkspaceFiles,
    Workspace,
}

/// One entry in the composer's "what does this allow" picker. Every
/// entry produces a capability an enforcement point actually reads
/// (devices gate the portal, `NetHost` gates egress, `FsPath` gates the
/// sandbox binds and the filesystem, `Workspace` gates administration).
pub struct GrantChoice {
    label: &'static str,
    target: GrantTarget,
    verb: Verb,
}

impl GrantChoice {
    pub fn label(&self) -> &'static str {
        self.label
    }
}

static GRANT_CHOICES: &[GrantChoice] = &[
    GrantChoice {
        label: "Camera",
        target: GrantTarget::Device("camera"),
        verb: Verb::Read,
    },
    GrantChoice {
        label: "Microphone",
        target: GrantTarget::Device("microphone"),
        verb: Verb::Read,
    },
    GrantChoice {
        label: "Screen capture",
        target: GrantTarget::Device("screencast"),
        verb: Verb::Read,
    },
    GrantChoice {
        label: "Remote control",
        target: GrantTarget::Device("remote-desktop"),
        verb: Verb::Write,
    },
    GrantChoice {
        label: "Network access",
        target: GrantTarget::AnyHost,
        verb: Verb::Read,
    },
    GrantChoice {
        label: "Read workspace files",
        target: GrantTarget::WorkspaceFiles,
        verb: Verb::Read,
    },
    GrantChoice {
        label: "Edit workspace files",
        target: GrantTarget::WorkspaceFiles,
        verb: Verb::Write,
    },
    GrantChoice {
        label: "Administer workspace",
        target: GrantTarget::Workspace,
        verb: Verb::Admin,
    },
];

/// Fallback for [`GrantDraft::choice`] if the index and [`GRANT_CHOICES`]'s
/// length ever disagree — unreachable in practice since the index is
/// always taken modulo the live length, but a real value beats a panic.
const FALLBACK_CHOICE: GrantChoice = GrantChoice {
    label: "Administer workspace",
    target: GrantTarget::Workspace,
    verb: Verb::Admin,
};

/// The path workspace files are projected at. A capability scoped here
/// is enforced by the sandbox's binds and by the filesystem alike.
const WORKSPACE_FILES_PATH: &str = "/var/workspace";

pub const EXPIRY_CHOICES: [Expiry; 3] = [Expiry::Never, Expiry::EndOfSession, Expiry::AfterUse];

/// The in-flight grant the Permissions tab's composer is composing.
/// Holds indices rather than resolved values so a subject leaving the
/// workspace can't leave a dangling reference — [`grant_subjects`] is
/// re-derived every frame and the index taken modulo its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantDraft {
    pub subject_index: usize,
    pub choice_index: usize,
    pub expiry: Expiry,
}

impl Default for GrantDraft {
    fn default() -> Self {
        Self {
            subject_index: 0,
            choice_index: 0,
            expiry: Expiry::EndOfSession,
        }
    }
}

impl GrantDraft {
    /// Advances to the next subject, wrapping at `len`. `len == 0` (an
    /// empty subject list) lands back on index 0 via `checked_rem`'s
    /// `None` fallback, same as the guard this replaced.
    pub fn cycle_subject(&mut self, len: usize) {
        self.subject_index = self
            .subject_index
            .wrapping_add(1)
            .checked_rem(len)
            .unwrap_or(0);
    }

    pub fn cycle_choice(&mut self) {
        self.choice_index = self
            .choice_index
            .wrapping_add(1)
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
    }

    pub fn choice(&self) -> &'static GrantChoice {
        let idx = self
            .choice_index
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
        GRANT_CHOICES.get(idx).unwrap_or(&FALLBACK_CHOICE)
    }
}

/// The subjects the composer can target, in picker order: the
/// workspace's people first, then every app running in the workspace or
/// already holding a capability — without running apps, a host could
/// only grant to an app already granted something.
pub fn grant_subjects(ws: &Workspace) -> Vec<GrantSubject> {
    let mut out: Vec<GrantSubject> = ws
        .roster()
        .into_iter()
        .map(|(user, _)| GrantSubject {
            kind: SubjectKind::Member,
            id: user,
        })
        .collect();

    let mut apps: Vec<String> = Vec::new();
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                apps.push(tile_app_id(&tab.tile));
            }
        }
    }
    for cap in &ws.capabilities {
        if let Subject::Principal(Principal::App(app)) = &cap.subject {
            apps.push(app.clone());
        }
    }
    apps.sort();
    apps.dedup();
    out.extend(apps.into_iter().map(|id| GrantSubject {
        kind: SubjectKind::App,
        id,
    }));
    out
}

/// The app id a tile belongs to, for subject discovery. `TileContent` has
/// no variant without an app identity, so this never needs `Option`.
fn tile_app_id(tile: &crate::shell::state::TileContent) -> String {
    use crate::shell::state::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. } | TileContent::PeerWindow { app_id, .. } => {
            app_id.clone()
        }
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            kind.as_str().to_string()
        }
    }
}

/// Build the capability `draft` currently names against `ws`, or `None`
/// when the draft has no addressable subject (an empty workspace) or
/// names a subject this panel cannot mint for.
pub fn draft_capability(ws: &Workspace, draft: &GrantDraft) -> Option<CapOverride> {
    let subjects = grant_subjects(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    let subject_entry = subjects.get(idx)?;
    let subject = subject_entry.to_subject()?;
    let choice = draft.choice();
    let scope = match choice.target {
        GrantTarget::Device(d) => Scope::Device(d.to_string()),
        GrantTarget::AnyHost => Scope::NetHost("*".to_string()),
        GrantTarget::WorkspaceFiles => Scope::FsPath(WORKSPACE_FILES_PATH.to_string()),
        GrantTarget::Workspace => Scope::Workspace,
    };
    Some(CapOverride::grant(subject, scope, choice.verb).expiring(draft.expiry))
}

/// The subject a draft currently names, for rendering the picker.
pub fn draft_subject(ws: &Workspace, draft: &GrantDraft) -> Option<GrantSubject> {
    let subjects = grant_subjects(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    subjects.get(idx).cloned()
}

// Authority-checked mutations

/// What a panel mutation changed, so the caller can mirror it into the
/// room document and to disk. Empty means either the actor lacked
/// authority, or there was nothing to change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapEdit {
    pub revoked: Vec<String>,
    pub granted: Vec<String>,
}

impl CapEdit {
    pub fn is_empty(&self) -> bool {
        self.revoked.is_empty() && self.granted.is_empty()
    }
}

/// Grant the capability `draft` names, as `actor`. Routes through
/// [`Workspace::grant_as`], the same authority check the portal and
/// replica validator use — not a UI-side copy of it.
pub fn apply_grant_draft(ws: &mut Workspace, actor: &UserId, draft: &GrantDraft) -> CapEdit {
    let Some(cap) = draft_capability(ws, draft) else {
        return CapEdit::default();
    };
    match ws.grant_as(actor, cap) {
        Some(id) => CapEdit {
            revoked: Vec::new(),
            granted: vec![id],
        },
        None => CapEdit::default(),
    }
}

/// Revoke the capability with id `cap_id`, as `actor`. Routes through
/// [`Workspace::revoke_as`].
pub fn apply_revoke(ws: &mut Workspace, actor: &UserId, cap_id: &str) -> CapEdit {
    if ws.revoke_as(actor, cap_id) {
        CapEdit {
            revoked: vec![cap_id.to_string()],
            granted: Vec::new(),
        }
    } else {
        CapEdit::default()
    }
}

/// The roles a member's role chip cycles through. `Denied` is not in
/// the cycle — that's what Revoke expresses, not an accidental click.
const ROLE_CYCLE: [CapRole; 4] = [
    CapRole::Viewer,
    CapRole::Commenter,
    CapRole::Editor,
    CapRole::Owner,
];

/// Inverse of the mapping [`Workspace::role_of`] reads.
fn role_verb(role: CapRole) -> Option<Verb> {
    match role {
        CapRole::Owner => Some(Verb::Admin),
        CapRole::Editor => Some(Verb::Write),
        CapRole::Commenter => Some(Verb::Comment),
        CapRole::Viewer => Some(Verb::Read),
        CapRole::Denied => None,
    }
}

pub fn next_role(role: CapRole) -> CapRole {
    let idx = ROLE_CYCLE.iter().position(|r| *r == role).unwrap_or(0);
    let next = idx
        .wrapping_add(1)
        .checked_rem(ROLE_CYCLE.len())
        .unwrap_or(0);
    ROLE_CYCLE.get(next).copied().unwrap_or(CapRole::Viewer)
}

fn membership_ids(ws: &Workspace, member: &UserId) -> Vec<String> {
    ws.capabilities
        .iter()
        .filter(|cap| cap.is_room_membership() && subject_is_person(&cap.subject, member))
        .map(|cap| cap.id.clone())
        .collect()
}

fn subject_is_person(subject: &Subject, user: &UserId) -> bool {
    match subject {
        Subject::Principal(Principal::Person(id)) => id == user,
        Subject::User(key) => Principal::person_from_key(*key).id() == user.as_str(),
        _ => false,
    }
}

/// Move `member` to `role`, as `actor`. Lowering a role retires the
/// existing membership capabilities first — grants keep the strongest
/// of two over the same scope.
pub fn set_member_role(
    ws: &mut Workspace,
    actor: &UserId,
    member: &UserId,
    role: CapRole,
) -> CapEdit {
    let Some(verb) = role_verb(role) else {
        return CapEdit::default();
    };
    if !ws.may_administer(actor) {
        return CapEdit::default();
    }
    // The owner's role is structural, not a capability — nothing to rewrite.
    if ws.owner.as_deref() == Some(member.as_str()) {
        return CapEdit::default();
    }
    let mut edit = CapEdit::default();
    for id in membership_ids(ws, member) {
        if ws.revoke_as(actor, &id) {
            edit.revoked.push(id);
        }
    }
    let cap = CapOverride::membership(
        Subject::person(member.clone()),
        ws.id.raw().to_string(),
        verb,
    );
    if let Some(id) = ws.grant_as(actor, cap) {
        edit.granted.push(id);
    }
    edit
}

/// Remove `member` and every capability they hold, as `actor`.
pub fn remove_member(ws: &mut Workspace, actor: &UserId, member: &UserId) -> CapEdit {
    // The owner is the workspace's root of trust; removing them would
    // leave a room no one can administer.
    if ws.owner.as_deref() == Some(member.as_str()) {
        return CapEdit::default();
    }
    let ids: Vec<String> = ws
        .capabilities
        .iter()
        .filter(|cap| subject_is_person(&cap.subject, member))
        .map(|cap| cap.id.clone())
        .collect();
    let mut edit = CapEdit::default();
    for id in ids {
        if ws.revoke_as(actor, &id) {
            edit.revoked.push(id);
        }
    }
    // Off the roster too, but only if a revoke actually happened.
    if !edit.revoked.is_empty() {
        ws.collaborators.retain(|c| c.id != member.as_str());
    }
    edit
}

// Members pane

/// One row in the Members pane, plus the geometry the renderer painted
/// so click-routing matches a point back to a member without
/// re-deriving layout.
#[derive(Clone, Debug)]
pub struct MemberRow {
    pub user: UserId,
    pub role: CapRole,
    /// The owner's role is structural, so their role chip is inert.
    pub is_workspace_owner: bool,
    pub grant_count: usize,
    pub row_rect: Rect,
    pub role_rect: Rect,
    pub remove_rect: Rect,
}

/// The roster in a stable display order: owner first, then everyone
/// else by id. [`Workspace::roster`]'s own order follows list position,
/// which moves a row when a role change rewrites its capability.
fn ordered_roster(ws: &Workspace) -> Vec<(UserId, CapRole)> {
    let mut rows = ws.roster();
    let owner = ws.owner.clone();
    rows.sort_by(|a, b| {
        let a_owner = Some(&a.0) == owner.as_ref();
        let b_owner = Some(&b.0) == owner.as_ref();
        b_owner.cmp(&a_owner).then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// Pure geometry; feeds both the renderer and the click router.
pub fn members_pane_rows(ws: &Workspace, pane_rect: Rect) -> Vec<MemberRow> {
    let mut rows = Vec::new();
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return rows;
    }
    let mut cursor_y =
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + SECTION_HEADER_HEIGHT;
    for (user, role) in ordered_roster(ws) {
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            cursor_y,
            pane_rect.x1 - PANE_PAD_X,
            cursor_y + ROW_HEIGHT,
        );
        if row_rect.y1 > pane_rect.y1 - PANE_PAD_TOP {
            break;
        }
        let remove_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let role_w = 96.0_f64;
        let role_rect = Rect::new(
            remove_rect.x0 - 12.0 - role_w,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            remove_rect.x0 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let is_workspace_owner = ws.owner.as_deref() == Some(user.as_str());
        let grant_count = ws
            .capabilities
            .iter()
            .filter(|cap| {
                cap.effect == Effect::Grant
                    && !cap.is_room_membership()
                    && subject_is_person(&cap.subject, &user)
            })
            .count();
        rows.push(MemberRow {
            user,
            role,
            is_workspace_owner,
            grant_count,
            row_rect,
            role_rect,
            remove_rect,
        });
        cursor_y += ROW_HEIGHT + ROW_GAP;
    }
    rows
}

/// `is_owner` is the *local* user's authority — a non-owner sees every
/// row and control, disabled.
#[allow(
    clippy::too_many_arguments,
    reason = "called from shell::overlay_host, outside this change's scope; bundling params \
              into a struct would require updating that caller too"
)]
pub fn draw_members_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    ws: &Workspace,
    local_user: &UserId,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let rows = members_pane_rows(ws, pane_rect);

    let roster_count = ordered_roster(ws).len();
    draw_pane_intro(
        scene,
        transform,
        pane_rect,
        Icon::Home,
        &format!("{roster_count} people share this workspace"),
        "Roles set the baseline; specific exceptions remain visible in Privacy & Security.",
        if is_owner { "You manage" } else { "View only" },
        text_renderer,
        theme,
    );

    if rows.is_empty() {
        let empty = Rect::new(
            pane_rect.x0,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
            pane_rect.x1,
            pane_rect.y1,
        );
        draw_empty_state(
            scene,
            transform,
            empty,
            Icon::Shield,
            (
                "No one else is in this workspace",
                "Share the invite link from the General tab to add someone.",
            ),
            text_renderer,
            theme,
        );
        return;
    }

    draw_section_header(
        scene,
        transform,
        Point::new(
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        ),
        "MEMBERS",
        Some(ordered_roster(ws).len()),
        text_renderer,
        theme,
    );

    for row in &rows {
        draw_member_row(
            scene,
            transform,
            row,
            local_user,
            is_owner,
            text_renderer,
            theme,
        );
    }

    if !is_owner {
        draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
    }
}

/// Paint one [`MemberRow`] — avatar, name, role chip, Remove button.
fn draw_member_row(
    scene: &mut Scene,
    transform: Affine,
    row: &MemberRow,
    local_user: &UserId,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    draw_row_card(scene, transform, row.row_rect, theme);

    let avatar_size = 28.0_f64;
    let avatar = Rect::new(
        row.row_rect.x0 + 12.0,
        row.row_rect.center().y - avatar_size / 2.0,
        row.row_rect.x0 + 12.0 + avatar_size,
        row.row_rect.center().y + avatar_size / 2.0,
    );
    let name = short_id(&row.user);
    draw_avatar(scene, transform, avatar, &name, false, text_renderer, theme);

    // Person ids are hex keys; a name alone doesn't say which row is you.
    let mut text_limit = row.role_rect.x0 - 12.0;
    if &row.user == local_user {
        let you = draw_pill(
            scene,
            transform,
            Point::new(text_limit, row.row_rect.center().y),
            "You",
            Tint {
                color: colors.accent[5],
                alpha: 1.0,
            },
            text_renderer,
            theme,
        );
        text_limit = you.x0 - 8.0;
    }

    let text_x = avatar.x1 + 12.0;
    let text_max = text_limit - text_x;
    let title_size = tokens.typography.scale.sm;
    let title_text = elide(&name, title_size, text_max, text_renderer);
    text_renderer.draw_text(
        scene,
        &title_text,
        title_size,
        colors.text_primary,
        at(transform, text_x, row.row_rect.y0 + 12.0),
        None,
    );

    let detail = match row.grant_count {
        0 => format!("{} · no extra permissions", role_label(row.role)),
        1 => format!("{} · 1 extra permission", role_label(row.role)),
        n => format!("{} · {n} extra permissions", role_label(row.role)),
    };
    let detail_size = tokens.typography.scale.xs;
    let detail_text = elide(&detail, detail_size, text_max, text_renderer);
    text_renderer.draw_text(
        scene,
        &detail_text,
        detail_size,
        colors.text_dim,
        at(transform, text_x, row.row_rect.y0 + 32.0),
        None,
    );

    draw_member_role_chip(
        scene,
        transform,
        row,
        is_owner,
        detail_size,
        text_renderer,
        theme,
    );

    draw_danger_button(
        scene,
        transform,
        row.remove_rect,
        "Remove",
        is_owner && !row.is_workspace_owner,
        text_renderer,
        theme,
    );
}

/// The role chip half of [`draw_member_row`] — its fill, label and the
/// chevron affording that owners can cycle it.
fn draw_member_role_chip(
    scene: &mut Scene,
    transform: Affine,
    row: &MemberRow,
    is_owner: bool,
    detail_size: f32,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let role_interactive = is_owner && !row.is_workspace_owner;
    let chip_tint = if row.role == CapRole::Denied {
        colors.semantic.danger
    } else if row.is_workspace_owner {
        colors.semantic.success
    } else {
        colors.accent[5]
    };
    let chip_alpha = if role_interactive || row.is_workspace_owner {
        1.0
    } else {
        DISABLED_ALPHA
    };
    scene.fill(
        Fill::NonZero,
        transform,
        chip_tint.with_alpha_factor(0.20 * chip_alpha),
        None,
        &RoundedRect::from_rect(row.role_rect, f64::from(tokens.radii.md)),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        chip_tint.with_alpha_factor(if role_interactive { 0.60 } else { 0.35 }),
        None,
        &RoundedRect::from_rect(row.role_rect, f64::from(tokens.radii.md)),
    );
    let role_text = role_label(row.role);
    let (rw, rh) = text_renderer.measure(role_text, detail_size, None);
    // text_primary, not the chip's own hue: same-hue label on fill is unreadable.
    text_renderer.draw_text(
        scene,
        role_text,
        detail_size,
        colors.text_primary.with_alpha_factor(chip_alpha),
        at(
            transform,
            row.role_rect.x0 + 10.0,
            row.role_rect.center().y - f64::from(rh) / 2.0,
        ),
        None,
    );
    if role_interactive {
        let glyph = 12.0_f64;
        let gx =
            (row.role_rect.x0 + 10.0 + f64::from(rw) + 6.0).min(row.role_rect.x1 - 6.0 - glyph);
        draw_icon(
            scene,
            transform,
            Rect::new(
                gx,
                row.role_rect.center().y - glyph / 2.0,
                gx + glyph,
                row.role_rect.center().y + glyph / 2.0,
            ),
            Icon::ChevronRight,
            chip_tint,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

#[derive(Clone, Debug)]
pub struct DefaultAppRow {
    pub resource: desktop_policy::ResourceKind,
    pub workspace_rect: Rect,
    pub user_rect: Rect,
    pub row_rect: Rect,
}

pub fn default_app_rows(
    pane: Rect,
    candidates: &[desktop_policy::AppCandidate],
) -> Vec<DefaultAppRow> {
    let mut resources: Vec<_> = candidates
        .iter()
        .flat_map(|a| a.resources.iter().cloned())
        .collect();
    resources.sort();
    resources.dedup();
    let x0 = pane.x0 + PANE_PAD_X;
    let x1 = pane.x1 - PANE_PAD_X;
    let control_w = 190.0;
    resources
        .into_iter()
        .enumerate()
        .filter_map(|(index, resource)| {
            // `index` is a row position in a pane-height-bounded list of
            // deduplicated resource kinds — well under 2^53.
            #[allow(
                clippy::cast_precision_loss,
                reason = "index is a small row position bounded by the resource-kind list, well under 2^53"
            )]
            let y0 = pane.y0
                + PANE_PAD_TOP
                + PANE_INTRO_HEIGHT
                + PANE_INTRO_GAP
                + 44.0
                + index as f64 * 72.0;
            let row_rect = Rect::new(x0, y0, x1, y0 + 64.0);
            (row_rect.y1 <= pane.y1 - 10.0).then(|| DefaultAppRow {
                resource,
                workspace_rect: Rect::new(
                    x1 - control_w * 2.0 - 12.0,
                    y0 + 26.0,
                    x1 - control_w - 12.0,
                    y0 + 56.0,
                ),
                user_rect: Rect::new(x1 - control_w, y0 + 26.0, x1, y0 + 56.0),
                row_rect,
            })
        })
        .collect()
}

fn app_name<'a>(id: &str, candidates: &'a [desktop_policy::AppCandidate]) -> &'a str {
    candidates
        .iter()
        .find(|a| a.package_id == id)
        .map_or("Unavailable app", |a| a.display_name.as_str())
}

/// Bundles the per-workspace inputs to [`draw_default_apps_pane`] so the
/// call stays under clippy's argument-count budget, without disturbing
/// the shared `scene`/`transform`/`text_renderer`/`theme` draw parameters
/// every other pane drawer in this file takes in that same order.
pub struct DefaultAppsCtx<'a> {
    pub ws: &'a Workspace,
    pub local_user: &'a UserId,
    pub candidates: &'a [desktop_policy::AppCandidate],
}

pub fn draw_default_apps_pane(
    scene: &mut Scene,
    transform: Affine,
    pane: Rect,
    ctx: DefaultAppsCtx<'_>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let DefaultAppsCtx {
        ws,
        local_user,
        candidates,
    } = ctx;
    let resources = candidates
        .iter()
        .flat_map(|candidate| candidate.resources.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let custom_rules = ws.default_apps.policy.len();
    let status = if custom_rules == 0 {
        format!("{resources} file types")
    } else {
        format!("{custom_rules} custom rules")
    };
    draw_pane_intro(
        scene,
        transform,
        pane,
        Icon::LayoutGrid,
        "Defaults without lock-in",
        "The workspace supplies a baseline; each person can explicitly choose something else.",
        &status,
        text_renderer,
        theme,
    );
    let heading_y = pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP;
    draw_section_header(
        scene,
        transform,
        Point::new(pane.x0 + PANE_PAD_X, heading_y),
        "DEFAULT APPLICATIONS",
        None,
        text_renderer,
        theme,
    );
    text_renderer.draw_text(
        scene,
        "Workspace choice",
        theme.tokens.typography.scale.xs,
        theme.tokens.colors.text_dim,
        at(transform, pane.x1 - 404.0, heading_y + 22.0),
        None,
    );
    text_renderer.draw_text(
        scene,
        "Your choice",
        theme.tokens.typography.scale.xs,
        theme.tokens.colors.text_dim,
        at(transform, pane.x1 - 194.0, heading_y + 22.0),
        None,
    );
    let can_admin = ws.may_administer(local_user);
    for row in default_app_rows(pane, candidates) {
        draw_row_card(scene, transform, row.row_rect, theme);
        let label = row.resource.display_label();
        text_renderer.draw_text(
            scene,
            &label,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            at(transform, row.row_rect.x0 + 12.0, row.row_rect.y0 + 12.0),
            None,
        );
        let policy = ws
            .default_apps
            .policy
            .get(&row.resource)
            .unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal);
        let policy_label = match policy {
            desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => "Follow personal defaults",
            desktop_policy::WorkspaceDefaultPolicy::App(id) => app_name(id, candidates),
        };
        let choice = ws
            .default_apps
            .user_choices
            .get(local_user)
            .and_then(|m| m.get(&row.resource))
            .unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace);
        let choice_label = match choice {
            desktop_policy::UserWorkspaceChoice::FollowWorkspace => "Use workspace choice",
            desktop_policy::UserWorkspaceChoice::PersonalDefault => "Use my personal default",
            desktop_policy::UserWorkspaceChoice::App(id) => app_name(id, candidates),
        };
        draw_cycler(
            scene,
            transform,
            row.workspace_rect,
            policy_label,
            can_admin,
            text_renderer,
            theme,
        );
        draw_cycler(
            scene,
            transform,
            row.user_rect,
            choice_label,
            true,
            text_renderer,
            theme,
        );
    }
    if candidates.iter().all(|a| a.resources.is_empty()) {
        draw_empty_state(
            scene,
            transform,
            pane,
            Icon::LayoutGrid,
            (
                "No file handlers installed",
                "Apps appear here after they declare supported file types.",
            ),
            text_renderer,
            theme,
        );
    }
}

pub fn next_workspace_default(
    current: Option<&desktop_policy::WorkspaceDefaultPolicy>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::WorkspaceDefaultPolicy {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal) {
        desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => apps.first().map_or(
            desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
            |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
        ),
        desktop_policy::WorkspaceDefaultPolicy::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(
                desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
                |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
            ),
    }
}

pub fn next_user_default(
    current: Option<&desktop_policy::UserWorkspaceChoice>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::UserWorkspaceChoice {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace) {
        desktop_policy::UserWorkspaceChoice::FollowWorkspace => {
            desktop_policy::UserWorkspaceChoice::PersonalDefault
        }
        desktop_policy::UserWorkspaceChoice::PersonalDefault => apps
            .first()
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
        desktop_policy::UserWorkspaceChoice::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
    }
}

/// A disabled control with no reason reads as a broken one.
fn draw_authority_footnote(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let note = "Only the workspace owner can change permissions.";
    let size = tokens.typography.scale.xs;
    text_renderer.draw_text(
        scene,
        note,
        size,
        tokens.colors.text_dim,
        at(
            transform,
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y1 - PANE_PAD_TOP - 14.0,
        ),
        None,
    );
}

// Permissions pane

/// One capability row, keyed by the capability's stable id — the same
/// key [`Workspace::revoke_as`] takes, so a click can never revoke a
/// different capability than the one the row was painted for.
#[derive(Clone, Debug)]
pub struct PermissionRow {
    pub cap_id: String,
    pub subject: GrantSubject,
    pub allows: String,
    pub expiry: &'static str,
    pub granted_by: Option<String>,
    pub row_rect: Rect,
    pub revoke_rect: Rect,
}

/// Pure layout — the renderer and the click router both read it, so a
/// click always lands on the rect that was painted.
#[derive(Clone, Debug, Default)]
pub struct PermissionsPaneLayout {
    /// Zero-area when the workspace has no addressable subject.
    pub subject_rect: Rect,
    pub choice_rect: Rect,
    /// In [`EXPIRY_CHOICES`] order.
    pub expiry_rects: [Rect; 3],
    pub submit_rect: Rect,
    pub rows: Vec<PermissionRow>,
    /// Capabilities that did not fit the pane.
    pub overflow: usize,
}

fn classify_subject(subject: &Subject) -> GrantSubject {
    match subject {
        Subject::Principal(Principal::App(id)) => GrantSubject {
            kind: SubjectKind::App,
            id: id.clone(),
        },
        Subject::Principal(Principal::Person(id)) => GrantSubject {
            kind: SubjectKind::Member,
            id: id.clone(),
        },
        Subject::User(key) => GrantSubject {
            kind: SubjectKind::Member,
            id: Principal::person_from_key(*key).id().to_string(),
        },
        Subject::Role(role) => GrantSubject {
            kind: SubjectKind::Role,
            id: format!("everyone with {}", role_label(*role)),
        },
        Subject::Principal(Principal::Agent(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: format!("agent {}", short_id(id)),
        },
        Subject::Principal(Principal::Token(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: format!("token {}", short_id(id)),
        },
        Subject::Principal(Principal::Room(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: format!("room {}", short_id(id)),
        },
    }
}

/// The capabilities the Permissions pane lists, grouped by subject
/// kind, then subject, then capability id. A *person's* room-membership
/// capability is excluded — the Members tab renders that as a role —
/// but a room/workspace-scoped grant held by an app or agent stays
/// listed here, or it would be revocable from neither tab.
fn listed_capabilities(ws: &Workspace) -> Vec<(GrantSubject, &CapOverride)> {
    let mut out: Vec<(GrantSubject, &CapOverride)> = ws
        .capabilities
        .iter()
        .filter(|cap| {
            cap.effect == Effect::Grant && !(cap.is_room_membership() && cap.subject_is_person())
        })
        .map(|cap| (classify_subject(&cap.subject), cap))
        .collect();
    out.sort_by(|a, b| {
        a.0.kind
            .cmp(&b.0.kind)
            .then_with(|| a.0.id.cmp(&b.0.id))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    out
}

/// Geometry does not depend on the draft, so the click router can
/// resolve a point without knowing it.
pub fn permissions_pane_layout(ws: &Workspace, pane_rect: Rect) -> PermissionsPaneLayout {
    let mut layout = PermissionsPaneLayout::default();
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return layout;
    }

    // ---- Composer -------------------------------------------------
    let card = Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + SECTION_HEADER_HEIGHT,
        pane_rect.x1 - PANE_PAD_X,
        pane_rect.y0
            + PANE_PAD_TOP
            + PANE_INTRO_HEIGHT
            + PANE_INTRO_GAP
            + SECTION_HEADER_HEIGHT
            + COMPOSER_HEIGHT,
    );
    let inner_x0 = card.x0 + 12.0;
    let inner_x1 = card.x1 - 12.0;
    let controls_y = card.y1 - 12.0 - CONTROL_HEIGHT;
    let gap = 8.0_f64;
    let expiry_w = EXPIRY_SEGMENT_WIDTH * 3.0;
    let flexible = (inner_x1 - inner_x0) - expiry_w - SUBMIT_WIDTH - gap * 3.0;
    let subject_w = (flexible * 0.45).max(72.0);
    let choice_w = (flexible - subject_w).max(72.0);

    layout.subject_rect = Rect::new(
        inner_x0,
        controls_y,
        inner_x0 + subject_w,
        controls_y + CONTROL_HEIGHT,
    );
    layout.choice_rect = Rect::new(
        layout.subject_rect.x1 + gap,
        controls_y,
        layout.subject_rect.x1 + gap + choice_w,
        controls_y + CONTROL_HEIGHT,
    );
    let expiry_x0 = layout.choice_rect.x1 + gap;
    layout.expiry_rects = std::array::from_fn(|i| {
        // `i` is a `std::array::from_fn::<_, 3>` index, always 0..=2.
        #[allow(
            clippy::cast_precision_loss,
            reason = "i is a fixed 3-element array index, always 0..=2"
        )]
        let x0 = expiry_x0 + i as f64 * EXPIRY_SEGMENT_WIDTH;
        Rect::new(
            x0,
            controls_y,
            x0 + EXPIRY_SEGMENT_WIDTH,
            controls_y + CONTROL_HEIGHT,
        )
    });
    layout.submit_rect = Rect::new(
        inner_x1 - SUBMIT_WIDTH,
        controls_y,
        inner_x1,
        controls_y + CONTROL_HEIGHT,
    );
    if grant_subjects(ws).is_empty() {
        layout.subject_rect = Rect::ZERO;
        layout.submit_rect = Rect::ZERO;
    }

    // ---- Capability rows ------------------------------------------
    let entries = listed_capabilities(ws);
    let mut cursor_y = card.y1 + 16.0;
    let mut last_kind: Option<SubjectKind> = None;
    let mut placed = 0usize;
    for (subject, cap) in &entries {
        let needs_header = last_kind != Some(subject.kind);
        let header_h = if needs_header {
            SECTION_HEADER_HEIGHT
        } else {
            0.0
        };
        let row_y0 = cursor_y + header_h;
        if row_y0 + ROW_HEIGHT > pane_rect.y1 - PANE_PAD_TOP {
            break;
        }
        let row_rect = Rect::new(
            pane_rect.x0 + PANE_PAD_X,
            row_y0,
            pane_rect.x1 - PANE_PAD_X,
            row_y0 + ROW_HEIGHT,
        );
        let revoke_rect = Rect::new(
            row_rect.x1 - 12.0 - REVOKE_WIDTH,
            row_rect.center().y - REVOKE_HEIGHT / 2.0,
            row_rect.x1 - 12.0,
            row_rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        layout.rows.push(PermissionRow {
            cap_id: cap.id.clone(),
            subject: subject.clone(),
            allows: capability_allows(cap),
            expiry: expiry_label(cap.expiry),
            granted_by: granted_by_label(cap),
            row_rect,
            revoke_rect,
        });
        last_kind = Some(subject.kind);
        cursor_y = row_rect.y1 + ROW_GAP;
        placed = placed.saturating_add(1);
    }
    layout.overflow = entries.len().saturating_sub(placed);
    layout
}

/// Non-owners see the same list and controls, painted disabled, with a
/// footnote explaining why.
#[allow(
    clippy::too_many_arguments,
    reason = "called from shell::overlay_host, outside this change's scope; bundling params \
              into a struct would require updating that caller too"
)]
pub fn draw_permissions_pane(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    ws: &Workspace,
    draft: &GrantDraft,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    if pane_rect.width() <= 0.0 || pane_rect.height() <= 0.0 {
        return;
    }
    let layout = permissions_pane_layout(ws, pane_rect);
    let active_grants = listed_capabilities(ws).len();
    draw_pane_intro(
        scene,
        transform,
        pane_rect,
        Icon::Shield,
        "Access is explicit and revocable",
        "Grants are enforced by the app sandbox and stay scoped to this workspace.",
        &format!("{active_grants} active grants"),
        text_renderer,
        theme,
    );

    let card = draw_permission_composer(
        scene,
        transform,
        pane_rect,
        (ws, draft),
        is_owner,
        &layout,
        (&mut *text_renderer, theme),
    );

    // ---- Capability rows ------------------------------------------
    if layout.rows.is_empty() {
        let body = Rect::new(pane_rect.x0, card.y1, pane_rect.x1, pane_rect.y1);
        draw_empty_state(
            scene,
            transform,
            body,
            Icon::Shield,
            (
                "No permissions granted",
                "Apps get nothing until it is granted here or approved in a prompt.",
            ),
            text_renderer,
            theme,
        );
        if !is_owner {
            draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
        }
        return;
    }

    let mut last_kind: Option<SubjectKind> = None;
    let group_counts = |kind: SubjectKind| -> usize {
        layout
            .rows
            .iter()
            .filter(|r| r.subject.kind == kind)
            .count()
    };
    for row in &layout.rows {
        if last_kind != Some(row.subject.kind) {
            draw_section_header(
                scene,
                transform,
                Point::new(
                    row.row_rect.x0,
                    row.row_rect.y0 - SECTION_HEADER_HEIGHT + 6.0,
                ),
                row.subject.kind.section_label(),
                Some(group_counts(row.subject.kind)),
                text_renderer,
                theme,
            );
            last_kind = Some(row.subject.kind);
        }
        draw_permission_row(scene, transform, row, is_owner, text_renderer, theme);
    }

    if layout.overflow > 0 {
        let more = format!("+{} more", layout.overflow);
        text_renderer.draw_text(
            scene,
            &more,
            theme.tokens.typography.scale.xs,
            theme.tokens.colors.text_dim,
            at(
                transform,
                pane_rect.x0 + PANE_PAD_X,
                pane_rect.y1 - PANE_PAD_TOP - 14.0,
            ),
            None,
        );
    } else if !is_owner {
        draw_authority_footnote(scene, transform, pane_rect, text_renderer, theme);
    }
}

/// The "Who" and "What it allows" cyclers inside the composer card.
fn draw_who_what_pickers(
    scene: &mut Scene,
    transform: Affine,
    card: Rect,
    subject_draft: (&Workspace, &GrantDraft),
    is_owner: bool,
    layout: &PermissionsPaneLayout,
    render: (&mut TextRenderer, &Theme),
) {
    let (ws, draft) = subject_draft;
    let (text_renderer, theme) = render;
    let caption_y = card.y0 + 12.0;
    let subject = draft_subject(ws, draft);
    match &subject {
        Some(entry) => {
            draw_control_caption(
                scene,
                transform,
                Point::new(layout.subject_rect.x0 + 2.0, caption_y),
                "Who",
                text_renderer,
                theme,
            );
            draw_cycler(
                scene,
                transform,
                layout.subject_rect,
                &entry.display(),
                is_owner,
                text_renderer,
                theme,
            );
        }
        None => {
            draw_control_caption(
                scene,
                transform,
                Point::new(card.x0 + 12.0, caption_y),
                "Nobody to grant to yet — invite someone, or open an app here.",
                text_renderer,
                theme,
            );
        }
    }
    draw_control_caption(
        scene,
        transform,
        Point::new(layout.choice_rect.x0 + 2.0, caption_y),
        "What it allows",
        text_renderer,
        theme,
    );
    draw_cycler(
        scene,
        transform,
        layout.choice_rect,
        draft.choice().label,
        is_owner,
        text_renderer,
        theme,
    );
}

/// Paint the "Grant a permission" composer card — header, the who/what
/// cyclers, the expiry track, the submit button. Returns the card's rect
/// so the caller can lay out the capability rows beneath it.
fn draw_permission_composer(
    scene: &mut Scene,
    transform: Affine,
    pane_rect: Rect,
    subject_draft: (&Workspace, &GrantDraft),
    is_owner: bool,
    layout: &PermissionsPaneLayout,
    render: (&mut TextRenderer, &Theme),
) -> Rect {
    let draft = subject_draft.1;
    let (text_renderer, theme) = render;
    let tokens = &theme.tokens;
    let colors = &tokens.colors;

    draw_section_header(
        scene,
        transform,
        Point::new(
            pane_rect.x0 + PANE_PAD_X,
            pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP,
        ),
        "GRANT A PERMISSION",
        None,
        text_renderer,
        theme,
    );
    let card = Rect::new(
        pane_rect.x0 + PANE_PAD_X,
        pane_rect.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT + PANE_INTRO_GAP + SECTION_HEADER_HEIGHT,
        pane_rect.x1 - PANE_PAD_X,
        pane_rect.y0
            + PANE_PAD_TOP
            + PANE_INTRO_HEIGHT
            + PANE_INTRO_GAP
            + SECTION_HEADER_HEIGHT
            + COMPOSER_HEIGHT,
    );
    let card_rr = RoundedRect::from_rect(card, f64::from(tokens.radii.lg));
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised,
        None,
        &card_rr,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.border_standard,
        None,
        &card_rr,
    );

    draw_who_what_pickers(
        scene,
        transform,
        card,
        subject_draft,
        is_owner,
        layout,
        (&mut *text_renderer, theme),
    );

    let caption_y = card.y0 + 12.0;
    draw_control_caption(
        scene,
        transform,
        Point::new(layout.expiry_rects[0].x0 + 2.0, caption_y),
        "Until",
        text_renderer,
        theme,
    );
    draw_expiry_track(
        scene,
        transform,
        layout,
        draft,
        is_owner,
        text_renderer,
        theme,
    );

    if layout.submit_rect.area() > 0.0 {
        draw_primary_button(
            scene,
            transform,
            layout.submit_rect,
            "Grant",
            is_owner,
            text_renderer,
            theme,
        );
    }
    card
}

/// The expiry segmented control inside the composer card.
fn draw_expiry_track(
    scene: &mut Scene,
    transform: Affine,
    layout: &PermissionsPaneLayout,
    draft: &GrantDraft,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let expiry_track = Rect::new(
        layout.expiry_rects[0].x0,
        layout.expiry_rects[0].y0,
        layout.expiry_rects[2].x1,
        layout.expiry_rects[2].y1,
    );
    let track_rr = RoundedRect::from_rect(expiry_track, f64::from(tokens.radii.md));
    let track_alpha = if is_owner { 1.0 } else { DISABLED_ALPHA };
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised.with_alpha_factor(track_alpha),
        None,
        &track_rr,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        colors.border_standard.with_alpha_factor(track_alpha),
        None,
        &track_rr,
    );
    for (seg, expiry) in layout.expiry_rects.into_iter().zip(EXPIRY_CHOICES) {
        let selected = draft.expiry == expiry;
        if selected {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(0.90 * track_alpha),
                None,
                &RoundedRect::from_rect(seg.inset(2.0), f64::from(tokens.radii.md) - 2.0),
            );
        }
        let label = expiry_label(expiry);
        let size = tokens.typography.scale.sm;
        let (tw, th) = text_renderer.measure(label, size, None);
        let color = if selected {
            colors.text_primary
        } else {
            colors.text_dim
        };
        text_renderer.draw_text(
            scene,
            label,
            size,
            color.with_alpha_factor(track_alpha),
            at(
                transform,
                seg.center().x - f64::from(tw) / 2.0,
                seg.center().y - f64::from(th) / 2.0,
            ),
            None,
        );
    }
}

/// Paint one [`PermissionRow`] — avatar, subject, expiry chip, Revoke button.
fn draw_permission_row(
    scene: &mut Scene,
    transform: Affine,
    row: &PermissionRow,
    is_owner: bool,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    draw_row_card(scene, transform, row.row_rect, theme);

    let avatar_size = 28.0_f64;
    let avatar = Rect::new(
        row.row_rect.x0 + 12.0,
        row.row_rect.center().y - avatar_size / 2.0,
        row.row_rect.x0 + 12.0 + avatar_size,
        row.row_rect.center().y + avatar_size / 2.0,
    );
    let title = row.subject.display();
    draw_avatar(
        scene,
        transform,
        avatar,
        &title,
        row.subject.kind == SubjectKind::App,
        text_renderer,
        theme,
    );

    let chip = draw_pill(
        scene,
        transform,
        Point::new(row.revoke_rect.x0 - 12.0, row.row_rect.center().y),
        row.expiry,
        Tint {
            color: colors.accent[5],
            alpha: 1.0,
        },
        text_renderer,
        theme,
    );

    let text_x = avatar.x1 + 12.0;
    let text_max = chip.x0 - 12.0 - text_x;
    let title_size = tokens.typography.scale.sm;
    let title_text = elide(&title, title_size, text_max, text_renderer);
    text_renderer.draw_text(
        scene,
        &title_text,
        title_size,
        colors.text_primary,
        at(transform, text_x, row.row_rect.y0 + 12.0),
        None,
    );
    let detail = match &row.granted_by {
        Some(by) => format!("{} · {by}", row.allows),
        None => row.allows.clone(),
    };
    let detail_size = tokens.typography.scale.xs;
    let detail_text = elide(&detail, detail_size, text_max, text_renderer);
    text_renderer.draw_text(
        scene,
        &detail_text,
        detail_size,
        colors.text_dim,
        at(transform, text_x, row.row_rect.y0 + 32.0),
        None,
    );

    draw_danger_button(
        scene,
        transform,
        row.revoke_rect,
        "Revoke",
        is_owner,
        text_renderer,
        theme,
    );
}

#[cfg(test)]
mod tab_tests {
    //! The modal's tab surface: default tab, switching, sidebar
    //! geometry, and that the sidebar renders without a pane drawer.

    use super::*;

    fn ws_settings_default() -> WorkspaceSettings {
        WorkspaceSettings::new(WorkspaceId::default())
    }

    #[test]
    fn ws_settings_default_tab_is_general() {
        let ws = ws_settings_default();
        assert_eq!(ws.active_tab, WsTab::General);
    }

    #[test]
    fn select_tab_switches_active() {
        let mut ws = ws_settings_default();
        ws.select_tab(WsTab::Members);
        assert_eq!(ws.active_tab, WsTab::Members);
        ws.select_tab(WsTab::Permissions);
        assert_eq!(ws.active_tab, WsTab::Permissions);
        ws.select_tab(WsTab::General);
        assert_eq!(ws.active_tab, WsTab::General);
    }

    #[test]
    fn ws_tab_default_matches_general() {
        assert_eq!(WsTab::default(), WsTab::General);
    }

    #[test]
    fn ws_tab_all_iterates_in_nav_order() {
        assert_eq!(
            WsTab::all(),
            [
                WsTab::General,
                WsTab::Members,
                WsTab::Permissions,
                WsTab::Apps
            ]
        );
    }

    #[test]
    fn compact_labels_keep_every_destination_distinct() {
        assert_eq!(
            WsTab::all().map(|tab| tab.compact_label()),
            ["General", "People", "Privacy", "Apps"]
        );
    }

    #[test]
    fn permissions_tab_is_reachable_on_a_workspace_with_no_grants() {
        // A host who wants to grant something before any grant exists
        // has to be able to reach the tab that grants it.
        let ws = Workspace::new(WorkspaceId::default(), "no-grants");
        assert!(!ws.has_app_grants());
        assert!(WsTab::all().contains(&WsTab::Permissions));
        let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
        let layout = permissions_pane_layout(&ws, pane);
        assert!(
            layout.choice_rect.area() > 0.0,
            "the capability picker must be laid out even with zero grants"
        );
    }

    #[test]
    fn sidebar_rect_is_256_px_wide() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let sidebar = ws.sidebar_rect(modal);
        assert_eq!(sidebar.width(), SIDEBAR_WIDTH);
        assert_eq!(sidebar.height(), modal.height());
        assert_eq!(sidebar.x0, modal.x0);
    }

    #[test]
    fn pane_rect_starts_after_sidebar() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let pane = ws.pane_rect(modal);
        assert_eq!(pane.x0, modal.x0 + SIDEBAR_WIDTH);
        assert_eq!(pane.x1, modal.x1);
        assert!((pane.width() + SIDEBAR_WIDTH - modal.width()).abs() < f64::EPSILON);
    }

    #[test]
    fn nav_rows_yield_one_rect_per_tab_in_order() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let rows = ws.nav_rows(modal);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].0, WsTab::General);
        assert_eq!(rows[1].0, WsTab::Members);
        assert_eq!(rows[2].0, WsTab::Permissions);
        assert_eq!(rows[3].0, WsTab::Apps);
        for (_, r) in &rows {
            assert!((r.height() - NAV_ROW_HEIGHT).abs() < f64::EPSILON);
            assert!(r.x1 > r.x0);
        }
        let stride = NAV_ROW_HEIGHT + NAV_ROW_GAP;
        assert!((rows[1].1.y0 - rows[0].1.y0 - stride).abs() < f64::EPSILON);
        assert!((rows[2].1.y0 - rows[1].1.y0 - stride).abs() < f64::EPSILON);
    }

    #[test]
    fn nav_hit_test_returns_tab_at_point() {
        let ws = ws_settings_default();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        for (tab, row) in ws.nav_rows(modal) {
            assert_eq!(ws.nav_hit_test(modal, row.center()), Some(tab));
        }
        assert_eq!(
            ws.nav_hit_test(modal, Point::new(modal.x1 - 10.0, modal.y1 - 10.0)),
            None
        );
    }

    #[test]
    fn draw_body_paints_sidebar_when_pane_drawer_absent() {
        let ws = ws_settings_default();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        ws.draw_body(&mut scene, Affine::IDENTITY, modal, &mut tr, &theme, None);
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "sidebar must paint at least one draw command even without a pane drawer"
        );
    }

    #[test]
    fn draw_body_invokes_pane_drawer_for_active_tab() {
        let mut ws = ws_settings_default();
        ws.select_tab(WsTab::Members);
        let modal = Rect::new(0.0, 0.0, 896.0, 600.0);
        let expected_pane = ws.pane_rect(modal);

        let mut observed: Option<Rect> = None;
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut drawer =
            |_s: &mut Scene, _x: Affine, pane: Rect, _t: &mut TextRenderer, _th: &Theme| {
                observed = Some(pane);
            };
        ws.draw_body(
            &mut scene,
            Affine::IDENTITY,
            modal,
            &mut tr,
            &theme,
            Some(&mut drawer),
        );
        assert_eq!(observed, Some(expected_pane));
    }
}

#[cfg(test)]
mod members_pane_tests {
    //! The Members pane renders [`Workspace::roster`] — the membership
    //! the capability model actually holds — and its role control
    //! rewrites the membership capability that `role_of` reads back.

    use super::*;
    use crate::shell::state::{Verb, WorkspaceId};

    const OWNER: &str = "owner-user";
    const PEER: &str = "peer-user";

    fn ws_with_two_members() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(7), "members-test");
        ws.set_owner(OWNER);
        let cap =
            CapOverride::membership(Subject::person(PEER), ws.id.raw().to_string(), Verb::Read);
        ws.grant_as(&OWNER.to_string(), cap)
            .expect("owner may grant membership");
        ws
    }

    #[test]
    fn members_rows_come_from_the_roster() {
        let ws = ws_with_two_members();
        let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
        let rows = members_pane_rows(&ws, pane);
        assert_eq!(rows.len(), 2, "owner + invited peer");
        let owner_row = rows
            .iter()
            .find(|r| r.user == OWNER)
            .expect("owner is in the roster");
        assert!(owner_row.is_workspace_owner);
        assert_eq!(owner_row.role, CapRole::Owner);
        let peer_row = rows.iter().find(|r| r.user == PEER).expect("peer row");
        assert!(!peer_row.is_workspace_owner);
        assert_eq!(peer_row.role, CapRole::Viewer);
        for row in &rows {
            assert!(row.role_rect.x0 > row.row_rect.x0);
            assert!(row.remove_rect.x1 <= row.row_rect.x1);
            assert!(row.role_rect.x1 <= row.remove_rect.x0);
        }
    }

    #[test]
    fn owner_can_promote_a_member_and_role_of_agrees() {
        let mut ws = ws_with_two_members();
        assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
        let edit = set_member_role(
            &mut ws,
            &OWNER.to_string(),
            &PEER.to_string(),
            CapRole::Editor,
        );
        assert!(!edit.is_empty());
        assert_eq!(
            ws.role_of(&PEER.to_string()),
            Some(CapRole::Editor),
            "the role the panel shows must be the role the model decides with"
        );
        assert!(ws.allows(
            &ws.actor_for_person(&PEER.to_string()),
            &crate::shell::state::ResourceRef::Workspace,
            Verb::Write
        ));
    }

    #[test]
    fn owner_can_demote_a_member() {
        // `grant_capability` keeps the strongest of two grants, so the
        // old membership must be retired first or `role_of` keeps
        // answering the higher role.
        let mut ws = ws_with_two_members();
        set_member_role(
            &mut ws,
            &OWNER.to_string(),
            &PEER.to_string(),
            CapRole::Editor,
        );
        assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Editor));
        set_member_role(
            &mut ws,
            &OWNER.to_string(),
            &PEER.to_string(),
            CapRole::Viewer,
        );
        assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
        assert!(
            !ws.allows(
                &ws.actor_for_person(&PEER.to_string()),
                &crate::shell::state::ResourceRef::Workspace,
                Verb::Write
            ),
            "a demoted member must stop being able to write, not merely stop reading as Editor"
        );
    }

    #[test]
    fn non_owner_cannot_change_a_role() {
        let mut ws = ws_with_two_members();
        let edit = set_member_role(
            &mut ws,
            &PEER.to_string(),
            &PEER.to_string(),
            CapRole::Owner,
        );
        assert!(
            edit.is_empty(),
            "a viewer must not be able to promote itself"
        );
        assert_eq!(ws.role_of(&PEER.to_string()), Some(CapRole::Viewer));
    }

    #[test]
    fn the_recorded_owner_role_is_structural() {
        let mut ws = ws_with_two_members();
        let edit = set_member_role(
            &mut ws,
            &OWNER.to_string(),
            &OWNER.to_string(),
            CapRole::Viewer,
        );
        assert!(edit.is_empty());
        assert_eq!(ws.role_of(&OWNER.to_string()), Some(CapRole::Owner));
    }

    #[test]
    fn removing_a_member_takes_their_grants_with_them() {
        let mut ws = ws_with_two_members();
        let cap = CapOverride::grant(
            Subject::person(PEER),
            Scope::FsPath("/var/workspace".into()),
            Verb::Write,
        );
        ws.grant_as(&OWNER.to_string(), cap).expect("owner grants");
        assert_eq!(
            members_pane_rows(&ws, Rect::new(0.0, 0.0, 640.0, 480.0))[1].grant_count,
            1
        );

        let edit = remove_member(&mut ws, &OWNER.to_string(), &PEER.to_string());
        assert_eq!(edit.revoked.len(), 2, "membership + the file grant");
        assert_eq!(ws.role_of(&PEER.to_string()), None);
        assert!(
            !ws.allows(
                &ws.actor_for_person(&PEER.to_string()),
                &crate::shell::state::ResourceRef::FsPath("/var/workspace/notes".into()),
                Verb::Write
            ),
            "removing a member must revoke what they held, not just hide them"
        );
    }

    #[test]
    fn the_owner_cannot_be_removed() {
        let mut ws = ws_with_two_members();
        let edit = remove_member(&mut ws, &OWNER.to_string(), &OWNER.to_string());
        assert!(edit.is_empty());
        assert_eq!(ws.role_of(&OWNER.to_string()), Some(CapRole::Owner));
    }

    #[test]
    fn role_chip_cycles_through_the_positive_roles() {
        assert_eq!(next_role(CapRole::Viewer), CapRole::Commenter);
        assert_eq!(next_role(CapRole::Commenter), CapRole::Editor);
        assert_eq!(next_role(CapRole::Editor), CapRole::Owner);
        assert_eq!(next_role(CapRole::Owner), CapRole::Viewer);
    }

    #[test]
    fn draw_members_pane_paints_more_with_members_than_without() {
        let theme = Theme::dark();
        let pane = Rect::new(0.0, 0.0, 640.0, 480.0);
        let mut tr = TextRenderer::new();

        let mut empty = Scene::new();
        draw_members_pane(
            &mut empty,
            Affine::IDENTITY,
            pane,
            &Workspace::new(WorkspaceId::default(), "empty"),
            &OWNER.to_string(),
            true,
            &mut tr,
            &theme,
        );
        let mut full = Scene::new();
        draw_members_pane(
            &mut full,
            Affine::IDENTITY,
            pane,
            &ws_with_two_members(),
            &OWNER.to_string(),
            true,
            &mut tr,
            &theme,
        );
        assert!(
            full.encoding().draw_tags.len() > empty.encoding().draw_tags.len(),
            "a populated roster must paint more than the empty state"
        );
    }

    #[test]
    fn draw_members_pane_no_op_on_zero_area_pane() {
        let ws = ws_with_two_members();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        draw_members_pane(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(100.0, 100.0, 100.0, 100.0),
            &ws,
            &OWNER.to_string(),
            true,
            &mut tr,
            &theme,
        );
        assert!(scene.encoding().draw_tags.is_empty());
    }
}

#[cfg(test)]
mod permissions_pane_tests {
    //! The Permissions pane lists [`Workspace::capabilities`] and its
    //! composer writes one through [`Workspace::grant_as`].

    use super::*;
    use crate::shell::state::{Capability, GRANT_ALWAYS, GRANT_SESSION, ResourceRef, WorkspaceId};

    const OWNER: &str = "owner-user";

    fn ws_with_grants() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(3), "perm-test");
        ws.set_owner(OWNER);
        ws.grant_app(
            "firefox",
            &Capability::Camera {
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "firefox",
            &Capability::Network {
                hosts: None,
                scope: GRANT_SESSION,
            },
        );
        ws
    }

    fn pane() -> Rect {
        Rect::new(0.0, 0.0, 660.0, 480.0)
    }

    #[test]
    fn rows_are_one_per_granted_capability() {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        assert_eq!(layout.rows.len(), 2);
        for row in &layout.rows {
            assert_eq!(row.subject.kind, SubjectKind::App);
            assert_eq!(row.subject.id, "firefox");
            assert!(row.revoke_rect.x1 <= row.row_rect.x1);
            assert!(row.revoke_rect.x0 > row.row_rect.x0);
            assert!(!row.cap_id.is_empty(), "rows are keyed by capability id");
        }
        assert_ne!(layout.rows[0].cap_id, layout.rows[1].cap_id);
        assert_eq!(layout.overflow, 0);
    }

    #[test]
    fn membership_capabilities_are_not_listed_as_permissions() {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        assert!(
            layout
                .rows
                .iter()
                .all(|r| r.subject.kind == SubjectKind::App),
            "only non-membership grants belong in the Permissions list"
        );
    }

    #[test]
    fn a_workspace_scoped_app_grant_is_listed_and_revocable() {
        let mut ws = ws_with_grants();
        let id = ws
            .grant_as(
                &OWNER.to_string(),
                CapOverride::grant(Subject::app("firefox"), Scope::Workspace, Verb::Admin),
            )
            .expect("owner grants");
        let layout = permissions_pane_layout(&ws, pane());
        let row = layout
            .rows
            .iter()
            .find(|r| r.cap_id == id)
            .expect("the workspace-scoped app grant has a row");
        assert_eq!(row.subject.kind, SubjectKind::App);
        assert_eq!(row.allows, "Administer this workspace");
        assert_eq!(
            apply_revoke(&mut ws, &OWNER.to_string(), &row.cap_id)
                .revoked
                .len(),
            1
        );
        assert!(!ws.allows(
            &ws.actor_for_app("firefox"),
            &ResourceRef::Workspace,
            Verb::Admin
        ));
    }

    #[test]
    fn a_persons_membership_is_left_to_the_members_tab() {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        assert!(
            layout
                .rows
                .iter()
                .all(|r| r.subject.kind == SubjectKind::App),
            "no person membership rows in the Permissions list"
        );
        assert!(
            !members_pane_rows(&ws, pane()).is_empty(),
            "but Members has one"
        );
    }

    #[test]
    fn person_and_role_subjects_are_listed_too() {
        let mut ws = ws_with_grants();
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::person("peer-user"),
                Scope::FsPath("/var/workspace".into()),
                Verb::Write,
            ),
        )
        .expect("owner grants");
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::Role(CapRole::Editor),
                Scope::Device("camera".into()),
                Verb::Read,
            ),
        )
        .expect("owner grants");
        // This assertion is about grouping every subject kind, so give
        // the viewport room for every seeded row after the explanatory
        // header and composer.
        let layout = permissions_pane_layout(&ws, Rect::new(0.0, 0.0, 660.0, 620.0));
        let kinds: Vec<SubjectKind> = layout.rows.iter().map(|r| r.subject.kind).collect();
        assert!(kinds.contains(&SubjectKind::App));
        assert!(kinds.contains(&SubjectKind::Member));
        assert!(kinds.contains(&SubjectKind::Role));
        let mut sorted = kinds.clone();
        sorted.sort();
        assert_eq!(kinds, sorted, "rows must be grouped by subject kind");
    }

    #[test]
    fn rows_carry_the_scope_and_grantor_the_model_recorded() {
        let mut ws = Workspace::new(WorkspaceId::from(4), "labels");
        ws.set_owner(OWNER);
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::FsPath("/var/workspace".into()),
                Verb::Write,
            )
            .expiring(Expiry::EndOfSession),
        )
        .expect("owner grants");
        let layout = permissions_pane_layout(&ws, pane());
        let row = &layout.rows[0];
        assert_eq!(row.allows, "Edit workspace files");
        assert_eq!(row.expiry, "Session");
        assert_eq!(
            row.granted_by.as_deref(),
            Some("granted by owner-user"),
            "grant_as records the grantor; the row shows it"
        );
    }

    /// No capability line may name the mount point workspace files are
    /// projected at (HIG §4). Swept over every scope kind, not just the
    /// two the panel shows today, so a new scope can't reintroduce the leak.
    #[test]
    fn no_capability_line_leaks_the_workspace_mount_point() {
        use collab_types::role::ArtifactKind;
        let scopes = vec![
            Scope::Workspace,
            Scope::Room("r".into()),
            Scope::App("firefox".into()),
            Scope::FsPath(WORKSPACE_FILES_PATH.into()),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/")),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design/drafts")),
            Scope::Device("camera".into()),
            Scope::NetHost("*".into()),
            Scope::NetHost("example.com".into()),
            Scope::Action {
                app: "notes".into(),
                action: "export".into(),
            },
            Scope::ResourceType(ArtifactKind::Note),
        ];
        let verbs = [
            Verb::Read,
            Verb::Write,
            Verb::Admin,
            Verb::Comment,
            Verb::Invite,
        ];
        for scope in &scopes {
            for verb in verbs {
                let cap = CapOverride::grant(Subject::app("a"), scope.clone(), verb);
                let line = capability_allows(&cap);
                assert!(
                    !line.contains(WORKSPACE_FILES_PATH),
                    "the permission line {line:?} names {WORKSPACE_FILES_PATH} \
                     — where the OS keeps workspace files is not what the user \
                     is being asked to agree to",
                );
                assert!(
                    !line.contains("/var/"),
                    "the permission line {line:?} exposes a system path",
                );
                assert!(
                    !line.is_empty(),
                    "every scope must still describe itself; {scope:?} produced \
                     an empty line",
                );
            }
        }
    }

    /// The projection root and a path inside it must read as different
    /// things, so a row never understates what is being granted.
    #[test]
    fn workspace_scopes_distinguish_the_root_from_a_folder_inside_it() {
        let root = CapOverride::grant(
            Subject::app("a"),
            Scope::FsPath(WORKSPACE_FILES_PATH.into()),
            Verb::Write,
        );
        let sub = CapOverride::grant(
            Subject::app("a"),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
            Verb::Write,
        );
        assert_eq!(capability_allows(&root), "Edit workspace files");
        assert_eq!(
            capability_allows(&sub),
            "Edit \u{201c}design\u{201d} in this workspace",
        );
        assert_ne!(
            capability_allows(&root),
            capability_allows(&sub),
            "granting every workspace file must not read the same as granting \
             one folder in it",
        );
    }

    #[test]
    fn the_composer_offers_the_roster_and_the_workspaces_apps() {
        let ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        assert_eq!(
            subjects[0],
            GrantSubject {
                kind: SubjectKind::Member,
                id: OWNER.to_string()
            },
            "people come first"
        );
        assert!(subjects.contains(&GrantSubject {
            kind: SubjectKind::App,
            id: "firefox".to_string()
        }));
    }

    #[test]
    fn granting_from_the_composer_writes_a_capability_that_enforces() {
        let mut ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        let firefox = subjects
            .iter()
            .position(|s| s.id == "firefox")
            .expect("firefox is a candidate subject");
        let mut draft = GrantDraft {
            subject_index: firefox,
            choice_index: 0,
            expiry: Expiry::Never,
        };
        while draft.choice().label != "Microphone" {
            draft.cycle_choice();
        }
        assert!(!ws.allows(
            &ws.actor_for_app("firefox"),
            &ResourceRef::Device("microphone".into()),
            Verb::Read
        ));

        let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
        assert_eq!(edit.granted.len(), 1);
        assert!(
            ws.allows(
                &ws.actor_for_app("firefox"),
                &ResourceRef::Device("microphone".into()),
                Verb::Read
            ),
            "the granted capability must start working, not merely start rendering"
        );
    }

    #[test]
    fn granting_records_the_chosen_expiry() {
        let mut ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
        // Re-granting an existing capability merges expiries, so pick
        // something the app doesn't already hold to observe this one.
        let mut draft = GrantDraft {
            subject_index: firefox,
            choice_index: 0,
            expiry: Expiry::AfterUse,
        };
        while draft.choice().label != "Microphone" {
            draft.cycle_choice();
        }
        let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
        let id = &edit.granted[0];
        let cap = ws
            .capabilities
            .iter()
            .find(|c| &c.id == id)
            .expect("stored capability");
        assert_eq!(cap.expiry, Expiry::AfterUse);
        assert!(
            !cap.persists_across_restart(),
            "an after-use grant must not be written down as permanent"
        );
    }

    #[test]
    fn re_granting_never_shortens_an_existing_grant() {
        // `grant_capability` keeps the longer expiry for the same
        // (subject, scope, effect, verb).
        let mut ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
        let mut draft = GrantDraft {
            subject_index: firefox,
            choice_index: 0,
            expiry: Expiry::AfterUse,
        };
        while draft.choice().label != "Camera" {
            draft.cycle_choice();
        }
        apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
        let camera = ws
            .capabilities
            .iter()
            .filter(|c| matches!(&c.scope, Scope::Device(d) if d == "camera"))
            .collect::<Vec<_>>();
        assert_eq!(
            camera.len(),
            1,
            "the re-grant merges rather than duplicating"
        );
        assert_eq!(
            camera[0].expiry,
            Expiry::Never,
            "the existing permanent grant is not demoted to once-only"
        );
        let layout = permissions_pane_layout(&ws, pane());
        let row = layout
            .rows
            .iter()
            .find(|r| r.allows.contains("camera"))
            .expect("camera row");
        assert_eq!(
            row.expiry, "Always",
            "the row must report the expiry that is stored, not the one that was asked for"
        );
    }

    #[test]
    fn a_non_owner_cannot_grant() {
        let mut ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
        let draft = GrantDraft {
            subject_index: firefox,
            choice_index: 0,
            expiry: Expiry::Never,
        };
        let before = ws.capabilities.len();
        let edit = apply_grant_draft(&mut ws, &"someone-else".to_string(), &draft);
        assert!(edit.is_empty(), "grant_as must refuse a non-administrator");
        assert_eq!(ws.capabilities.len(), before);
    }

    #[test]
    fn revoking_a_row_stops_the_capability_working() {
        let mut ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        let camera_row = layout
            .rows
            .iter()
            .find(|r| r.allows.contains("camera"))
            .expect("camera row");
        let edit = apply_revoke(&mut ws, &OWNER.to_string(), &camera_row.cap_id);
        assert_eq!(edit.revoked, vec![camera_row.cap_id.clone()]);
        assert!(
            !ws.allows(
                &ws.actor_for_app("firefox"),
                &ResourceRef::Device("camera".into()),
                Verb::Read
            ),
            "the revoked capability must stop working, not merely stop rendering"
        );
        let after = permissions_pane_layout(&ws, pane());
        assert_eq!(after.rows.len(), 1);
    }

    #[test]
    fn a_non_owner_cannot_revoke() {
        let mut ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        let target = layout.rows[0].cap_id.clone();
        let edit = apply_revoke(&mut ws, &"someone-else".to_string(), &target);
        assert!(edit.is_empty());
        assert_eq!(permissions_pane_layout(&ws, pane()).rows.len(), 2);
    }

    #[test]
    fn revoking_an_unknown_id_is_a_no_op() {
        let mut ws = ws_with_grants();
        let edit = apply_revoke(&mut ws, &OWNER.to_string(), "cap-does-not-exist");
        assert!(edit.is_empty());
        assert_eq!(ws.capabilities.len(), 3, "owner membership + two grants");
    }

    #[test]
    fn every_offered_capability_produces_a_scope_an_enforcement_point_reads() {
        let ws = ws_with_grants();
        let subjects = grant_subjects(&ws);
        let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
        for (i, choice) in GRANT_CHOICES.iter().enumerate() {
            let draft = GrantDraft {
                subject_index: firefox,
                choice_index: i,
                expiry: Expiry::Never,
            };
            let cap = draft_capability(&ws, &draft).expect("every choice resolves");
            assert_eq!(cap.effect, Effect::Grant);
            let resource = match &cap.scope {
                Scope::Device(d) => ResourceRef::Device(d.clone()),
                Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
                Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
                Scope::Workspace => ResourceRef::Workspace,
                other => panic!("unexpected scope in the grant table: {other:?}"),
            };
            let mut probe = ws.clone();
            probe
                .grant_as(&OWNER.to_string(), cap.clone())
                .expect("owner grants");
            assert!(
                probe.allows(&probe.actor_for_app("firefox"), &resource, cap.verb),
                "granting {:?} must make the decision function allow it",
                choice.label
            );
        }
    }

    #[test]
    fn draw_permissions_pane_paints_an_empty_state_with_no_grants() {
        let ws = Workspace::new(WorkspaceId::default(), "empty");
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        draw_permissions_pane(
            &mut scene,
            Affine::IDENTITY,
            pane(),
            &ws,
            &GrantDraft::default(),
            true,
            &mut tr,
            &theme,
        );
        assert!(
            !scene.encoding().draw_tags.is_empty(),
            "the composer and the empty state must both paint"
        );
    }

    #[test]
    fn draw_permissions_pane_paints_more_with_grants_than_without() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let draft = GrantDraft::default();

        let mut empty = Scene::new();
        draw_permissions_pane(
            &mut empty,
            Affine::IDENTITY,
            pane(),
            &Workspace::new(WorkspaceId::default(), "empty"),
            &draft,
            true,
            &mut tr,
            &theme,
        );
        let mut full = Scene::new();
        draw_permissions_pane(
            &mut full,
            Affine::IDENTITY,
            pane(),
            &ws_with_grants(),
            &draft,
            true,
            &mut tr,
            &theme,
        );
        assert!(full.encoding().draw_tags.len() > empty.encoding().draw_tags.len());
    }

    #[test]
    fn draw_permissions_pane_no_op_on_zero_area_pane() {
        let ws = ws_with_grants();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        draw_permissions_pane(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(100.0, 100.0, 100.0, 100.0),
            &ws,
            &GrantDraft::default(),
            true,
            &mut tr,
            &theme,
        );
        assert!(scene.encoding().draw_tags.is_empty());
    }

    #[test]
    fn rows_that_do_not_fit_are_counted_rather_than_clipped() {
        let mut ws = Workspace::new(WorkspaceId::from(9), "many");
        ws.set_owner(OWNER);
        for i in 0..12 {
            ws.grant_as(
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::app(format!("app-{i}")),
                    Scope::Device("camera".into()),
                    Verb::Read,
                ),
            )
            .expect("owner grants");
        }
        let short_pane = Rect::new(0.0, 0.0, 660.0, 300.0);
        let layout = permissions_pane_layout(&ws, short_pane);
        assert!(layout.rows.len() < 12);
        assert_eq!(layout.rows.len() + layout.overflow, 12);
        for row in &layout.rows {
            assert!(
                row.row_rect.y1 <= short_pane.y1,
                "a laid-out row must fit inside the pane"
            );
        }
    }

    #[test]
    fn expiry_segments_are_laid_out_in_picker_order_without_overlap() {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane());
        for i in 1..3 {
            assert!(layout.expiry_rects[i].x0 >= layout.expiry_rects[i - 1].x1 - f64::EPSILON);
        }
        assert!(layout.expiry_rects[2].x1 <= layout.submit_rect.x0);
        assert!(layout.subject_rect.x1 <= layout.choice_rect.x0);
        assert!(layout.choice_rect.x1 <= layout.expiry_rects[0].x0);
    }

    #[test]
    fn long_identifiers_are_shortened_rather_than_overrun() {
        let long = "a".repeat(64);
        let shortened = short_id(&long);
        assert!(shortened.chars().count() <= 11);
        assert!(shortened.ends_with('…'));
        assert_eq!(short_id("ada"), "ada");
    }
}

#[cfg(test)]
mod roster_order_tests {
    //! The Members list must not reorder under the pointer.

    use super::*;
    use crate::shell::state::WorkspaceId;

    #[test]
    fn changing_a_role_does_not_move_the_row() {
        // A role change rewrites the capability, shifting the raw
        // roster order; the pane's order must not shift with it.
        let mut ws = Workspace::new(WorkspaceId::from(5), "order");
        ws.set_owner("host");
        let room = ws.id.raw().to_string();
        for person in ["ada", "bo", "cy"] {
            ws.grant_as(
                &"host".to_string(),
                CapOverride::membership(Subject::person(person), room.clone(), Verb::Read),
            )
            .expect("owner may grant");
        }
        let pane = Rect::new(0.0, 0.0, 640.0, 600.0);
        let before: Vec<UserId> = members_pane_rows(&ws, pane)
            .into_iter()
            .map(|r| r.user)
            .collect();
        assert_eq!(before, vec!["host", "ada", "bo", "cy"]);

        set_member_role(
            &mut ws,
            &"host".to_string(),
            &"ada".to_string(),
            CapRole::Editor,
        );
        let after: Vec<UserId> = members_pane_rows(&ws, pane)
            .into_iter()
            .map(|r| r.user)
            .collect();
        assert_eq!(after, before, "the row order must be identical");
    }
}

#[cfg(test)]
mod default_app_authority_tests {
    use super::*;

    fn candidate(id: &str) -> desktop_policy::AppCandidate {
        desktop_policy::AppCandidate {
            package_id: id.into(),
            display_name: id.into(),
            resources: [desktop_policy::ResourceKind::extension("pdf").unwrap()].into(),
        }
    }

    #[test]
    fn admin_policy_and_personal_escape_hatch_are_separately_authorized() {
        let candidates = [candidate("dev.qos.viewer"), candidate("dev.qos.other")];
        let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
        let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
        let mut ws = Workspace::new(WorkspaceId::from(7), "Docs");
        ws.set_owner("owner");

        assert!(
            ws.set_default_app_policy_as(
                &"member".into(),
                pdf.clone(),
                desktop_policy::WorkspaceDefaultPolicy::App("dev.qos.viewer".into()),
                &resolver
            )
            .is_err()
        );
        ws.set_default_app_policy_as(
            &"owner".into(),
            pdf.clone(),
            desktop_policy::WorkspaceDefaultPolicy::App("dev.qos.viewer".into()),
            &resolver,
        )
        .unwrap();

        assert!(
            ws.set_default_app_choice_as(
                &"owner".into(),
                &"member".into(),
                pdf.clone(),
                desktop_policy::UserWorkspaceChoice::PersonalDefault,
                &resolver
            )
            .is_err()
        );
        ws.set_default_app_choice_as(
            &"member".into(),
            &"member".into(),
            pdf.clone(),
            desktop_policy::UserWorkspaceChoice::App("dev.qos.other".into()),
            &resolver,
        )
        .unwrap();
        assert_eq!(
            ws.default_apps.user_choices["member"][&pdf],
            desktop_policy::UserWorkspaceChoice::App("dev.qos.other".into())
        );
    }

    #[test]
    fn controls_cycle_through_inheritance_before_apps() {
        let candidates = [candidate("dev.qos.viewer")];
        let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
        assert_eq!(
            next_workspace_default(None, &pdf, &candidates),
            desktop_policy::WorkspaceDefaultPolicy::App("dev.qos.viewer".into())
        );
        assert_eq!(
            next_user_default(None, &pdf, &candidates),
            desktop_policy::UserWorkspaceChoice::PersonalDefault
        );
    }
}
