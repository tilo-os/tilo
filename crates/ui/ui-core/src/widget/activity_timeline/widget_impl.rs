use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::Key;

use crate::i18n::widget_text;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{ModelPath, TaffyTreeExt, Widget};

use super::{ActivityTimeline, FILTER_STRIP_HEIGHT, FocusedRow, PADDING, VisualRow, row_time};

impl Widget for ActivityTimeline {
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if !self.rect.contains(point) {
            return None;
        }
        (0..self.row_count()).find_map(|i| {
            // A band an agent ran says what the run called itself (its session's title) on
            // hover: detail, never the band's headline.
            if let Some(VisualRow::Group(g)) = self.rows.get(i) {
                let row = self.row_rect(i).filter(|row| row.contains(point))?;
                let title = self.row_groups.get(*g)?.work_title.clone()?;
                return Some((row, format!("‘{title}’")));
            }
            let face = self.avatar_rect(i).filter(|face| face.contains(point))?;
            let entry = self.row_entry(i)?;
            // The person, then what wrote for them, a fact to a line: the agent, its name, its
            // session's title, the computer and the folder.
            let mut tip = self.author_label(entry);
            if let Some(writer) = crate::history_source::sentence::attribution_hover(&entry.author)
            {
                tip.push('\n');
                tip.push_str(&writer);
            }
            Some((face, tip))
        })
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.note_frame_theme(theme);
        if !self.fill {
            // Its strip, its rows and its padding: the page scrolls it with the rest.
            let height = self.content_height() + FILTER_STRIP_HEIGHT + PADDING * 3.0;
            let style = taffy::Style {
                size: taffy::Size {
                    width: taffy::style_helpers::percent(1.0_f32),
                    height: taffy::style_helpers::length(crate::num::f32_of_f64(height)),
                },
                ..Default::default()
            };
            return taffy.widget_leaf(style, node_map, self);
        }
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let prev = self.rect;
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if prev != self.rect {
            self.dirty = true;
        }
        let theme = self.face_theme.clone();
        self.place_controls(&theme);
    }

    /// Dirty while what it places may have moved: a selection, a name being
    /// typed, a filter, a list that changed. A leaf that always answered
    /// "dirty" re-solved its box every frame for a strip and a bar nobody
    /// moved.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.note_frame_theme(theme);
        if self.draws_own_surface {
            self.draw_surface_chrome(scene, transform, theme);
        }

        self.draw_filter_strip(scene, transform, text_renderer, theme);

        // Nothing-has-happened and filter-is-hiding-activity are distinct
        // cases; rendering them identically reads as broken.
        if self.rows.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme);
            self.dirty = false;
            return;
        }

        self.draw_rows(scene, transform, text_renderer, theme);

        self.draw_selection_bar(scene, transform, text_renderer, theme);

        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::Ime(_) if self.naming.is_some() => {
                self.handle_naming_ime(event, *ctx.modifiers, ctx.handled);
            }
            // A name field owns the keyboard while it is up, so a version
            // called "edits" cannot accidentally re-filter the pane.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.naming.is_some()
                && !ctx.modifiers.state().control_key()
                && !ctx.modifiers.state().super_key() =>
            {
                self.handle_naming_key(event, *ctx.modifiers, ctx.handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && c.eq_ignore_ascii_case("c")
                && self
                    .focused_row()
                    .and_then(|row| self.row_face(row))
                    .is_some_and(|face| face.copy.is_some()) =>
            {
                *ctx.handled = self.focused_row().is_some_and(|row| self.copy_row(row));
            }
            // Author typeahead. Chorded keys are left alone so the host's
            // own shortcuts still work while the pane has the keyboard.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !ctx.modifiers.state().control_key()
                && !ctx.modifiers.state().super_key()
                && !ctx.modifiers.state().alt_key() =>
            {
                self.handle_timeline_navigation_key(
                    logical_key,
                    ctx.modifiers.state().shift_key(),
                    ctx.handled,
                );
                if !*ctx.handled {
                    self.handle_author_typeahead_key(logical_key, ctx.handled);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) if self.naming.is_none() => {
                let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
                if !text.is_empty() {
                    self.author_query.push_str(&text);
                    self.rebuild();
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(ctx.handled, ctx.focus_target, ctx.node_id, *ctx.modifiers);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, ctx.handled);
            }
            _ => {}
        }
    }

    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.follow_clock()
    }

    fn serve_deadline(&mut self, _now: crate::time::Instant) -> bool {
        self.follow_clock()
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        let wait = self
            .times_change_at()?
            .duration_since(crate::time::wall())
            .unwrap_or_default();
        crate::time::now().checked_add(wait)
    }

    fn name(&self) -> &'static str {
        "ActivityTimeline"
    }

    fn is_focusable(&self) -> bool {
        !self.rows.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.has_focus != focused {
            self.has_focus = focused;
            if focused && self.focused_row.is_none() && !self.rows.is_empty() {
                let _ = self.focus_row(0);
            }
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.has_focus
    }

    /// The description carries both counts, because "7 items" over a list
    /// of three collapsed bands would not describe what is on screen.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let groups = self
            .rows
            .iter()
            .filter(|r| matches!(r, VisualRow::Group(_)))
            .count();
        let description = if groups == 0 {
            format!("{} entries", self.visible.len())
        } else {
            format!("{} entries in {} groups", self.visible.len(), groups)
        };
        let row_count = self.rows.len();
        let children = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let bounds = self.row_rect_unclipped(index)?;
                match row {
                    VisualRow::Group(group) => {
                        let item = self.row_groups.get(*group)?;
                        let description = item.spoken_facts();
                        Some(
                            AccessNode::new(Role::TreeItem)
                                .with_action_target(format!("timeline-group:{}", item.key))
                                .with_name(item.title.clone())
                                .with_description(description)
                                .with_position(index.saturating_add(1), row_count)
                                .with_level(item.depth.saturating_add(1))
                                .with_bounds(bounds)
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(
                                            self.focused_row.as_ref()
                                                == Some(&FocusedRow::Group(item.key.clone())),
                                        )
                                        .expanded(item.expanded),
                                ),
                        )
                    }
                    VisualRow::Entry(entry, depth) => {
                        let item = self.entries.get(*entry)?;
                        let id = crate::HistoryEntryId::hex(&item.id);
                        let said = if item.is_genesis() {
                            self.summary_of(item).to_owned()
                        } else {
                            format!(
                                "{}: {}",
                                crate::history_source::sentence::author_line(&item.author),
                                self.summary_of(item)
                            )
                        };
                        let name = Self::entry_breadcrumb(item).map_or_else(
                            || said.clone(),
                            |breadcrumb| format!("{breadcrumb}: {said}"),
                        );
                        Some(
                            AccessNode::new(Role::ListItem)
                                .with_action_target(format!("timeline-entry:{id}"))
                                .with_name(name)
                                .with_description(
                                    [
                                        item.entry_kind.glyph().to_owned(),
                                        row_time(item, self.now()),
                                    ]
                                    .into_iter()
                                    .filter(|part| !part.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                                )
                                .with_position(index.saturating_add(1), row_count)
                                .with_level(depth.saturating_add(1))
                                .with_bounds(bounds)
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(
                                            self.focused_row.as_ref()
                                                == Some(&FocusedRow::Entry(item.id)),
                                        )
                                        .selected(self.selection.is_some_and(|(start, end)| {
                                            *entry >= start && *entry <= end
                                        })),
                                ),
                        )
                    }
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name(widget_text("history-entries"))
                .with_description(description)
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.has_focus),
                )
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let row = if let Some(key) = target.strip_prefix("timeline-group:") {
            self.rows
                .iter()
                .enumerate()
                .find_map(|(index, row)| match row {
                    VisualRow::Group(group)
                        if self
                            .row_groups
                            .get(*group)
                            .is_some_and(|item| item.key == key) =>
                    {
                        Some(index)
                    }
                    _ => None,
                })
        } else if let Some(hex_id) = target.strip_prefix("timeline-entry:") {
            self.rows
                .iter()
                .enumerate()
                .find_map(|(index, row)| match row {
                    VisualRow::Entry(entry, _)
                        if self
                            .entries
                            .get(*entry)
                            .is_some_and(|item| crate::HistoryEntryId::hex(&item.id) == hex_id) =>
                    {
                        Some(index)
                    }
                    _ => None,
                })
        } else {
            None
        };
        let Some(row) = row else {
            return false;
        };
        match action {
            Action::Focus => self.focus_row(row),
            Action::Activate => self.activate_row(row),
            Action::ScrollIntoView => {
                self.scroll_row_into_view(row);
                true
            }
            Action::Expand
                if self
                    .group_at_row(row)
                    .is_some_and(|(_, expanded)| !expanded) =>
            {
                self.toggle_group_at_row(row)
            }
            Action::Collapse if self.group_at_row(row).is_some_and(|(_, expanded)| expanded) => {
                self.toggle_group_at_row(row)
            }
            _ => false,
        }
    }

    fn test_id(&self) -> Option<&str> {
        Some("activity_timeline")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.naming
            .as_ref()
            .and_then(|naming| naming.field.ime_cursor_rect())
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let naming = self.naming.as_ref()?;
        let scope = format!("{:?}:{:?}", naming.target, self.selection);
        Some(crate::ime::scoped_fingerprint(
            scope.as_bytes(),
            naming.field.ime_edit_fingerprint(key)?,
            key,
        ))
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(crate::ime::InputContent::TEXT)
    }

    crate::redraw_flag!();
}
