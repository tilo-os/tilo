use super::flow::{Flow, flow_builders, flow_capabilities, flow_widget_methods};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent layout flags model orthogonal sizing choices"
)]
#[tilo_ui_base::component::not_a_component(
    layout,
    "the language's `stack` (`column` is its older name)"
)]
pub struct Column {
    flow: Flow,
    extra_left_padding: f32,
    extra_top_padding: f32,
    extra_bottom_padding: f32,
    no_padding: bool,
    vertical_padding_override: Option<f32>,
    horizontal_padding_override: Option<(f32, f32)>,
    intrinsic_height: bool,
    intrinsic_width: bool,
    fill_remaining_height: bool,
}

impl Default for Column {
    fn default() -> Self {
        Self::new()
    }
}

impl Column {
    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    /// This keeps the widget's vtable instantiation out of the app.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn tilo_ui_base::widget::Widget> {
        Box::new(self)
    }

    pub fn new() -> Self {
        Self {
            flow: Flow::new(),
            extra_left_padding: 0.0,
            extra_top_padding: 0.0,
            extra_bottom_padding: 0.0,
            no_padding: false,
            vertical_padding_override: None,
            horizontal_padding_override: None,
            intrinsic_height: false,
            intrinsic_width: false,
            fill_remaining_height: false,
        }
    }

    flow_builders!();

    /// Resolve the column's height from its content (`auto`) instead of
    /// filling the parent (`percent(1.0)`). Required when wrapping the
    /// column in a `Scrollable` — see the field doc on
    /// `Column::intrinsic_height` for the gory details.
    #[must_use]
    pub fn with_intrinsic_height(mut self) -> Self {
        self.intrinsic_height = true;
        self
    }

    /// Size the column's width from its widest child instead of filling the
    /// parent: a column of buttons or labels beside other content in a
    /// row, a popover body. Children still align to its leading edge.
    #[must_use]
    pub fn with_intrinsic_width(mut self) -> Self {
        self.intrinsic_width = true;
        self
    }

    /// Skip the default `spacing.s6` inset on all sides. Used by
    /// the shell's foreground column so the topbar lands at y=0
    /// instead of inset by the column's gutter.
    #[must_use]
    pub fn with_no_padding(mut self) -> Self {
        self.no_padding = true;
        self
    }

    pub fn apply_graph_props(
        &mut self,
        id: &str,
        gap: Option<f32>,
        no_padding: bool,
        intrinsic_height: bool,
        fill_remaining_height: bool,
    ) {
        self.flow.id = Some(id.to_owned());
        self.flow.gap_override = gap;
        self.no_padding = no_padding;
        self.intrinsic_height = intrinsic_height;
        self.fill_remaining_height = fill_remaining_height;
        self.flow.invalidate();
    }

    #[must_use]
    pub fn with_extra_top_padding(mut self, px: f32) -> Self {
        self.extra_top_padding = px;
        self
    }

    pub fn set_extra_top_padding(&mut self, px: f32) {
        self.flow.style_dirty |= self.extra_top_padding.to_bits() != px.to_bits();
        self.extra_top_padding = px;
    }

    pub fn set_extra_bottom_padding(&mut self, px: f32) {
        self.flow.style_dirty |= self.extra_bottom_padding.to_bits() != px.to_bits();
        self.extra_bottom_padding = px;
    }

    /// Extra padding on the bottom, on top of the default
    /// `spacing.s6` inset.
    #[must_use]
    pub fn with_extra_bottom_padding(mut self, px: f32) -> Self {
        self.extra_bottom_padding = px;
        self
    }

    /// Override the column's top + bottom padding (default
    /// `spacing.s6`); left/right are unaffected. For layouts
    /// that need to hug the viewport's top/bottom edges.
    #[must_use]
    pub fn with_vertical_padding(mut self, px: f32) -> Self {
        self.vertical_padding_override = Some(px);
        self
    }

    pub fn set_vertical_padding(&mut self, px: f32) {
        self.flow.style_dirty |= self.vertical_padding_override != Some(px);
        self.vertical_padding_override = Some(px);
    }

    #[must_use]
    pub fn with_horizontal_padding(mut self, left: f32, right: f32) -> Self {
        self.horizontal_padding_override = Some((left, right));
        self
    }

    /// Override the vertical gap between children (default
    /// `theme.gap_standard`). `0.0` for a tightly banded stack whose
    /// children own their own internal spacing.
    #[must_use]
    pub fn with_gap(mut self, px: f32) -> Self {
        self.flow.gap_override = Some(px);
        self.flow.rhythm = None;
        self
    }

    pub fn set_gap(&mut self, px: f32) {
        self.flow.style_dirty |= self.flow.gap_override != Some(px) || self.flow.rhythm.is_some();
        self.flow.gap_override = Some(px);
        self.flow.rhythm = None;
    }

    /// Switch from `height: percent(1.0)` to `flex_grow: 1 + flex_basis: 0`.
    /// Use when the column is a flex sibling of a fixed-height widget (a
    /// topbar, say) — the percent variant would claim 100% of the parent on
    /// top of the sibling's height, overflowing and pushing the column's
    /// own bottom padding off-screen.
    #[must_use]
    pub fn with_fill_remaining_height(mut self) -> Self {
        self.fill_remaining_height = true;
        self
    }
}

impl Widget for Column {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        #[cfg(feature = "model")]
        dyn tilo_ui_base::widget::permission::PermissionGuarded,
        dyn tilo_ui_base::presence::capability::PublishesPresence
    );

    flow_widget_methods!();

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.flow.build_children(taffy, theme, node_map);
        self.flow.vertical = true;
        self.flow.laid_gap = f64::from(self.flow.gap(theme));
        self.flow.content_sized = self.intrinsic_height;

        let style = Style {
            display: if self.intrinsic_height {
                taffy::Display::Grid
            } else {
                taffy::Display::Flex
            },
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::FlexStart),
            // A parent may legitimately stretch or explicitly size this
            // Column taller than its content. Pack implicit Grid rows at
            // the start so surplus block space does not inflate the
            // requested gap between them.
            align_content: self.intrinsic_height.then_some(taffy::AlignContent::Start),
            // The one column is the Column's own width, never wider: an
            // `auto` track grows to its widest child's content and pushes
            // a padded child past the Column's edge.
            grid_template_columns: if self.intrinsic_height {
                vec![taffy::style_helpers::minmax(
                    length(0.0_f32),
                    taffy::style_helpers::fr(1.0_f32),
                )]
            } else {
                Vec::new()
            },
            flex_shrink: 0.0, // Prevent vertical squashing
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(self.flow.gap(theme)),
            },
            padding: {
                // The page gutter: the `s6` token (24). Tokens are the
                // source of truth; on every theme built through
                // `Theme::from_tokens` / `with_ui_scale` / a custom
                // spec this is exactly twice `padding_standard`. A host
                // theme that overrides the legacy `padding_standard`
                // alias alone (`Theme::with_component_scales` preserves
                // such aliases) no longer moves this gutter.
                let (base_x, base_y) = if self.no_padding {
                    (0.0, 0.0)
                } else {
                    self.flow
                        .rhythm_padding(theme)
                        .unwrap_or((theme.tokens.spacing.s6, theme.tokens.spacing.s6))
                };
                let vert = self.vertical_padding_override.unwrap_or(base_y);
                let (left, right) = self
                    .horizontal_padding_override
                    .unwrap_or((base_x + self.extra_left_padding, base_x));
                tilo_ui_base::widget::layout::insets(
                    vert + self.extra_top_padding,
                    right,
                    vert + self.extra_bottom_padding,
                    left,
                )
            },
            size: taffy::Size {
                width: if self.intrinsic_width {
                    taffy::Dimension::auto()
                } else {
                    percent(1.0_f32)
                },
                height: if self.intrinsic_height || self.fill_remaining_height {
                    taffy::Dimension::auto()
                } else {
                    percent(1.0_f32)
                },
            },
            ..Default::default()
        };
        // A column that fills what its parent's column leaves takes that height and no more: its
        // content scrolls or clips inside it rather than growing it past the parent.
        let style = tilo_ui_base::widget::layout::filling(
            style,
            if self.fill_remaining_height { 1.0 } else { 0.0 },
        );
        let node = taffy.parent_node(style, &self.flow.child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if let Some(shifts) = self.flow.follower_shifts() {
            let grows = shifts.last().copied().unwrap_or(0.0).max(0.0);
            let mut rect = self.flow.rect;
            if self.flow.content_sized {
                rect.y1 += grows;
            }
            regions.push(rect);
            return;
        }
        self.for_each_child_in_clip(&mut |child| child.collect_damage(regions));
    }

    fn name(&self) -> &'static str {
        "Column"
    }

    /// A content-sized column a band moves in grows with it, and so moves
    /// what follows it in its own container.
    fn disclosing(&self) -> Option<tilo_ui_base::widget::Disclosing> {
        self.flow.disclosing()
    }
}

flow_capabilities!(Column);

#[cfg(any(test, feature = "test-support"))]
impl Column {
    /// How far below its laid-out place each child is drawn this frame,
    /// then how much taller the column is drawn; `None` at rest.
    pub fn drawn_offsets(&self) -> Option<Vec<f64>> {
        self.flow.follower_shifts()
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Column {
    fn action_control_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        tilo_ui_base::widget::controls::children_action_control_bindings(self)
    }
    fn has_action_control_bindings(&self) -> bool {
        tilo_ui_base::widget::controls::children_have_action_control_bindings(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Container, Row, Text};
    use std::collections::HashMap;
    use tilo_ui_base::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::{Affine, Point};
    use vello_encoding::DrawTag;

    fn lay_out(root: &mut dyn Widget) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, Point::new(0.0, 0.0));
        (taffy, node)
    }

    fn glyph_count(root: &mut dyn Widget) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = tilo_ui_base::presence::PresenceCtx::default();
        let mut scene = Scene::new();
        // Draw through the column's cached-draw path (the buggy one) — that's where the leading Text was
        // dropped.
        root.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            presence.draw_env(),
        ));
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| matches!(**t, DrawTag::IMAGE) || **t == DrawTag::COLOR)
            .count()
    }

    tilo_test_env::scenario! {
        /// A column's rhythm, intrinsic sizing and leading text.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_column_resolves_its_gaps_insets_and_intrinsic_sizes;

        phase an_intrinsic_width_column_hugs_its_widest_child;
        {
            use tilo_ui_base::widget::Spacer;
            let mut row = Row::new().no_wrap().push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_intrinsic_width()
                    .push(Box::new(Spacer::fixed(40.0)))
                    .push(Box::new(Spacer::fixed(90.0))),
            ));
            lay_out(&mut row);
            let column = row.children()[0].get_rect();
            assert!((column.width() - 90.0).abs() < 0.5, "{column:?}");
        }

        phase intrinsic_column_gap_applies_between_children_and_to_content_height;
        {
            fn intrinsic_column_keeps_requested_gap_when_parent_stretches_height() {
                let theme = Theme::dark();
                let mut col = Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_gap(20.0)
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(10.0),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(10.0),
                    ));
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let mut tr = TextRenderer::new();
                let column_node = col.build_layout(&mut taffy, &theme, &mut node_map);
                let host_node = taffy.parent_node(
                    Style {
                        size: taffy::Size::from_lengths(400.0_f32, 300.0_f32),
                        ..Default::default()
                    },
                    &[column_node],
                );

                taffy
                    .compute_layout_with_measure(
                        host_node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(400.0),
                            height: taffy::AvailableSpace::Definite(300.0),
                        },
                        |known, available, node_id, _, _| {
                            if let Some(&ptr) = node_map.get(&node_id) {
                                unsafe { (*ptr).measure(known, available, &mut tr) }
                            } else {
                                taffy::Size::ZERO
                            }
                        },
                    )
                    .unwrap();
                col.apply_layout(&taffy, column_node, Point::ZERO);

                assert_eq!(taffy.layout(column_node).unwrap().size.height, 300.0);
                assert_eq!(col.get_rect().height(), 300.0);
                assert_eq!(col.flow.children[0].get_rect().y0, 0.0);
                assert_eq!(col.flow.children[1].get_rect().y0, 30.0);
                assert_eq!(
                    col.flow.children[1].get_rect().y0 - col.flow.children[0].get_rect().y1,
                    20.0
                );
            }

            intrinsic_column_keeps_requested_gap_when_parent_stretches_height();
        }

        phase intrinsic_column_gap_composes_with_padding_margins_nesting_and_scroll;
        {
            fn nested_intrinsic_column_keeps_taffy_and_widget_sibling_geometry_aligned() {
                let inner = Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_gap(20.0)
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ));
                let mut outer = Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_gap(0.0)
                    .push(Box::new(inner))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(10.0),
                    ));

                let (mut taffy, node) = lay_out(&mut outer);

                assert_eq!(taffy.children(node).unwrap(), outer.flow.child_nodes);
                assert_eq!(
                    taffy.layout(outer.flow.child_nodes[0]).unwrap().size.width,
                    800.0
                );
                assert_eq!(
                    taffy.layout(outer.flow.child_nodes[0]).unwrap().size.height,
                    130.0
                );
                assert_eq!(outer.flow.children[0].get_rect().width(), 800.0);
                assert_eq!(outer.flow.children[0].get_rect().height(), 130.0);
                assert_eq!(
                    taffy.layout(outer.flow.child_nodes[1]).unwrap().location.y,
                    130.0
                );
                assert_eq!(outer.flow.children[1].get_rect().y0, 130.0);
                assert_eq!(outer.get_rect().height(), 140.0);

                taffy
                    .compute_layout(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(800.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                    )
                    .unwrap();
                outer.apply_layout(&taffy, node, Point::ZERO);
                assert_eq!(
                    taffy.layout(outer.flow.child_nodes[0]).unwrap().size.height,
                    130.0
                );
                assert_eq!(outer.flow.children[0].get_rect().height(), 130.0);
                assert_eq!(outer.flow.children[1].get_rect().y0, 130.0);
            }

            fn intrinsic_column_in_row_respects_parent_center_alignment() {
                let short_column = Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_gap(0.0)
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(10.0),
                    ));
                let mut row = Row::new()
                    .no_wrap()
                    .push(Box::new(short_column))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ));

                let _ = lay_out(&mut row);

                assert_eq!(row.get_rect().height(), 30.0);
                assert_eq!(row.children()[0].get_rect().height(), 10.0);
                assert_eq!(row.children()[0].get_rect().y0, 10.0);
                assert_eq!(row.children()[1].get_rect().y0, 0.0);
            }

            fn intrinsic_column_gap_contributes_to_scrollable_content_height() {
                use crate::widget::Scrollable;
                use tilo_ui_base::widget::ScrollAxis;

                let inner = Column::new()
                    .with_intrinsic_height()
                    .with_no_padding()
                    .with_gap(20.0)
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_height(30.0),
                    ));
                let mut scroll = Scrollable::new(ScrollAxis::Vertical)
                    .with_height(50.0)
                    .with_child(Box::new(inner));

                let (taffy, scroll_node) = lay_out(&mut scroll);

                let inner = scroll.children()[0];
                let inner_node = taffy.children(scroll_node).unwrap()[0];
                assert_eq!(scroll.get_rect().height(), 50.0);
                assert_eq!(taffy.layout(inner_node).unwrap().size.height, 130.0);
                assert_eq!(inner.get_rect().height(), 130.0);
                assert_eq!(inner.children()[2].get_rect().y0, 100.0);
                assert_eq!(scroll.max_offset_y(), 80.0);
            }

            fn horizontal_scrollable_keeps_cross_axis_stretch() {
                use crate::widget::Scrollable;
                use tilo_ui_base::widget::ScrollAxis;

                let mut scroll = Scrollable::new(ScrollAxis::Horizontal)
                    .with_height(50.0)
                    .with_child(Box::new(
                        Container::new().with_no_padding().with_width(1_200.0),
                    ));

                let _ = lay_out(&mut scroll);

                assert_eq!(scroll.get_rect().height(), 50.0);
                assert_eq!(scroll.children()[0].get_rect().height(), 50.0);
            }

            nested_intrinsic_column_keeps_taffy_and_widget_sibling_geometry_aligned();
            intrinsic_column_in_row_respects_parent_center_alignment();
            intrinsic_column_gap_contributes_to_scrollable_content_height();
            horizontal_scrollable_keeps_cross_axis_stretch();
        }

        phase leading_text_in_columns_is_drawn_and_survives_redraw;
        {
            /// The settings scenario: a bare leading `Text` (NOT wrapped in a
            /// Row or Container) at the head of an intrinsic-height Column hosted
            /// inside a `Scrollable`. This is the exact shape Settings worked
            /// around by wrapping every heading in a background Container. The
            /// leading Text must resolve a non-degenerate rect through the measure
            /// pass.
            fn bare_leading_text_in_scrolled_intrinsic_column() {
                use crate::widget::Scrollable;
                use tilo_ui_base::widget::ScrollAxis;
                let inner = Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new("Section Heading")))
                    .push(Box::new(
                        Container::new()
                            .with_background()
                            .with_child(Box::new(Text::new("Row one"))),
                    ))
                    .push(Box::new(Text::new("Another Heading")))
                    .push(Box::new(
                        Container::new()
                            .with_background()
                            .with_child(Box::new(Text::new("Row two"))),
                    ));
                let mut scroll = Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(inner));
                let _ = lay_out(&mut scroll);

                let inner_col = &scroll.children()[0];
                let lead = inner_col.children()[0];
                let r = lead.get_rect();
                assert!(
                    r.width() > 1.0 && r.height() > 1.0,
                    "bare leading Text in a scrolled intrinsic Column collapsed: {r:?}"
                );

                // And it must actually paint.
                let fills = glyph_count(&mut scroll);
                assert!(fills >= 3, "expected headings + rows to paint; got {fills}");
            }

            bare_leading_text_in_scrolled_intrinsic_column();
        }
    }
}
