//! The Tilo mark and the workspace pill: the brand's own shapes, the name the
//! pill shows and the budget it has for it, the chevron that opens the
//! workspace menu, and the rects a press on either addresses.

use super::*;

impl TopBarChrome {
    /// Lead the bar with the Tilo mark, which asks for
    /// [`ChromeIntent::ToggleTiloMenu`]: a host with no other place for
    /// what Tilo is (the browser) offers it there. Off by default.
    #[must_use]
    pub fn with_tilo_mark(mut self) -> Self {
        self.tilo_mark = Some(false);
        self
    }

    /// The menu the mark opens is shown or gone; the mark reads as pressed
    /// while it is.
    pub fn set_tilo_menu_open(&mut self, open: bool) {
        if let Some(shown) = self.tilo_mark.as_mut() {
            *shown = open;
        }
    }

    /// Where the Tilo mark was last drawn, for anchoring its menu; zero
    /// without the mark.
    pub fn tilo_mark_rect(&self) -> Rect {
        if self.tilo_mark.is_some() {
            self.control_rect(Self::CONTROL_TILO_MARK)
        } else {
            Rect::ZERO
        }
    }

    /// Where the workspace name was last drawn, for anchoring its menu
    /// ([`ChromeIntent::ToggleWorkspaceMenu`]: the workspaces, where it is
    /// hosted, joining and leaving, renaming, the overview, a new desk).
    pub fn workspace_menu_rect(&self) -> Rect {
        self.control_rect(Self::CONTROL_WORKSPACE_SETTINGS)
    }

    /// Semibold — a clickable title without tipping into the clock's
    /// h1/h2 tier.
    pub fn workspace_name_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    // The following `draw_*` helpers exist only to keep `Widget::draw`
    // under the line budget; none is meaningful on its own.

    /// The Tilo mark as the bar's leading control, starting at `x`; returns
    /// where the next control starts (`x` itself without the mark).
    pub(super) fn draw_tilo_mark(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        x: f64,
    ) -> f64 {
        let Some(open) = self.tilo_mark else {
            self.tilo_mark_rect = Rect::ZERO;
            return x;
        };
        // The opt-in web menu keeps its minimum pointer target inside the
        // compact bar; native controls retain the smaller bar-control size.
        let control = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
        self.tilo_mark_rect = Rect::new(
            x,
            center_y - control / 2.0,
            x + control,
            center_y + control / 2.0,
        );
        paint_bar_selector(
            scene,
            transform,
            &RoundedRect::from_rect(self.tilo_mark_rect, f64::from(theme.tokens.radii.control)),
            ui::chrome::SelectionState::new(
                open,
                self.hovered_control().as_deref() == Some(Self::CONTROL_TILO_MARK),
                false,
            ),
            theme,
        );
        let side = f64::from(theme.tokens.metrics.icon_md);
        paint_tilo_mark(
            scene,
            transform,
            Rect::from_center_size(self.tilo_mark_rect.center(), (side, side)),
            theme,
        );
        x + control + f64::from(theme.tokens.spacing.s2)
    }

    pub(super) fn workspace_label(
        &self,
        text_renderer: &mut TextRenderer,
        ws_text_style: &TextStyle,
        label_budget: f32,
    ) -> String {
        // The name alone: the workspace menu lists the others, so no
        // position count.
        text_renderer.elide_to_width_style(
            &self.workspace_name,
            label_budget.max(0.0),
            &ws_text_style
                .clone()
                .with_weight(self.workspace_name_text_weight()),
        )
    }

    /// How wide the workspace name may grow in `room`. Named strips follow
    /// the pill, so the name leaves them their width down to a short
    /// name's.
    pub(super) fn workspace_label_budget(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        room: f64,
    ) -> f32 {
        let strips = self.strips_natural_width(text_renderer, theme);
        let strips_room = if strips > 0.0 {
            strips + f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        ui::num::f32_of_f64((room - strips_room).max(room.min(96.0)).clamp(32.0, 260.0))
    }

    /// Workspace-name pill (click toggles the workspace menu) and its
    /// trailing chevron, starting at `x`.
    pub(super) fn draw_workspace_pill(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        x: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let ws_text_style = tokens.typography.style(TextRole::Label);
        // Measured at the same weight it's drawn at (Semibold), so the
        // pill width tracks the glyph advance, not the narrower Regular.
        let ws_weight = self.workspace_name_text_weight();
        // The centre carousel owns a fixed central lane.  Long translated or
        // user-authored workspace names must yield to it instead of painting
        // underneath it.
        let center_left =
            f64::midpoint(self.rect.x0, self.rect.x1) - self.center_band_width(theme) / 2.0;
        // Capsule chrome around the label: a leading pad, the gap to the
        // chevron, the chevron itself and a trailing pad of the same size — so
        // the pill reads centred on its contents. The `s4` beyond that is
        // the breathing room `long_workspace_name_yields_to_the_center_lane`
        // keeps between the pill and the centre selector.
        let pad = f64::from(tokens.spacing.s2);
        let chevron = SMALL_GLYPH_PX;
        let pill_chrome = pad * 3.0 + chevron;
        let breathing = f64::from(tokens.spacing.s4);
        let label_budget = self.workspace_label_budget(
            text_renderer,
            theme,
            center_left - x - pill_chrome - breathing,
        );
        let ws_label = self.workspace_label(text_renderer, &ws_text_style, label_budget);
        let (ws_w, ws_h) = text_renderer.measure_style(
            &ws_label,
            None,
            &ws_text_style.clone().with_weight(ws_weight),
        );
        // A bar control's height, grown with its label at a large text
        // scale (as `ControlSize` grows), never taller than the bar.
        let pill_h = BAR_CONTROL_PX
            .max((f64::from(ws_h) + f64::from(tokens.spacing.s1) / 2.0).ceil())
            .min(self.rect.height().max(BAR_CONTROL_PX));
        // A bar control: the workspace name and its chevron, plain at rest,
        // lifting under the pointer and while its menu is open.
        let workspace_lens_open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.snapshot().overlays.ws_settings);
        let name = self.control_name(Self::CONTROL_WORKSPACE_SETTINGS);
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_WORKSPACE_SETTINGS);
        let face = ui::widget::BarControlFace::new(&name)
            .label(&ws_label)
            .trailing(Self::WORKSPACE_MENU_ICON)
            .open(workspace_lens_open)
            .hovered(hovered);
        let pill_w = face.width(text_renderer, theme);
        let ws_pill_outer = Rect::new(
            x - pad,
            center_y - pill_h / 2.0,
            x - pad + pill_w,
            center_y + pill_h / 2.0,
        );
        if self.renaming_workspace() {
            // The new name is typed where the name stands, so it reads as
            // renaming the workspace, not the strip named after it.
            let field_h = ui::widget::FieldFace::height(ui::widget::ControlSize::Regular, theme);
            let room = (center_left - breathing - ws_pill_outer.x0).max(BAR_CONTROL_PX);
            let field = Rect::new(
                ws_pill_outer.x0,
                center_y - field_h / 2.0,
                ws_pill_outer.x0 + pill_w.max(strips::FIELD_MAX_PX).min(room),
                center_y + field_h / 2.0,
            );
            self.ws_pill_rect = field;
            self.ws_chevron_rect = Rect::ZERO;
            self.paint_workspace_name_field(scene, transform, theme, text_renderer, field);
            return;
        }
        self.ws_pill_rect = ws_pill_outer;
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.settings_anchor(ws_pill_outer);
        }
        let ground = colors.surfaces.base;
        let mut ctx = ui::paint::PaintCtx::new(scene, transform, text_renderer, theme);
        face.paint(&mut ctx, ws_pill_outer, ground);
        let chevron_x = x + f64::from(ws_w) + f64::from(tokens.metrics.icon_label_gap);
        self.ws_chevron_rect = Rect::new(
            chevron_x,
            center_y - chevron / 2.0,
            chevron_x + chevron,
            center_y + chevron / 2.0,
        );
    }
}

/// The Tilo mark's left bar as the brand's `mark.svg` draws it, in its
/// 100 × 100 view box: `M10.98 31.85 Q10.979892448752274 27.96648782251284
/// 14.4 26.12 L20.26 22.97 Q… Z`. The right bar is its mirror image about
/// the box's centre line and the central block a plain rounded rectangle.
pub(super) fn tilo_mark_side() -> vello::kurbo::BezPath {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((10.98, 31.85));
    path.quad_to(
        (10.979_892_448_752_274, 27.966_487_822_512_84),
        (14.4, 26.12),
    );
    path.line_to((20.26, 22.97));
    path.quad_to(
        (24.843_373_493_975_903, 20.506_024_096_385_54),
        (24.84, 25.71),
    );
    path.line_to((24.84, 74.29));
    path.quad_to(
        (24.843_373_493_975_903, 79.493_975_903_614_46),
        (20.26, 77.03),
    );
    path.line_to((14.4, 73.88));
    path.quad_to(
        (10.979_892_448_752_274, 72.033_512_177_487_15),
        (10.98, 68.15),
    );
    path.close_path();
    path
}

/// The Tilo mark, the brand's own shapes, centred in `rect` with its view
/// box the largest square that fits. A dark palette takes the brand's dark
/// variant (`mark-dark.svg`): the light one's pale bars glare there.
pub fn paint_tilo_mark(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    static SIDE: std::sync::LazyLock<vello::kurbo::BezPath> =
        std::sync::LazyLock::new(tilo_mark_side);
    let (sides, block) = if theme.tokens.colors.palette.dark {
        (
            Color::from_rgb8(0x3b, 0x43, 0x76),
            Color::from_rgb8(0x6f, 0x82, 0xff),
        )
    } else {
        (
            Color::from_rgb8(0x9f, 0xa6, 0xe0),
            Color::from_rgb8(0x2a, 0x3f, 0xd1),
        )
    };
    let side = rect.width().min(rect.height());
    let center = rect.center();
    let view = transform
        .pre_translate((center.x - side / 2.0, center.y - side / 2.0).into())
        .pre_scale(side / 100.0);
    scene.fill(Fill::NonZero, view, sides, None, &*SIDE);
    let mirrored = view
        .pre_translate((100.0, 0.0).into())
        .pre_scale_non_uniform(-1.0, 1.0);
    scene.fill(Fill::NonZero, mirrored, sides, None, &*SIDE);
    let block_shape = RoundedRect::new(29.0, 16.0, 71.0, 84.0, 6.0);
    scene.fill(Fill::NonZero, view, block, None, &block_shape);
}
