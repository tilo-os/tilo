//! The two cards that float over the calendar without changing what it
//! shows: the one-line quick add, and the keyboard shortcut sheet.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_dialog::{self, ModalAction, ModalHit, ModalLayout, ModalSpec};
use ui::widget::{TextInput, Widget as _, text_control_height};
use ui::{TextRole, paint::PaintCtx};

use crate::draw_util::text_in_band;
use ui::widget::DrawChild;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SheetAnswer {
    Add,
    Close,
}

pub(super) struct ShortcutRow {
    pub label: &'static str,
    pub keys: &'static str,
}

pub(super) const SHORTCUTS: &[ShortcutRow] = &[
    ShortcutRow {
        label: "calendar-help-month",
        keys: "M",
    },
    ShortcutRow {
        label: "calendar-help-week",
        keys: "W",
    },
    ShortcutRow {
        label: "calendar-help-day",
        keys: "D",
    },
    ShortcutRow {
        label: "calendar-help-agenda",
        keys: "A",
    },
    ShortcutRow {
        label: "calendar-help-today",
        keys: "T",
    },
    ShortcutRow {
        label: "calendar-help-previous",
        keys: "[",
    },
    ShortcutRow {
        label: "calendar-help-next",
        keys: "]",
    },
    ShortcutRow {
        label: "calendar-help-previous-day",
        keys: "{Left}",
    },
    ShortcutRow {
        label: "calendar-help-next-day",
        keys: "{Right}",
    },
    ShortcutRow {
        label: "calendar-help-previous-week",
        keys: "{Up}",
    },
    ShortcutRow {
        label: "calendar-help-next-week",
        keys: "{Down}",
    },
    ShortcutRow {
        label: "calendar-help-new-event",
        keys: "N",
    },
    ShortcutRow {
        label: "calendar-help-quick-add",
        keys: "{shift:N}",
    },
    ShortcutRow {
        label: "calendar-help-delete",
        keys: "{Delete}",
    },
    ShortcutRow {
        label: "calendar-help-close",
        keys: "{Esc}",
    },
    ShortcutRow {
        label: "calendar-help-shortcuts",
        keys: "?",
    },
];

pub(super) struct QuickAdd {
    open: bool,
    input: TextInput,
    title: String,
    add: String,
    cancel: String,
    layout: Option<ModalLayout>,
    lines: (Rect, Rect),
    summary: String,
}

impl QuickAdd {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            input: TextInput::local()
                .with_placeholder(crate::resource::t("calendar-quick-placeholder"))
                .with_accessible_name(crate::resource::t("calendar-quick-field"))
                .with_id("quick-add-field"),
            title: crate::resource::t("calendar-quick-title"),
            add: crate::resource::t("calendar-quick-add"),
            cancel: crate::resource::t("calendar-quick-cancel"),
            layout: None,
            lines: (Rect::ZERO, Rect::ZERO),
            summary: String::new(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self) {
        self.input.set_text("");
        self.summary.clear();
        self.open = true;
        self.input.set_focus(true);
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.input.set_focus(false);
        self.layout = None;
        self.lines = (Rect::ZERO, Rect::ZERO);
        self.input.apply_layout_external(Rect::ZERO);
    }

    pub(super) fn text(&self) -> String {
        self.input.value().to_owned()
    }

    pub(super) fn set_summary(&mut self, summary: String) {
        self.summary = summary;
    }

    pub(super) fn layout(&mut self, window: Rect, theme: &Theme) {
        if !self.open {
            self.layout = None;
            return;
        }
        let field_h =
            text_control_height(f64::from(theme.tokens.metrics.min_pointer_target), theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let actions = [
            ModalAction::primary(&self.add),
            ModalAction::secondary(&self.cancel),
        ];
        let title = self.title.clone();
        let (layout, line_h) = ui::text_render::with_layout_renderer(theme, |text| {
            let line = f64::from(
                text.measure_style("Ag", None, &theme.text(TextRole::Body))
                    .1,
            );
            let body_h = field_h + gap + line;
            (
                modal_dialog::layout(
                    window,
                    &ModalSpec::new(&title).actions(&actions).body_height(body_h),
                    text,
                    theme,
                ),
                line,
            )
        });
        let body = layout.body;
        let field = Rect::new(body.x0, body.y0, body.x1, (body.y0 + field_h).min(body.y1));
        self.input.apply_layout_external(field);
        let line_top = field.y1 + gap;
        self.lines = (
            field,
            Rect::new(
                body.x0,
                line_top,
                body.x1,
                (line_top + 2.0 * line_h).min(body.y1),
            ),
        );
        self.layout = Some(layout);
    }

    #[cfg(test)]
    pub(super) fn card(&self) -> Rect {
        self.layout
            .as_ref()
            .map_or(Rect::ZERO, |layout| layout.card)
    }

    pub(super) fn field_contains(&self, p: Point) -> bool {
        self.lines.0.contains(p)
    }

    pub(super) fn hit(&self, p: Point) -> Option<SheetAnswer> {
        let layout = self.layout.as_ref()?;
        match layout.hit(p)? {
            ModalHit::Action(0) => Some(SheetAnswer::Add),
            ModalHit::Action(_) | ModalHit::Close | ModalHit::Scrim => Some(SheetAnswer::Close),
            ModalHit::Card => None,
        }
    }

    pub(super) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let actions = [
            ModalAction::primary(&self.add),
            ModalAction::secondary(&self.cancel),
        ];
        let title = self.title.clone();
        let spec = ModalSpec::new(&title)
            .actions(&actions)
            .body_height(layout.body.height());
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        self.input
            .draw_child(scene, transform, text, &sheet, presence);
        let style = sheet.text(TextRole::Body);
        let band = self.lines.1;
        let mut ctx = PaintCtx::new(scene, transform, text, &sheet);
        let lines = crate::draw_util::wrap_lines(&mut ctx, &self.summary, &style, band.width(), 2);
        let mut top = band.y0;
        for line in lines {
            ctx.text(
                &line,
                &style,
                sheet.text_dim,
                Point::new(band.x0, top),
                None,
            );
            top += f64::from(style.line_height_px());
        }
        modal_dialog::paint_actions(scene, transform, text, theme, &layout, &spec);
    }
}

pub(super) struct ShortcutSheet {
    open: bool,
    title: String,
    layout: Option<ModalLayout>,
    rows: Vec<(Rect, Rect)>,
}

impl ShortcutSheet {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            title: crate::resource::t("calendar-help-title"),
            layout: None,
            rows: Vec::new(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn toggle(&mut self) {
        self.set_open(!self.open);
    }

    pub(super) fn set_open(&mut self, open: bool) {
        self.open = open;
        if !open {
            self.layout = None;
            self.rows.clear();
        }
    }

    #[cfg(test)]
    pub(super) fn card(&self) -> Rect {
        self.layout
            .as_ref()
            .map_or(Rect::ZERO, |layout| layout.card)
    }

    pub(super) fn hit(&self, p: Point) -> Option<SheetAnswer> {
        let layout = self.layout.as_ref()?;
        match layout.hit(p)? {
            ModalHit::Close | ModalHit::Scrim | ModalHit::Action(_) => Some(SheetAnswer::Close),
            ModalHit::Card => None,
        }
    }

    pub(super) fn layout(&mut self, window: Rect, theme: &Theme) {
        if !self.open {
            self.layout = None;
            self.rows.clear();
            return;
        }
        let title = self.title.clone();
        let gap = f64::from(theme.tokens.spacing.s1);
        let style = theme.text(TextRole::Body);
        let (layout, row, keys_w) = ui::text_render::with_layout_renderer(theme, |text| {
            let line = f64::from(text.measure_style("Ag", None, &style).1);
            let row = line + gap;
            let body_h = row * SHORTCUTS.len() as f64;
            let keys_w = SHORTCUTS
                    .iter()
                    .map(|row| {
                        f64::from(
                            text.measure_style(&ui::spell_shortcut(row.keys), None, &style)
                                .0,
                        )
                    })
                    .fold(0.0, f64::max)
                    // A gap on each side, so an elided label never runs into
                    // the keys it belongs to.
                    + 2.0 * gap;
            (
                modal_dialog::layout(
                    window,
                    &ModalSpec::new(&title).body_height(body_h),
                    text,
                    theme,
                ),
                row,
                keys_w,
            )
        });
        let body = layout.body;
        let keys_w = keys_w.min(body.width() * 0.7);
        self.rows = (0..SHORTCUTS.len())
            .map(|index| {
                let top = body.y0 + row * (index as f64);
                let band = Rect::new(body.x0, top, body.x1, (top + row - gap).min(body.y1));
                (
                    Rect::new(band.x0, band.y0, band.x1 - keys_w, band.y1),
                    // The keys' own column, one gap clear of the words so an
                    // elided label never runs into them.
                    Rect::new(band.x1 - keys_w + gap, band.y0, band.x1, band.y1),
                )
            })
            .collect();
        self.layout = Some(layout);
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let title = self.title.clone();
        let spec = ModalSpec::new(&title).body_height(layout.body.height());
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let style = sheet.text(TextRole::Body);
        let mut ctx = PaintCtx::new(scene, transform, text, &sheet);
        for (what, row) in SHORTCUTS.iter().zip(&self.rows) {
            text_in_band(
                &mut ctx,
                &crate::resource::t(what.label),
                &style,
                sheet.text_primary,
                row.0.x0,
                row.0,
                row.0.width(),
            );
            text_in_band(
                &mut ctx,
                &ui::spell_shortcut(what.keys),
                &style,
                sheet.text_dim,
                row.1.x0,
                row.1,
                row.1.width(),
            );
        }
    }
}

impl super::CalendarApp {
    pub(super) fn layout_sheets(&mut self) {
        let window = self.rect;
        let theme = self.layout_theme.clone();
        if self.quick_add.is_open() {
            let summary = crate::quick_add::parse(&self.quick_add.text(), self.now(), self.anchor)
                .summary(self.anchor);
            self.quick_add.set_summary(summary);
            self.quick_add.layout(window, &theme);
        }
        self.shortcuts.layout(window, &theme);
    }

    pub(super) fn commit_quick_add(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !self.quick_add.is_open() {
            return;
        }
        let line = self.quick_add.text();
        let quick = crate::quick_add::parse(&line, self.now(), self.anchor);
        let event = crate::model::Event {
            id: uuid::Uuid::new_v4(),
            title: quick.title.clone(),
            start: quick.start,
            end: quick.end,
            all_day: false,
            color: self.model.next_event_order_color(),
            reminder_min: Some(10),
            timezone: self.authoring_zone(),
            ..crate::model::Event::default()
        };
        self.quick_add.close();
        self.spawn_event(event, doc);
    }

    pub(super) fn close_sheets(&mut self) {
        self.quick_add.close();
        self.shortcuts.set_open(false);
    }

    pub(super) fn sheet_open(&self) -> bool {
        self.quick_add.is_open() || self.shortcuts.is_open()
    }
}

impl super::CalendarApp {
    pub(super) fn dispatch_to_sheet(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.handle_sheet_event(&mut crate::input_context::InputContext {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state.reborrow(),
            path: ctx.path,
            handled: ctx.handled,
            focus_target: ctx.focus_target,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        });
    }

    pub(super) fn handle_sheet_event(
        &mut self,
        input: &mut crate::input_context::InputContext<'_>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
        *input.handled = true;
        self.dirty = true;
        if self.shortcuts.is_open() {
            match input.event {
                WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            logical_key:
                                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                            ..
                        },
                    ..
                } => self.close_sheets(),
                // A press anywhere on or beside the sheet puts it away; the
                // sheet is the only thing that takes input while it is up.
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if self.shortcuts.hit(self.last_mouse).is_some() => self.close_sheets(),
                _ => {}
            }
            return;
        }
        match input.event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                        ..
                    },
                ..
            } => self.close_sheets(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
                        ..
                    },
                ..
            } => {
                let mut doc = input.state.reborrow();
                self.commit_quick_add(&mut doc);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let at = self.last_mouse;
                if self.quick_add.field_contains(at) {
                    self.quick_add
                        .input_mut()
                        .handle_event(input.event, &mut input.event_ctx());
                    return;
                }
                match self.quick_add.hit(at) {
                    Some(SheetAnswer::Add) => {
                        let mut doc = input.state.reborrow();
                        self.commit_quick_add(&mut doc);
                    }
                    Some(SheetAnswer::Close) => self.close_sheets(),
                    None => {}
                }
            }
            _ => {
                self.quick_add
                    .input_mut()
                    .handle_event(input.event, &mut input.event_ctx());
            }
        }
    }
}
