//! Direct note entry beside the selection toolbar. The draft is a local CRDT;
//! selecting or dismissing a passage never writes an empty shared annotation.
use tilo_rich_text::{Draft, FieldOutcome, RichTextComposer, RichTextSurface, SubmitKey};
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::paint::PaintCtx;
use ui::widget::{BlockEditor, Button, ButtonFace, ButtonRole, ControlSize, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::input_controls::InputEventCtx;

pub(crate) struct SelectionComposer {
    host: RichTextComposer<Draft>,
    rect: Rect,
    cursor: Point,
    save: Rect,
}

impl Default for SelectionComposer {
    fn default() -> Self {
        Self {
            host: RichTextComposer::new(
                RichTextSurface::over(Draft::new())
                    .with_editor(
                        BlockEditor::new()
                            .with_wrap_lines(true)
                            .with_compact_paragraph_spacing(true)
                            .with_block_handles(false),
                    )
                    .with_placeholder("Write a note… Enter to save")
                    .with_submit(SubmitKey::Enter),
            ),
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            save: Rect::ZERO,
        }
    }
}

impl SelectionComposer {
    pub(crate) fn clear(&mut self) {
        self.host.widget_mut().source_mut().clear();
        self.host.widget_mut().set_focus(false);
        self.rect = Rect::ZERO;
        self.save = Rect::ZERO;
    }

    pub(crate) fn text(&self) -> String {
        self.host.widget().source().plain_text()
    }

    pub(crate) fn clip_blocks(&self) -> Vec<tilo_document::ClipBlock> {
        self.host.widget().source().clip_blocks()
    }

    pub(crate) fn accessibility_nodes(&self) -> Vec<AccessNode> {
        let mut nodes = self.accessibility().into_iter().collect::<Vec<_>>();
        if self.rect != Rect::ZERO && !self.text().trim().is_empty() {
            nodes.push(
                AccessNode::new(Role::Button)
                    .with_name("Save note")
                    .with_bounds(self.save)
                    .with_action_target("books.selection.note.save")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(self.text().trim().is_empty()),
                    ),
            );
        }
        nodes
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if self.rect == Rect::ZERO {
            return None;
        }
        self.host.widget().accessibility().map(|node| {
            node.with_name("Note on selected passage")
                .with_bounds(self.rect)
                .with_action_target("books.selection.note")
        })
    }

    pub(crate) fn perform_action(
        &mut self,
        action: &Action,
        edits: &mut ui::widget::Edits<'_>,
        root: &ui::automerge::ObjId,
    ) -> bool {
        self.rect != Rect::ZERO
            && self
                .host
                .widget_mut()
                .perform_action(action, &[], edits, root)
    }

    pub(crate) fn handle_event(&mut self, ctx: &mut InputEventCtx<'_>) -> Option<FieldOutcome> {
        if let WindowEvent::CursorMoved { position, .. } = ctx.event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.rect == Rect::ZERO {
            return None;
        }
        if self.save.contains(self.cursor)
            && !self.text().trim().is_empty()
            && matches!(
                ctx.event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            return Some(FieldOutcome::Submit);
        }
        let focused = self.host.widget().editor().is_focused();
        let pointer = matches!(
            ctx.event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if pointer && !self.rect.contains(self.cursor) {
            return None;
        }
        if !pointer && !focused {
            return None;
        }
        if matches!(
            ctx.event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            self.host.widget_mut().set_focus(true);
        }
        self.host.sync(ctx.state.doc(), ctx.root);
        self.host.handle_event(
            ctx.event,
            ctx.scale_factor,
            &ctx.modifiers,
            &mut ctx.state,
            ctx.root,
            ctx.handled,
        );
        // A consumed edit without submission still needs a redraw.
        self.host.widget_mut().take_outcome()
    }

    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        toolbar: Rect,
        bounds: Rect,
        selection: Option<Rect>,
    ) {
        self.host.widget_mut().present_now();
        let gap = f64::from(ctx.theme.tokens.spacing.s2);
        let pad = f64::from(ctx.theme.tokens.spacing.s3);
        let width = toolbar.width().min(bounds.width() - 2.0 * gap).max(1.0);
        let above = (toolbar.y0 - bounds.y0 - 2.0 * gap).max(0.0);
        let below = (bounds.y1 - toolbar.y1 - 2.0 * gap).max(0.0);
        let room = above.max(below);
        let line = f64::from(ctx.theme.text(ui::TextRole::Body).line_height_px());
        if room < line + 2.0 * pad {
            self.rect = Rect::ZERO;
            return;
        }
        let theme = ctx.theme.on_ground(ui::chrome::panel_ground(ctx.theme));
        let button = ButtonFace::new(Rect::ZERO, "Save")
            .role(ButtonRole::Primary)
            .control_size(ControlSize::Small);
        let button_w = button.intrinsic_width(ctx.text, &theme);
        let content = self
            .host
            .measure(
                width - 3.0 * pad - button_w,
                ui::num::f32_of_f64((room - 2.0 * pad).min(6.0 * line)),
                &theme,
                ctx.text,
            )
            .max(line);
        let height = content.max(ControlSize::Small.height(&theme)) + 2.0 * pad;
        let prefer_above = selection.is_some_and(|rect| toolbar.y1 <= rect.y0);
        let side = if (prefer_above && above >= height) || below < height {
            ui::widget::floating_surface::PopoverSide::Above
        } else {
            ui::widget::floating_surface::PopoverSide::Below
        };
        let (rect, _) = ui::widget::floating_surface::place_bounded(
            toolbar,
            (width, height),
            side,
            gap,
            bounds,
        );
        ui::widget::floating_surface::paint(ctx.scene, ctx.transform, rect, ctx.theme);
        self.rect = rect;
        self.save = Rect::new(
            rect.x1 - pad - button_w,
            rect.y1 - pad - ControlSize::Small.height(&theme),
            rect.x1 - pad,
            rect.y1 - pad,
        );
        let input = Rect::new(
            rect.x0 + pad,
            rect.y0 + pad,
            self.save.x0 - pad,
            rect.y1 - pad,
        );
        self.host.layout(input, &theme, ctx.text);
        self.host.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &theme,
            &ui::presence::PresenceCtx::empty(),
        );
        if !self.text().trim().is_empty() {
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                &theme,
                &ButtonFace::new(self.save, "Save")
                    .role(ButtonRole::Primary)
                    .control_size(ControlSize::Small)
                    .hovered(self.save.contains(self.cursor)),
            );
        }
    }
}
