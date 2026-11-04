//! `AgendaPopover`: the bar's clock overlay — a title, a big reading, a
//! pause control, an add control and a close control — on
//! [`FloatingPanelFace`] (`docs/architecture/ui-inventory.md`,
//! "Clock/agenda popover").
//!
//! The floating timer used to paint its own panel, its own grip, its own
//! headline and its own buttons. An agenda popover is that whole thing as
//! one floating surface: the floating material, the panel radius and the
//! two-layer shadow come from [`FloatingPanelFace`]; the grip is the
//! panel's own drag handle; the headline and the reading are [`Text`]'s
//! roles; the controls are [`ButtonFace`]s. The host drags the panel by
//! [`AgendaPopoverFace::header`], hit-tests its controls with
//! [`AgendaPopoverLayout`], and never positions a control itself.

use vello::kurbo::{Line, Point, Rect, Stroke};

use crate::icons::Icon;
use crate::paint::PaintCtx;

use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::primitives::FloatingPanelFace;
use crate::widget::{Button, ButtonFace, ButtonRole, FloatingPanelLayout};

/// The reading an agenda popover shows: a title, the time, and its state.
#[derive(Clone, Debug, PartialEq)]
pub struct AgendaEntry {
    /// What the thing is called: "Tea".
    pub title: String,
    /// The reading itself, already formatted: "2:05".
    pub reading: String,
    /// Running right now (the pause button reads "Pause") rather than paused (it reads "Resume").
    pub running: bool,
}

impl AgendaEntry {
    /// A running entry.
    #[must_use]
    pub fn new(title: impl Into<String>, reading: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            reading: reading.into(),
            running: true,
        }
    }

    /// The entry is paused: its pause control offers to resume.
    #[must_use]
    pub fn paused(mut self) -> Self {
        self.running = false;
        self
    }
}

/// The two labels a popover's controls say, and the close control's name.
#[derive(Clone, Debug, PartialEq)]
pub struct AgendaLabels {
    /// The pause control: "Pause" while running, "Resume" while paused.
    pub toggle: String,
    /// The second control: "Add minute" for a timer, "Lap" for a stopwatch.
    pub add: String,
    /// The close control's accessible name.
    pub close: String,
}

impl AgendaLabels {
    /// The labels for `entry`.
    #[must_use]
    pub fn of(entry: &AgendaEntry) -> Self {
        Self {
            toggle: if entry.running {
                String::from("Pause")
            } else {
                String::from("Resume")
            },
            add: String::from("Add minute"),
            close: String::from("Close"),
        }
    }
}

/// Where an agenda popover put its parts, for the host's hit-testing and
/// for its accessibility tree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AgendaPopoverLayout {
    /// The panel.
    pub panel: Rect,
    /// The drag handle, which the host moves the panel by.
    pub header: Rect,
    /// The pause-or-resume control.
    pub toggle: Rect,
    /// The add control.
    pub add: Rect,
    /// The close control.
    pub close: Rect,
}

/// The clock overlay's content and state, borrowed.
#[derive(Clone, Copy, Debug)]
pub struct AgendaPopoverFace<'a> {
    entry: &'a AgendaEntry,
    labels: &'a AgendaLabels,
    focused: Option<AgendaControl>,
    hovered: Option<AgendaControl>,
}

/// One control of an agenda popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgendaControl {
    /// Pause or resume.
    Toggle,
    /// Add a minute, or take a lap.
    Add,
    /// Close the overlay.
    Close,
}

impl<'a> AgendaPopoverFace<'a> {
    /// The popover showing `entry` with `labels`.
    #[must_use]
    pub fn new(entry: &'a AgendaEntry, labels: &'a AgendaLabels) -> Self {
        Self {
            entry,
            labels,
            focused: None,
            hovered: None,
        }
    }

    /// The keyboard focus, for the ring a reader sees.
    #[must_use]
    pub fn focused(mut self, control: Option<AgendaControl>) -> Self {
        self.focused = control;
        self
    }

    /// The control under the pointer.
    #[must_use]
    pub fn hovered(mut self, control: Option<AgendaControl>) -> Self {
        self.hovered = control;
        self
    }

    /// The height of the popover's two wide actions: the input's minimum for
    /// adjacent controls (44 for a finger), since the floating timer is
    /// commonly used directly on a touch screen rather than at a desk.
    #[must_use]
    pub fn action_height(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.min_pointer_target).max(44.0)
    }

    /// The smallest the panel is: its headline, its reading and its controls.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        // The grip, the title, the reading, a gap, and the control row.
        let grip = f64::from(theme.tokens.spacing.rhythm.related);
        let title = f64::from(theme.text(TextRole::Label).line_height_px());
        let reading = f64::from(
            theme
                .text(TextRole::Heading)
                .with_weight(crate::text_render::TextWeight::Bold)
                .line_height_px(),
        );
        let controls = Self::action_height(theme);
        let inset = f64::from(theme.tokens.spacing.rhythm.section);
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        (grip + 2.0 * inset + title + reading + gap + controls).ceil()
    }

    /// The panel's height for its content: at least [`Self::height`].
    #[must_use]
    pub fn preferred_height(theme: &Theme) -> f64 {
        Self::height(theme)
    }

    /// Paint the popover in `panel` and hand back where its parts landed.
    #[must_use]
    pub fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        panel: Rect,
    ) -> (AgendaPopoverLayout, FloatingPanelLayout) {
        let theme = ctx.theme;
        // The panel's ground, its material and its two-layer shadow are the
        // FloatingPanel's: an agenda popover is a panel above all.
        let face = FloatingPanelFace::new().body_empty(false);
        let (layout, inner) = face.paint(ctx.scene, ctx.transform, panel, 1.0, theme);
        let theme = &inner;
        let inset = f64::from(theme.tokens.spacing.rhythm.section);
        let grip_len = f64::from(theme.tokens.spacing.rhythm.group) * 1.75;
        let grip_y = panel.y0 + inset / 2.0;
        // The drag handle: the line a person takes hold of.
        let grip = Line::new(
            Point::new(panel.center().x - grip_len, grip_y),
            Point::new(panel.center().x + grip_len, grip_y),
        );
        ctx.scene.stroke(
            &Stroke::new(3.0),
            ctx.transform,
            theme.tokens.colors.border_standard,
            None,
            &grip,
        );
        let title_style = theme.text(TextRole::Label);
        let reading_style = theme
            .text(TextRole::Heading)
            .with_weight(crate::text_render::TextWeight::Bold);
        let text_x = panel.x0 + inset;
        let title_y = panel.y0 + inset + grip_len / 2.0;
        ctx.text.draw_style(
            ctx.scene,
            &self.entry.title,
            theme.tokens.colors.text_dim,
            ctx.transform.pre_translate((text_x, title_y).into()),
            None,
            &title_style,
        );
        let (_, title_h) = ctx
            .text
            .measure_style(&self.entry.title, None, &title_style);
        let reading_y = title_y + f64::from(title_h) + f64::from(theme.tokens.spacing.rhythm.tight);
        ctx.text.draw_style(
            ctx.scene,
            &self.entry.reading,
            theme.tokens.colors.text_primary,
            ctx.transform.pre_translate((text_x, reading_y).into()),
            None,
            &reading_style,
        );
        let mut controls = self.control_row(panel, theme);
        let target = Self::action_height(theme);
        let close = Rect::new(
            panel.x1 - inset - target,
            panel.y0 + inset / 2.0,
            panel.x1 - inset,
            panel.y0 + inset / 2.0 + target,
        );
        controls.close = close;
        // The pause is the primary action; the add and the close stand beside it.
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(controls.toggle, &self.labels.toggle)
                .role(ButtonRole::Primary)
                .icon(if self.entry.running {
                    Icon::Pause
                } else {
                    Icon::Play
                })
                .hovered(self.hovered == Some(AgendaControl::Toggle))
                .focused(self.focused == Some(AgendaControl::Toggle)),
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(controls.add, &self.labels.add)
                .icon(Icon::Plus)
                .hovered(self.hovered == Some(AgendaControl::Add))
                .focused(self.focused == Some(AgendaControl::Add)),
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::icon_only(close, Icon::X, &self.labels.close)
                .role(ButtonRole::Plain)
                .hovered(self.hovered == Some(AgendaControl::Close))
                .focused(self.focused == Some(AgendaControl::Close)),
        );
        (
            AgendaPopoverLayout {
                panel,
                header: self.header(panel, theme),
                toggle: controls.toggle,
                add: controls.add,
                close,
            },
            layout,
        )
    }

    /// Where the two wide controls stand: at the panel's foot, a gutter in
    /// from its sides, each half its width.
    fn control_row(&self, panel: Rect, theme: &Theme) -> AgendaPopoverLayout {
        let inset = f64::from(theme.tokens.spacing.rhythm.section);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let height = Self::action_height(theme);
        let left = panel.x0 + inset;
        let right = panel.x1 - inset;
        let width = ((right - left) - gap) / 2.0;
        let y1 = panel.y1 - inset;
        let y0 = y1 - height;
        AgendaPopoverLayout {
            panel,
            header: self.header(panel, theme),
            toggle: Rect::new(left, y0, left + width, y1),
            add: Rect::new(left + width + gap, y0, right, y1),
            close: Rect::ZERO,
        }
    }

    /// The drag handle's rect: the panel's top band, where a pointer takes
    /// hold of it.
    #[must_use]
    pub fn header(&self, panel: Rect, theme: &Theme) -> Rect {
        let reach = f64::from(theme.tokens.spacing.rhythm.section)
            + f64::from(theme.text(TextRole::Label).line_height_px());
        Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + reach)
    }

    /// The control under `point`, when one is.
    #[must_use]
    pub fn control_at(layout: &AgendaPopoverLayout, point: Point) -> Option<AgendaControl> {
        if layout.toggle.contains(point) {
            Some(AgendaControl::Toggle)
        } else if layout.add.contains(point) {
            Some(AgendaControl::Add)
        } else if layout.close.contains(point) {
            Some(AgendaControl::Close)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::{TextRenderer, TextStyle};
    use vello::kurbo::Affine;

    fn entry() -> AgendaEntry {
        AgendaEntry::new("Tea", "2:05")
    }

    fn labels() -> AgendaLabels {
        AgendaLabels::of(&entry())
    }

    #[test]
    fn a_popover_is_as_tall_as_its_headline_reading_and_controls() {
        let theme = Theme::dark();
        // A panel the host sized to the popover's own preference: the
        // floating timer's established 150 px, clamped to what the window
        // gives it.
        let preferred = AgendaPopoverFace::height(&theme).max(150.0);
        let panel = Rect::from_origin_size(Point::ZERO, (252.0, preferred));
        let mut scene = vello::Scene::new();
        let mut text = TextRenderer::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let entry = entry();
        let labels = labels();
        let face = AgendaPopoverFace::new(&entry, &labels);
        let (layout, _) = face.paint(&mut ctx, panel);
        assert!(layout.panel.height() >= preferred - 1.0);
        assert!(layout.toggle.width() > 0.0 && layout.add.width() > 0.0);
        assert!(layout.close.width() > 0.0);
        // The controls stand inside the panel, and apart.
        for control in [layout.toggle, layout.add, layout.close] {
            assert!(panel.contains_rect(control), "a control stays on the panel");
        }
        assert!(
            layout.toggle.x1 <= layout.add.x0,
            "the two wide controls sit apart"
        );
    }

    #[test]
    fn the_pause_offers_to_resume_a_paused_entry() {
        let running = AgendaLabels::of(&entry());
        assert_eq!(running.toggle, "Pause");
        let paused = AgendaLabels::of(&entry().paused());
        assert_eq!(paused.toggle, "Resume");
    }

    #[test]
    fn a_press_is_routed_by_the_layout_it_was_painted_in() {
        let theme = Theme::dark();
        let panel = Rect::from_origin_size(Point::ZERO, (252.0, AgendaPopoverFace::height(&theme)));
        let mut scene = vello::Scene::new();
        let mut text = TextRenderer::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let entry = entry();
        let labels = labels();
        let face = AgendaPopoverFace::new(&entry, &labels);
        let (layout, _) = face.paint(&mut ctx, panel);
        assert_eq!(
            AgendaPopoverFace::control_at(&layout, layout.toggle.center()),
            Some(AgendaControl::Toggle)
        );
        assert_eq!(
            AgendaPopoverFace::control_at(&layout, layout.add.center()),
            Some(AgendaControl::Add)
        );
        assert_eq!(
            AgendaPopoverFace::control_at(&layout, layout.close.center()),
            Some(AgendaControl::Close)
        );
        assert_eq!(
            AgendaPopoverFace::control_at(&layout, layout.panel.center()),
            None
        );
    }

    #[test]
    fn the_panel_drags_by_its_header() {
        let theme = Theme::dark();
        let panel = Rect::from_origin_size(Point::new(100.0, 80.0), (252.0, 140.0));
        let entry = entry();
        let labels = labels();
        let face = AgendaPopoverFace::new(&entry, &labels);
        let header = face.header(panel, &theme);
        assert!(
            header.contains(Point::new(220.0, 90.0)),
            "the title band drags"
        );
        assert!(
            !header.contains(Point::new(220.0, 200.0)),
            "its controls do not"
        );
    }
}
