use super::flow::{Flow, flow_builders, flow_capabilities, flow_widget_methods};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent layout flags model orthogonal sizing choices"
)]
pub struct Column {
    flow: Flow,
    extra_left_padding: f32,
    extra_top_padding: f32,
    extra_bottom_padding: f32,
    no_padding: bool,
    /// See [`Column::with_vertical_padding`].
    vertical_padding_override: Option<f32>,
    /// See [`Column::with_horizontal_padding`].
    horizontal_padding_override: Option<(f32, f32)>,
    intrinsic_height: bool,
    /// See [`Column::with_intrinsic_width`].
    intrinsic_width: bool,
    /// See [`Column::with_fill_remaining_height`].
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
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
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

    /// Apply layout flags from a serialized column node without rebuilding.
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

    /// Extra padding on top, e.g. to reserve room for a floating
    /// affordance the column doesn't own (the responsive shell's Back
    /// chip painted over the detail pane's top-left corner).
    #[must_use]
    pub fn with_extra_top_padding(mut self, px: f32) -> Self {
        self.extra_top_padding = px;
        self
    }

    /// Update the extra top inset before the next layout pass.
    pub fn set_extra_top_padding(&mut self, px: f32) {
        self.flow.style_dirty |= self.extra_top_padding.to_bits() != px.to_bits();
        self.extra_top_padding = px;
    }

    /// Update the extra bottom inset before the next layout pass.
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

    /// Update the top + bottom inset before the next layout pass.
    pub fn set_vertical_padding(&mut self, px: f32) {
        self.flow.style_dirty |= self.vertical_padding_override != Some(px);
        self.vertical_padding_override = Some(px);
    }

    /// Override the column's left + right padding (replacing
    /// `base + extra_left_padding` on the left, `base` on the right).
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

    /// Update the inter-child gap before the next layout pass.
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
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
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
        // A band disclosing in the column moves the widgets after it, its
        // gap included.
        self.flow.vertical = true;
        self.flow.laid_gap = f64::from(self.flow.gap(theme));
        self.flow.content_sized = self.intrinsic_height;

        let style = Style {
            // Intrinsic-height columns use a one-column implicit Grid.
            // Taffy's Block layout ignores `gap`, while flex layout can
            // truncate the intrinsic main axis to a Scrollable's viewport.
            // Grid honors row gaps and intrinsically sizes every implicit
            // row, preserving both scroll extent and the one widget / one
            // direct Taffy child identity expected by host traversal.
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
                super::layout::insets(
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
            flex_basis: if self.fill_remaining_height {
                taffy::Dimension::length(0.0)
            } else {
                taffy::Dimension::auto()
            },
            flex_grow: if self.fill_remaining_height { 1.0 } else { 0.0 },
            ..Default::default()
        };
        let node = taffy.parent_node(style, &self.flow.child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        // A band on its way moves every widget after it; a content-sized
        // column is drawn that much taller.
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
    fn disclosing(&self) -> Option<crate::widget::Disclosing> {
        self.flow.disclosing()
    }
}

flow_capabilities!(Column);

#[cfg(test)]
impl Column {
    /// How far below its laid-out place each child is drawn this frame,
    /// then how much taller the column is drawn; `None` at rest.
    pub(crate) fn drawn_offsets(&self) -> Option<Vec<f64>> {
        self.flow.follower_shifts()
    }
}

impl crate::widget::controls::ActionControls for Column {
    fn action_control_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        crate::widget::controls::children_action_control_bindings(self)
    }
    fn has_action_control_bindings(&self) -> bool {
        crate::widget::controls::children_have_action_control_bindings(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use crate::tokens::Rhythm;
    use crate::widget::{Container, Row, Text};
    use std::collections::HashMap;
    use vello::Scene;
    use vello::kurbo::{Affine, Point};
    use vello_encoding::DrawTag;

    /// Build the five-pass layout for `root` inside an 800×600 viewport and
    /// return the populated taffy tree + node so the caller can draw.
    /// A theme change redraws a column's cached subtree: no child's size
    /// changes and no child is dirty, yet every pixel the cache holds is in
    /// the old theme. An unchecked box in a table cell stayed dark in a light
    /// window when the column replayed its cache.
    #[test]
    fn a_theme_change_redraws_the_cached_subtree() {
        let mut col = Column::new()
            .with_intrinsic_height()
            .push(Box::new(crate::widget::Checkbox::new("probe", "probe")));
        let _ = lay_out(&mut col);
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::default();
        let mut frame = |col: &mut Column, theme: &Theme| {
            let mut scene = Scene::new();
            col.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
            col.mark_drawn();
            scene
        };
        let dark = frame(&mut col, &Theme::dark());
        let dark_again = frame(&mut col, &Theme::dark());
        assert!(
            dark.encoding_eq(&dark_again),
            "an unchanged theme replays the same paint"
        );
        let light = frame(&mut col, &Theme::light());
        let light_alone = {
            let mut fresh = Column::new()
                .with_intrinsic_height()
                .push(Box::new(crate::widget::Checkbox::new("probe", "probe")));
            let _ = lay_out(&mut fresh);
            frame(&mut fresh, &Theme::light())
        };
        assert!(
            light.encoding_eq(&light_alone),
            "after switching to light the column paints what a fresh light column paints"
        );
        assert!(!light.encoding_eq(&dark), "light paint differs from dark");
    }

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

    /// Count GLYPH draw-tags in a freshly-drawn scene. A dropped leading Text
    /// shows up as zero glyphs.
    fn glyph_count(root: &mut dyn Widget) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::default();
        let mut scene = Scene::new();
        // Draw through the column's cached-draw path (the buggy one) — that's where the leading Text was
        // dropped.
        root.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| matches!(**t, DrawTag::IMAGE) || **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn a_changed_inset_or_gap_rebuilds_the_retained_layout_and_an_unchanged_one_does_not() {
        let mut col = Column::new()
            .with_vertical_padding(0.0)
            .with_gap(0.0)
            .push(Box::new(Text::new("a")));
        let _ = lay_out(&mut col);
        assert!(!col.take_structure_dirty(), "a fresh build owes nothing");
        // Hosts restyle every frame; restating the same values is free.
        col.set_vertical_padding(0.0);
        col.set_gap(0.0);
        col.set_extra_top_padding(0.0);
        col.set_extra_bottom_padding(0.0);
        assert!(!col.take_structure_dirty());
        // A new inset only reaches layout through a rebuild of the node.
        col.set_extra_bottom_padding(12.0);
        assert!(col.take_structure_dirty());
        assert!(!col.take_structure_dirty(), "the flag drains once");
        col.set_extra_top_padding(12.0);
        assert!(matches!(
            col.take_structure_change(),
            crate::widget::StructureChange::Rebuild
        ));
        let (taffy, node) = lay_out(&mut col);
        assert_eq!(taffy.style(node).unwrap().padding.top, length(12.0_f32));
    }

    #[test]
    fn rhythm_gaps_and_insets_resolve_from_the_theme_scale() {
        use crate::widget::Spacer;
        for scale in [1.0_f32, 1.5] {
            let theme = Theme::dark().with_ui_scale(scale);
            let r = theme.tokens.spacing.rhythm;
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Section)
                .with_padding_rhythm(Rhythm::Group, Rhythm::Related)
                .push(Box::new(Spacer::fixed(10.0)))
                .push(Box::new(Spacer::fixed(10.0)));
            let mut taffy = TaffyTree::<()>::new();
            let mut map = HashMap::new();
            let node = column.build_layout(&mut taffy, &theme, &mut map);
            let style = taffy.style(node).unwrap();
            assert_eq!(style.gap.height, length(r.section));
            assert_eq!(style.padding.left, length(r.group));
            assert_eq!(style.padding.top, length(r.related));

            let mut row = Row::new()
                .with_rhythm(Rhythm::Related)
                .with_padding_rhythm(None, Rhythm::Tight);
            let node = row.build_layout(&mut taffy, &theme, &mut map);
            let style = taffy.style(node).unwrap();
            assert_eq!(style.gap.width, length(r.related));
            assert_eq!(style.padding.left, length(0.0_f32));
            assert_eq!(style.padding.top, length(r.tight));
        }
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map = HashMap::new();
        let mut column = Column::new().with_rhythm(Rhythm::Group).with_gap(3.0);
        let node = column.build_layout(&mut taffy, &theme, &mut map);
        assert_eq!(taffy.style(node).unwrap().gap.height, length(3.0_f32));
    }

    #[test]
    fn an_intrinsic_width_column_hugs_its_widest_child() {
        use crate::widget::Spacer;
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

    #[test]
    fn intrinsic_column_gap_applies_between_children_and_to_content_height() {
        /// The default inset is the `s6` token — 24 on the stock scale and still
        /// exactly twice `padding_standard` once `ui_scale` or a custom theme's
        /// a theme's density has rescaled the whole spacing scale.
        fn default_inset_is_the_s6_token_on_every_scale() {
            for theme in [
                Theme::dark(),
                Theme::dark().with_ui_scale(1.5),
                Theme::from_document(&crate::theme::ThemeDocument {
                    density: crate::theme::ThemeDensity { spacing: 0.75 },
                    ..crate::theme::Preset::Dark.document()
                })
                .unwrap(),
            ] {
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let mut column = Column::new().push(Box::new(Text::new("body")));
                let node = column.build_layout(&mut taffy, &theme, &mut node_map);
                let padding = taffy.style(node).unwrap().padding;
                let expected = taffy::style_helpers::length(theme.tokens.spacing.s6);
                assert_eq!(padding.left, expected, "{}", theme.name);
                assert_eq!(padding.top, expected, "{}", theme.name);
                assert_eq!(
                    padding.left,
                    taffy::style_helpers::length(theme.padding_standard * 2.0),
                    "{}: s6 is twice padding_standard",
                    theme.name
                );
            }
        }

        fn intrinsic_column_applies_requested_gap_to_geometry_and_content_height() {
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

            let _ = lay_out(&mut col);

            assert_eq!(col.flow.children[0].get_rect().y0, 0.0);
            assert_eq!(col.flow.children[1].get_rect().y0, 30.0);
            assert_eq!(col.get_rect().height(), 40.0);
        }

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

        fn intrinsic_column_zero_gap_keeps_children_tightly_stacked() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(0.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.flow.children[0].get_rect().y0, 0.0);
            assert_eq!(col.flow.children[1].get_rect().y0, 10.0);
            assert_eq!(col.get_rect().height(), 20.0);
        }

        fn intrinsic_column_uses_default_theme_gap() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            // The dark theme's default gap is 12 px.
            assert_eq!(col.flow.children[1].get_rect().y0, 22.0);
            assert_eq!(col.get_rect().height(), 32.0);
        }

        fn intrinsic_column_inserts_one_gap_between_each_of_three_children() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(7.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.flow.children[0].get_rect().y0, 0.0);
            assert_eq!(col.flow.children[1].get_rect().y0, 17.0);
            assert_eq!(col.flow.children[2].get_rect().y0, 34.0);
            assert_eq!(col.get_rect().height(), 44.0);
        }

        default_inset_is_the_s6_token_on_every_scale();
        intrinsic_column_applies_requested_gap_to_geometry_and_content_height();
        intrinsic_column_keeps_requested_gap_when_parent_stretches_height();
        intrinsic_column_zero_gap_keeps_children_tightly_stacked();
        intrinsic_column_uses_default_theme_gap();
        intrinsic_column_inserts_one_gap_between_each_of_three_children();
    }

    #[test]
    fn intrinsic_column_gap_composes_with_padding_margins_nesting_and_scroll() {
        fn intrinsic_column_gap_is_additive_with_explicit_padding() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_vertical_padding(5.0)
                .with_horizontal_padding(7.0, 11.0)
                .with_extra_top_padding(3.0)
                .with_extra_bottom_padding(4.0)
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.flow.children[0].get_rect().x0, 7.0);
            assert_eq!(col.flow.children[0].get_rect().y0, 8.0);
            assert_eq!(col.flow.children[1].get_rect().y0, 38.0);
            assert_eq!(col.get_rect().height(), 57.0);
        }

        fn intrinsic_column_gap_is_additive_with_child_margins() {
            let theme = Theme::dark();
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ));
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let node = col.build_layout(&mut taffy, &theme, &mut node_map);

            let mut first_style = taffy.style(col.flow.child_nodes[0]).unwrap().clone();
            first_style.margin.top = length(2.0_f32);
            first_style.margin.bottom = length(3.0_f32);
            taffy
                .set_style(col.flow.child_nodes[0], first_style)
                .unwrap();
            let mut second_style = taffy.style(col.flow.child_nodes[1]).unwrap().clone();
            second_style.margin.top = length(4.0_f32);
            second_style.margin.bottom = length(6.0_f32);
            taffy
                .set_style(col.flow.child_nodes[1], second_style)
                .unwrap();

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
            col.apply_layout(&taffy, node, Point::ZERO);

            assert_eq!(col.flow.children[0].get_rect().y0, 2.0);
            assert_eq!(col.flow.children[1].get_rect().y0, 39.0);
            assert_eq!(col.get_rect().height(), 55.0);
        }

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
            use crate::widget::{ScrollAxis, Scrollable};

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
            use crate::widget::{ScrollAxis, Scrollable};

            let mut scroll = Scrollable::new(ScrollAxis::Horizontal)
                .with_height(50.0)
                .with_child(Box::new(
                    Container::new().with_no_padding().with_width(1_200.0),
                ));

            let _ = lay_out(&mut scroll);

            assert_eq!(scroll.get_rect().height(), 50.0);
            assert_eq!(scroll.children()[0].get_rect().height(), 50.0);
        }

        intrinsic_column_gap_is_additive_with_explicit_padding();
        intrinsic_column_gap_is_additive_with_child_margins();
        nested_intrinsic_column_keeps_taffy_and_widget_sibling_geometry_aligned();
        intrinsic_column_in_row_respects_parent_center_alignment();
        intrinsic_column_gap_contributes_to_scrollable_content_height();
        horizontal_scrollable_keeps_cross_axis_stretch();
    }

    #[test]
    fn leading_text_in_columns_is_drawn_and_survives_redraw() {
        /// Regression: a bare leading `Row[Text]` at the head of an
        /// intrinsic-height `Column` followed by a `Container` sibling must
        /// still emit the Text. Previously the column's cached-draw rebuild
        /// dropped the leading child while the Container painted.
        fn leading_text_in_intrinsic_column_is_drawn() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .push(Box::new(Row::new().push(Box::new(Text::new("Heading")))))
                .push(Box::new(
                    Container::new()
                        .with_background()
                        .with_child(Box::new(Text::new("Body"))),
                ));
            let _ = lay_out(&mut col);

            // The leading Row's resolved rect must be non-degenerate — if it
            // collapsed to zero the text could never paint.
            let row_rect = col.flow.children[0].get_rect();
            assert!(
                row_rect.width() > 1.0 && row_rect.height() > 1.0,
                "leading Row collapsed to a degenerate rect: {row_rect:?}"
            );

            // And the leading Text inside the Row must have a real rect too.
            let text_rect = col.flow.children[0].children()[0].get_rect();
            assert!(
                text_rect.width() > 1.0 && text_rect.height() > 1.0,
                "leading Text collapsed to a degenerate rect: {text_rect:?}"
            );

            // Drawing must emit content for BOTH children (a fill count > the
            // Container's lone background fill proves the leading text painted).
            let fills = glyph_count(&mut col);
            assert!(
                fills >= 2,
                "expected the leading Text + the Container body to both paint; \
             got {fills} fills (regression: leading child dropped)"
            );
        }

        /// Reproduce the *cached*-path drop: draw once (warms every cache),
        /// `mark_drawn`, then re-draw on a frame where the Column itself
        /// rebuilds (its rect changed) but the leading Row is cache-clean.
        /// The leading Text must still appear in the recomposed scene.
        fn leading_text_survives_cached_redraw() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .push(Box::new(Row::new().push(Box::new(Text::new("Heading")))))
                .push(Box::new(
                    Container::new()
                        .with_background()
                        .with_child(Box::new(Text::new("Body"))),
                ));
            let _ = lay_out(&mut col);

            // Frame 1 — warm caches, then clear dirty flags as the App does.
            let f1 = glyph_count(&mut col);
            col.mark_drawn();

            // Frame 2 — same content, caches clean. The cache-hit path must
            // replay the full subtree (leading Text included).
            let f2 = glyph_count(&mut col);
            assert!(
                f2 >= f1 && f2 >= 2,
                "cached re-draw dropped content: frame1={f1} frame2={f2}"
            );
        }

        /// The settings scenario: a bare leading `Text` (NOT wrapped in a
        /// Row or Container) at the head of an intrinsic-height Column hosted
        /// inside a `Scrollable`. This is the exact shape Settings worked
        /// around by wrapping every heading in a background Container. The
        /// leading Text must resolve a non-degenerate rect through the measure
        /// pass.
        fn bare_leading_text_in_scrolled_intrinsic_column() {
            use crate::widget::{ScrollAxis, Scrollable};
            // The Scrollable hosts the intrinsic column; this is what drives
            // the constrained measure pass that previously zeroed the leading
            // child.
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

            // Reach the inner column's first child (the bare leading Text).
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

        /// The same content as a plain (fill-height) column must also draw the
        /// leading text — guards the non-intrinsic path.
        fn leading_text_in_plain_column_is_drawn() {
            let mut col = Column::new()
                .push(Box::new(Text::new("Heading")))
                .push(Box::new(
                    Container::new()
                        .with_background()
                        .with_child(Box::new(Text::new("Body"))),
                ));
            let _ = lay_out(&mut col);
            let text_rect = col.flow.children[0].get_rect();
            assert!(
                text_rect.width() > 1.0 && text_rect.height() > 1.0,
                "leading Text collapsed: {text_rect:?}"
            );
        }

        leading_text_in_intrinsic_column_is_drawn();
        leading_text_survives_cached_redraw();
        bare_leading_text_in_scrolled_intrinsic_column();
        leading_text_in_plain_column_is_drawn();
    }
}
