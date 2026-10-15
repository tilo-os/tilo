//! The existing event's nonmodal, trailing details and attached-notes pane.
//! The same rich editor and document binding serve this pane and the full form.

use ui::Widget;
use ui::a11y::{AccessNode, Role};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use super::{Action, CalendarApp, FormField, FormRects, Sel};
use crate::date;
use crate::draw_util::{hairline, text_in_band};
use crate::model::Event;

#[derive(Default)]
pub(super) struct EventInspector {
    /// Explicit event selection requests inspection; creation and staged full
    /// editors do not. Kept across resizes so widening restores the pane.
    pub(super) requested: bool,
    layout: Option<InspectorLayout>,
}

#[derive(Clone)]
struct InspectorLayout {
    header: Rect,
    done: Rect,
    title: Rect,
    date: Rect,
    time: Rect,
    location: Rect,
    edit: Rect,
    notes_label: Rect,
    notes: Rect,
}

impl CalendarApp {
    /// Reserved trailing content, excluding the window's shared header. This
    /// uses the unsplit content rect, never `view_rect`, so the latter can
    /// safely trim its right edge to this rect's x0.
    pub(super) fn inspector_rect(&self) -> Option<Rect> {
        if !self.detail_open || !self.inspector.requested {
            return None;
        }
        let Some(Sel::Event(id)) = self.selection else {
            return None;
        };
        self.model.event(id)?;
        if self
            .fresh_draft
            .as_ref()
            .is_some_and(|draft| draft.id == id)
        {
            return None;
        }
        let scale = f64::from(self.layout_theme.ui_scale * self.layout_theme.text_scale);
        // Keep a readable notes column without consuming the agenda at larger
        // UI scales: at 1440px / 150%, 420px navigation leaves 480px of agenda.
        let width = (360.0 * scale).clamp(520.0, 540.0);
        let top = self.topbar.view_rect().y0;
        let content = self.content_rect;
        (content.width() >= width + 280.0 * scale && content.y1 - top >= 420.0 * scale)
            .then(|| Rect::new(content.x1 - width, top, content.x1, content.y1))
    }

    /// Only the full event/task sheet suppresses the underlying calendar,
    /// navigation controls and the host's ordinary focus handling.
    pub(super) fn detail_is_modal(&self) -> bool {
        self.detail_open && self.inspector_rect().is_none()
    }

    pub(super) fn event_form_active(&self) -> bool {
        self.detail_is_modal() && matches!(self.selection, Some(Sel::Event(_)))
    }

    pub(super) fn edit_selected_event(&mut self) {
        if !matches!(self.selection, Some(Sel::Event(_))) {
            return;
        }
        self.inspector.requested = false;
        self.blur_editor();
        self.form_event = None;
        self.ensure_form_bound();
        self.open_detail();
        self.focus_field(FormField::Title);
    }

    pub(super) fn inspector_notes_region(&self) -> Option<Rect> {
        self.inspector_rect()?;
        self.inspector.layout.as_ref().map(|layout| layout.notes)
    }

    /// Compact, fixed summary above an independently scrolling rich field.
    pub(super) fn layout_inspector(&mut self, rect: Rect) {
        let theme = &self.layout_theme;
        let spacing = &theme.tokens.spacing;
        let padding = f64::from(spacing.rhythm.group);
        let gap = f64::from(spacing.rhythm.related);
        let section = f64::from(spacing.rhythm.section);
        let row_h = ControlSize::Regular.height(theme);
        let body = Rect::new(
            rect.x0 + padding,
            rect.y0 + padding,
            rect.x1 - padding,
            rect.y1 - padding,
        );
        let done_w = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            ButtonFace::new(Rect::ZERO, "Done").intrinsic_width(text, theme)
        });
        let done = Rect::new(body.x1 - done_w, body.y0, body.x1, body.y0 + row_h);
        let header = Rect::new(body.x0, body.y0, done.x0 - gap, done.y1);
        let title_y = header.y1 + gap;
        let title = Rect::new(
            body.x0,
            title_y,
            body.x1,
            title_y + f64::from(theme.text(TextRole::Heading).line_height_px()),
        );
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let date = Rect::new(body.x0, title.y1 + gap, body.x1, title.y1 + gap + line);
        let time = Rect::new(body.x0, date.y1, body.x1, date.y1 + line);
        let location = Rect::new(body.x0, time.y1, body.x1, time.y1 + line);
        let edit_w = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            ButtonFace::new(Rect::ZERO, &crate::resource::t("calendar-inspector-edit"))
                .intrinsic_width(text, theme)
        });
        let edit = Rect::new(
            body.x0,
            location.y1 + gap,
            body.x0 + edit_w,
            location.y1 + gap + row_h,
        );
        let notes_y = edit.y1 + section;
        let notes_label = Rect::new(
            body.x0,
            notes_y,
            body.x1,
            notes_y + f64::from(theme.text(TextRole::Subheading).line_height_px()),
        );
        let notes = Rect::new(
            body.x0,
            notes_label.y1 + gap,
            body.x1,
            body.y1.max(notes_label.y1 + gap),
        );
        self.detail_scroll_max = (self.notes_content_h - notes.height()).max(0.0);
        self.detail_scroll_y = self.detail_scroll_y.clamp(0.0, self.detail_scroll_max);
        self.detail_rect = rect;
        self.modal = None;
        self.form = FormRects::default();
        self.form.notes_label = notes_label;
        self.form.notes_top = notes.y0;
        self.notes_w = notes.width();
        self.notes_origin = Point::new(notes.x0, notes.y0 - self.detail_scroll_y);
        self.inspector.layout = Some(InspectorLayout {
            header,
            done,
            title,
            date,
            time,
            location,
            edit,
            notes_label,
            notes,
        });
    }

    pub(super) fn draw_inspector(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(rect) = self.inspector_rect() else {
            return;
        };
        let Some(layout) = self.inspector.layout.clone() else {
            return;
        };
        let Some(event) = self
            .selected_event()
            .and_then(|id| self.model.event(id))
            .cloned()
        else {
            return;
        };
        let ground = ui::chrome::panel_ground(theme);
        let pane = theme.on_ground(ground);
        scene.fill(Fill::NonZero, t, ground, None, &rect);
        hairline(
            scene,
            t,
            ground,
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x0, rect.y1),
        );
        // Do not clear the hit map or add a dismiss scrim: all content to the
        // left remains visible, semantically reachable and interactive.
        self.hits.push((rect, Action::Noop));
        let mut ctx = PaintCtx::new(scene, t, text, &pane);
        self.draw_inspector_summary(&mut ctx, &layout, &event);
        self.draw_inspector_notes(&mut ctx, &layout, presence);
    }

    fn draw_inspector_summary(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        layout: &InspectorLayout,
        event: &Event,
    ) {
        let pane = ctx.theme;
        let t = ctx.transform;
        for (label, band, role) in [
            (
                crate::resource::t("calendar-inspector-heading"),
                layout.header,
                TextRole::Subheading,
            ),
            (
                event.display_title().to_string(),
                layout.title,
                TextRole::Heading,
            ),
            (inspector_date(event), layout.date, TextRole::Body),
            (inspector_time(event), layout.time, TextRole::Body),
            (
                if event.location.is_empty() {
                    crate::resource::t("calendar-inspector-no-location")
                } else {
                    event.location.clone()
                },
                layout.location,
                TextRole::Body,
            ),
            (
                crate::resource::t("calendar-inspector-notes"),
                layout.notes_label,
                TextRole::Subheading,
            ),
        ] {
            text_in_band(
                ctx,
                &label,
                &pane.text(role),
                pane.text_primary,
                band.x0,
                band,
                band.width(),
            );
        }
        for (label, bounds, action, role) in [
            (
                "Done".to_string(),
                layout.done,
                Action::CloseDetail,
                ButtonRole::Primary,
            ),
            (
                crate::resource::t("calendar-inspector-edit"),
                layout.edit,
                Action::EditEvent,
                ButtonRole::Secondary,
            ),
        ] {
            Button::paint(
                ctx.scene,
                t,
                ctx.text,
                pane,
                &ButtonFace::new(bounds, &label)
                    .role(role)
                    .hovered(bounds.contains(self.last_mouse)),
            );
            self.hits.push((bounds, action));
        }
    }

    fn draw_inspector_notes(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        layout: &InspectorLayout,
        presence: &PresenceCtx,
    ) {
        let pane = ctx.theme;
        let t = ctx.transform;
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &layout.notes,
        );
        self.notes_editor
            .draw(ctx.scene, t, ctx.text, pane, presence);
        ctx.scene.pop_layer();
        if self.detail_scroll_max > 0.0 {
            let gap = f64::from(pane.tokens.spacing.rhythm.tight);
            let track = Rect::new(
                layout.notes.x1 + gap,
                layout.notes.y0,
                layout.notes.x1 + gap + f64::from(pane.tokens.metrics.scroll_thumb),
                layout.notes.y1,
            );
            ui::widget::Scrollbar::vertical().draw(
                ctx.scene,
                t,
                track,
                ui::widget::scrollbar::ScrollMetrics::new(
                    layout.notes.height() + self.detail_scroll_max,
                    layout.notes.height(),
                    self.detail_scroll_y,
                ),
                pane,
                false,
            );
        }
        self.notes_editor
            .draw_overlay(ctx.scene, t, ctx.text, pane, presence);
    }

    pub(super) fn inspector_accessibility(&self) -> Option<AccessNode> {
        let rect = self.inspector_rect()?;
        let event = self.model.event(self.selected_event()?)?;
        let mut notes = AccessNode::new(Role::Group)
            .with_name(crate::resource::t("calendar-inspector-notes"))
            .with_bounds(self.inspector_notes_region()?);
        if let Some(editor) = self.notes_editor.accessibility() {
            notes.children.push(editor);
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name(crate::resource::t("calendar-inspector-autosave"))
                .with_bounds(rect)
                .with_children(vec![
                    AccessNode::new(Role::StaticText).with_name(format!(
                        "{}. {}. {}. {}",
                        event.display_title(),
                        inspector_date(event),
                        inspector_time(event),
                        if event.location.is_empty() {
                            crate::resource::t("calendar-inspector-no-location")
                        } else {
                            event.location.clone()
                        }
                    )),
                    notes,
                ]),
        )
    }
}

fn inspector_date(event: &Event) -> String {
    let start = event.start.date();
    // iCalendar's all-day end is exclusive; show the last included date.
    let end = if event.all_day && event.end.date() > start {
        event.end.date() - chrono::Duration::days(1)
    } else {
        event.end.date()
    };
    if start == end {
        date::fmt_day_long(start)
    } else {
        format!(
            "{} – {}",
            date::fmt_day_short(start),
            date::fmt_day_short(end)
        )
    }
}

fn inspector_time(event: &Event) -> String {
    if event.all_day {
        crate::resource::t("calendar-inspector-all-day")
    } else {
        let time = format!(
            "{} – {}",
            event.start.format("%H:%M"),
            event.end.format("%H:%M")
        );
        if event.timezone.is_empty() || event.timezone == "floating" {
            time
        } else {
            format!("{time} ({})", event.timezone)
        }
    }
}
