//! The semantic tree the editor exposes, and the actions assistive technology
//! and agents perform through it.

use super::block_layout::cell_table;
use super::toolbar::{STYLE_PALETTE, ToolAction};
use crate::compact_swatch_palette::PaletteKind;
use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::review_overlay::ReviewAction;
use tilo_doc::snapshot::{AttrVal, BlockView};
use ui::widget::Widget;
use vello::kurbo::Rect;

use super::panels::swatch_names;
use super::{A11yTreeNode, PanelHit, WordProcessorWidget};

impl A11yTreeNode {
    fn leaf(node: ui::a11y::AccessNode) -> Self {
        Self {
            node,
            children: Vec::new(),
        }
    }

    /// Convert this test-friendly carrier into the synthetic subtree the
    /// shared AccessKit/AT-SPI bridge actually exports. Without this explicit
    /// conversion, tests can inspect a rich private tree while the host sees
    /// only the document root.
    pub(super) fn into_access_node(self) -> ui::a11y::AccessNode {
        self.node.with_children(
            self.children
                .into_iter()
                .map(Self::into_access_node)
                .collect(),
        )
    }
}

/// Stable, locale-independent identity for an immediate-mode toolbar control.
pub(super) fn tool_action_id(action: ToolAction) -> String {
    match action {
        ToolAction::Bold => "bold".into(),
        ToolAction::Italic => "italic".into(),
        ToolAction::Underline => "underline".into(),
        ToolAction::Strike => "strike".into(),
        ToolAction::Super => "superscript".into(),
        ToolAction::Sub => "subscript".into(),
        ToolAction::Hyperlink => "hyperlink".into(),
        ToolAction::FontCycle => "font-family".into(),
        ToolAction::FontSizeDec => "font-size-decrease".into(),
        ToolAction::FontSizeInc => "font-size-increase".into(),
        ToolAction::InsertImage => "insert-image".into(),
        ToolAction::InsertTable => "insert-table".into(),
        ToolAction::TableRowAdd => "table-row-add".into(),
        ToolAction::TableRowDel => "table-row-delete".into(),
        ToolAction::TableColAdd => "table-column-add".into(),
        ToolAction::TableColDel => "table-column-delete".into(),
        ToolAction::TableDelete => "table-delete".into(),
        ToolAction::ToggleStyles => "toggle-styles".into(),
        ToolAction::ToggleRuler => "toggle-ruler".into(),
        ToolAction::ToggleOutline => "toggle-outline".into(),
        ToolAction::InsertToc => "insert-table-of-contents".into(),
        ToolAction::NewMenu => "new-from-template".into(),
        ToolAction::TextColorPalette => "text-color".into(),
        ToolAction::HighlightPalette => "highlight".into(),
        ToolAction::TextColor(None) => "text-color-default".into(),
        ToolAction::TextColor(Some(rgb)) => {
            format!("text-color-{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
        }
        ToolAction::Highlight(None) => "highlight-clear".into(),
        ToolAction::Highlight(Some(rgb)) => {
            format!("highlight-{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
        }
    }
}

/// A human-readable accessible name for a toolbar [`ToolAction`]. The visible
/// toolbar faces are terse ("B", "x²", an icon) for sighted
/// users; a screen reader needs the spelled-out verb instead.
pub(super) fn tool_action_a11y_label(a: ToolAction) -> String {
    let key = match a {
        ToolAction::Bold => "a11y-bold",
        ToolAction::Italic => "a11y-italic",
        ToolAction::Underline => "a11y-underline",
        ToolAction::Strike => "a11y-strikethrough",
        ToolAction::Super => "a11y-superscript",
        ToolAction::Sub => "a11y-subscript",
        ToolAction::Hyperlink => "a11y-hyperlink",
        ToolAction::FontCycle => "a11y-font",
        ToolAction::FontSizeDec => "a11y-size-down",
        ToolAction::FontSizeInc => "a11y-size-up",
        ToolAction::InsertImage => "a11y-insert-image",
        ToolAction::InsertTable => "a11y-insert-table",
        ToolAction::TableRowAdd => "a11y-table-row-add",
        ToolAction::TableRowDel => "a11y-table-row-del",
        ToolAction::TableColAdd => "a11y-table-col-add",
        ToolAction::TableColDel => "a11y-table-col-del",
        ToolAction::TableDelete => "a11y-table-delete",
        ToolAction::ToggleStyles => "a11y-styles-panel",
        ToolAction::ToggleRuler => "a11y-ruler",
        ToolAction::ToggleOutline => "a11y-document-outline",
        ToolAction::InsertToc => "a11y-insert-toc",
        ToolAction::NewMenu => "a11y-new-from-template",
        ToolAction::TextColorPalette => "picker-text-color",
        ToolAction::HighlightPalette => "picker-highlight",
        ToolAction::TextColor(c) => {
            return match c {
                Some(rgb) => {
                    let hex = format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                    t_args("a11y-text-color", &[arg("hex", hex)])
                }
                None => t("a11y-text-color-default"),
            };
        }
        ToolAction::Highlight(c) => {
            return match c {
                Some(rgb) => {
                    let hex = format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                    t_args("a11y-highlight", &[arg("hex", hex)])
                }
                None => t("a11y-highlight-clear"),
            };
        }
    };
    t(key)
}

pub(super) fn review_action_target(action: &ReviewAction) -> String {
    match action {
        ReviewAction::AcceptChange(id) => format!("word-processor.review:accept:{id}"),
        ReviewAction::RejectChange(id) => format!("word-processor.review:reject:{id}"),
        ReviewAction::AcceptAllChanges => "word-processor.review:accept-all".into(),
        ReviewAction::RejectAllChanges => "word-processor.review:reject-all".into(),
        ReviewAction::ResolveComment(id) => format!("word-processor.review:resolve:{id}"),
        ReviewAction::OpenCommentThread(id) => format!("word-processor.review:reply:{id}"),
    }
}

fn review_action_a11y_label(
    action: &ReviewAction,
    changes: &[tilo_doc::snapshot::ChangeView],
) -> String {
    use tilo_doc::snapshot::ChangeKind;
    match action {
        ReviewAction::AcceptChange(id) => match changes.iter().find(|change| change.id == *id) {
            Some(change) if change.kind == ChangeKind::Del => t("review-accept-deletion"),
            _ => t("review-accept-insertion"),
        },
        ReviewAction::RejectChange(id) => match changes.iter().find(|change| change.id == *id) {
            Some(change) if change.kind == ChangeKind::Del => t("review-reject-deletion"),
            _ => t("review-reject-insertion"),
        },
        ReviewAction::AcceptAllChanges => t("review-accept-all"),
        ReviewAction::RejectAllChanges => t("review-reject-all"),
        ReviewAction::ResolveComment(_) => t("review-resolve-comment"),
        ReviewAction::OpenCommentThread(_) => t("review-reply"),
    }
}

impl WordProcessorWidget {
    /// The 1-based heading level for a heading block (defaults to 1 when the
    /// `level` attr is absent or non-positive).
    fn block_heading_level(b: &BlockView) -> u8 {
        match b.attrs.get("level") {
            Some(AttrVal::Int(n)) if *n >= 1 => num::u8_of_i64((*n).min(255)),
            _ => 1,
        }
    }

    /// The accessible name for an image block: its explicit `alt` attr (set by
    /// the ODT/DOCX importers, round-tripped through `svg:desc`), falling back to
    /// the block's own text (a caption), or `None` when neither is present (a
    /// decorative / unlabelled image).
    fn image_alt(b: &BlockView) -> Option<String> {
        if let Some(AttrVal::Str(alt)) = b.attrs.get("alt") {
            let alt = alt.trim();
            if !alt.is_empty() {
                return Some(alt.to_string());
            }
        }
        let text = b.text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }

    /// The accessibility node for a single document block, or `None` for a block
    /// kind that carries no standalone semantics here (table cells are emitted as
    /// children of their table node, not at the top level).
    pub(super) fn block_access_node(b: &BlockView) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let node = match b.kind.as_str() {
            "heading" => AccessNode::new(Role::Heading {
                level: Self::block_heading_level(b),
            })
            .with_name(b.text.clone()),
            "paragraph" => AccessNode::new(Role::Paragraph).with_value(b.text.clone()),
            "list_item" => AccessNode::new(Role::ListItem).with_value(b.text.clone()),
            "image" => AccessNode::new(Role::Image).with_optional_name(Self::image_alt(b)),
            // Cells are surfaced under their table; standalone they have no role.
            "cell" => return None,
            // Unknown / future block kinds degrade to non-editable static text so
            // their content is still announced rather than dropped.
            _ => AccessNode::new(Role::StaticText).with_value(b.text.clone()),
        };
        Some(node)
    }

    /// Build the toolbar group node + a button node per visible toolbar control.
    /// Returns `None` when the toolbar has not been laid out yet (no frame drawn),
    /// so the document tree never carries an empty toolbar group.
    fn toolbar_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.toolbar.is_empty() {
            return None;
        }
        let mut buttons: Vec<A11yTreeNode> = self
            .toolbar
            .iter()
            .map(|(action, rect)| {
                let role = if matches!(
                    action,
                    ToolAction::Bold
                        | ToolAction::Italic
                        | ToolAction::Underline
                        | ToolAction::Strike
                        | ToolAction::Super
                        | ToolAction::Sub
                        | ToolAction::ToggleStyles
                        | ToolAction::ToggleRuler
                        | ToolAction::ToggleOutline
                ) {
                    Role::ToggleButton
                } else {
                    Role::Button
                };
                let node = AccessNode::new(role)
                    .with_name(tool_action_a11y_label(*action))
                    .with_bounds(*rect)
                    .with_action_target(format!(
                        "word-processor.toolbar:{}",
                        tool_action_id(*action)
                    ))
                    .with_state(AccessState {
                        focusable: true,
                        checked: (role == Role::ToggleButton).then(|| match action {
                            ToolAction::Bold => self.mark_active_in_selection("strong"),
                            ToolAction::Italic => self.mark_active_in_selection("em"),
                            ToolAction::Underline => self.mark_active_in_selection("underline"),
                            ToolAction::Strike => self.mark_active_in_selection("strike"),
                            ToolAction::Super => self.mark_active_in_selection("super"),
                            ToolAction::Sub => self.mark_active_in_selection("sub"),
                            ToolAction::ToggleStyles => self.view.styles_panel_visible,
                            ToolAction::ToggleRuler => self.view.ruler_visible,
                            ToolAction::ToggleOutline => self.outline.is_open(),
                            _ => false,
                        }),
                        ..Default::default()
                    });
                A11yTreeNode::leaf(node)
            })
            .collect();
        // The zoom field sits in the status band but belongs to the view
        // controls: expose it with them.
        if let Some(node) = Widget::accessibility(&self.zoom_input) {
            buttons.push(A11yTreeNode::leaf(node.with_bounds(self.zoom_rect)));
        }
        Some(A11yTreeNode {
            node: AccessNode::new(Role::Toolbar).with_name(t("a11y-group-formatting")),
            children: buttons,
        })
    }

    /// Expose the currently painted margin-card controls as real semantic
    /// buttons. Their bounds and targets are sourced from the same hit list as
    /// pointer input, so assistive technology never gets a parallel, stale UI.
    fn review_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if !self.review.is_visible() || self.review.hits().is_empty() {
            return None;
        }
        let buttons = self
            .review
            .hits()
            .iter()
            .map(|(rect, action)| {
                A11yTreeNode::leaf(
                    AccessNode::new(Role::Button)
                        .with_name(review_action_a11y_label(action, self.review.changes()))
                        .with_bounds(*rect)
                        .with_action_target(review_action_target(action))
                        .with_state(AccessState::new().focusable(true)),
                )
            })
            .collect();
        Some(A11yTreeNode {
            node: AccessNode::new(Role::Group).with_name(t("a11y-group-review")),
            children: buttons,
        })
    }

    /// Expose the selected image's placement choices from the exact hit list
    /// painted by the inspector. The full names/descriptions avoid making the
    /// compact visual labels the only cue for screen-reader and tooltip users.
    fn image_placement_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let buttons = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                let (id, name, description) = match hit {
                    PanelHit::ImageInline => (
                        "inline",
                        t("a11y-image-inline"),
                        t("a11y-image-inline-description"),
                    ),
                    PanelHit::ImageFreePosition => (
                        "free",
                        t("a11y-image-free-position"),
                        t("a11y-image-free-position-description"),
                    ),
                    PanelHit::ImageWrapLeft => (
                        "left",
                        t("a11y-image-wrap-left"),
                        t("a11y-image-wrap-left-description"),
                    ),
                    PanelHit::ImageWrapRight => (
                        "right",
                        t("a11y-image-wrap-right"),
                        t("a11y-image-wrap-right-description"),
                    ),
                    _ => return None,
                };
                Some(A11yTreeNode::leaf(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_description(description)
                        .with_bounds(*rect)
                        .with_action_target(format!("word-processor.image-placement:{id}"))
                        .with_state(AccessState::new().focusable(true)),
                ))
            })
            .collect::<Vec<_>>();
        (!buttons.is_empty()).then(|| A11yTreeNode {
            node: AccessNode::new(Role::Group).with_name(t("a11y-group-image-placement")),
            children: buttons,
        })
    }

    fn panel_hit_identity(&self, hit: &PanelHit) -> (String, String) {
        let (id, label) = match hit {
            PanelHit::Close => ("close".into(), t("panel-close")),
            PanelHit::Pick(id) => (format!("pick:{id}"), self.sheet.get(id).name),
            PanelHit::ParaAlign(value) => {
                (format!("align:{value}"), t(&format!("panel-align-{value}")))
            }
            PanelHit::SetFont(value) => {
                (format!("font:{value}"), t(&format!("panel-font-{value}")))
            }
            PanelHit::ToggleBold => ("bold".into(), t("panel-bold")),
            PanelHit::ToggleItalic => ("italic".into(), t("panel-italic")),
            PanelHit::SetColor(color) => {
                let label = STYLE_PALETTE
                    .iter()
                    .zip(swatch_names(PaletteKind::Text))
                    .find_map(|(candidate, label)| (candidate == color).then_some(label))
                    .unwrap_or_else(|| tool_action_a11y_label(ToolAction::TextColor(Some(*color))));
                let [red, green, blue] = color;
                (format!("color:{red:02x}{green:02x}{blue:02x}"), label)
            }
            PanelHit::OpenColorPicker => ("custom-color".into(), t("panel-colour-custom")),
            PanelHit::NewStyle => ("new-style".into(), t("panel-new-style")),
            PanelHit::ImageScale(factor) => {
                if *factor < 1.0 {
                    ("image-smaller".into(), t("panel-image-smaller"))
                } else {
                    ("image-larger".into(), t("panel-image-larger"))
                }
            }
            PanelHit::ImageFlipH => ("image-flip-h".into(), t("panel-image-flip-h")),
            PanelHit::ImageFlipV => ("image-flip-v".into(), t("panel-image-flip-v")),
            PanelHit::ImageCropInset => ("image-crop-inset".into(), t("panel-image-crop-inset")),
            PanelHit::ImageCropReset => ("image-crop-reset".into(), t("panel-image-crop-reset")),
            PanelHit::ImageInline => ("image-inline".into(), t("a11y-image-inline")),
            PanelHit::ImageFreePosition => ("image-free".into(), t("a11y-image-free-position")),
            PanelHit::ImageWrapLeft => ("image-left".into(), t("a11y-image-wrap-left")),
            PanelHit::ImageWrapRight => ("image-right".into(), t("a11y-image-wrap-right")),
        };
        (format!("word-processor.styles:{id}"), label)
    }

    fn panel_hit_checked(&self, hit: &PanelHit) -> Option<bool> {
        let style = self.focused_style_key().map(|key| self.sheet.get(&key));
        match hit {
            PanelHit::ParaAlign(value) => Some(self.current_align() == *value),
            PanelHit::SetFont(value) => style.map(|style| style.font == *value),
            PanelHit::ToggleBold => style.map(|style| style.bold),
            PanelHit::ToggleItalic => style.map(|style| style.italic),
            PanelHit::SetColor(value) => style.map(|style| style.color == *value),
            _ => None,
        }
    }

    pub(super) fn visible_styles_rect(&self, rect: Rect) -> Option<Rect> {
        let visible = rect.intersect(self.panel_body);
        (visible.width() > 0.0 && visible.height() > 0.0).then_some(visible)
    }

    /// Project the same painted hits and shared fields the foreground inspector
    /// actually owns. Scrolled-out controls have no actionable UI node.
    fn styles_page_accessibility_tree(&self) -> A11yTreeNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                let close = matches!(hit, PanelHit::Close);
                let bounds = if close {
                    Some(*rect)
                } else {
                    self.visible_styles_rect(*rect)
                }?;
                let (target, label) = self.panel_hit_identity(hit);
                let checked = self.panel_hit_checked(hit);
                let role = if matches!(hit, PanelHit::ToggleBold | PanelHit::ToggleItalic) {
                    Role::ToggleButton
                } else if checked.is_some() {
                    Role::RadioButton
                } else {
                    Role::Button
                };
                Some(A11yTreeNode::leaf(
                    AccessNode::new(role)
                        .with_name(label)
                        .with_bounds(bounds)
                        .with_action_target(target)
                        .with_state(AccessState {
                            focusable: true,
                            disabled: !close && self.read_only(),
                            checked,
                            ..Default::default()
                        }),
                ))
            })
            .collect::<Vec<_>>();
        for slot in &self.num_inputs {
            if let Some(bounds) = self.visible_styles_rect(slot.rect)
                && let Some(mut node) = Widget::accessibility(&slot.input)
            {
                node.state.disabled |= self.read_only();
                children.push(A11yTreeNode::leaf(
                    node.with_bounds(bounds)
                        .with_action_target(slot.field.semantic_target()),
                ));
            }
        }
        for (field, rect, label, target) in [
            (
                &self.line_spacing,
                self.line_spacing_rect,
                "panel-line-spacing",
                "word-processor.styles:line-spacing",
            ),
            (
                &self.style_pick,
                self.style_pick_rect,
                "panel-style",
                "word-processor.styles:style",
            ),
        ] {
            if let Some(bounds) = self.visible_styles_rect(rect)
                && let Some(mut node) = Widget::accessibility(field)
            {
                node.state.disabled |= self.read_only();
                children.push(A11yTreeNode::leaf(
                    node.with_name(t(label))
                        .with_bounds(bounds)
                        .with_action_target(target),
                ));
            }
        }
        let title = if self.selected_image_id().is_some() {
            t("panel-image")
        } else {
            t("toolbar-styles")
        };
        A11yTreeNode {
            node: AccessNode::new(Role::Group)
                .with_name(title)
                .with_bounds(self.panel_rect),
            children,
        }
    }

    pub(super) fn perform_styles_page_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
    ) -> Option<bool> {
        if !self.styles_panel_full_window() {
            return None;
        }
        let target = key_path.last().copied();
        let hit = self.panel_hits.iter().find_map(|(_, hit)| {
            (target == Some(ui::a11y::hash_key(&self.panel_hit_identity(hit).0)))
                .then(|| hit.clone())
        });
        if let Some(hit) = hit {
            if self.read_only() && !matches!(hit, PanelHit::Close) {
                return Some(false);
            }
            return Some(match action {
                ui::a11y::Action::Focus => {
                    self.set_focus(true);
                    true
                }
                ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck => {
                    self.apply_panel_hit(state, hit);
                    true
                }
                _ => false,
            });
        }
        Some(self.perform_styles_field_action(target, action, state))
    }

    fn perform_styles_field_action(
        &mut self,
        target: Option<u64>,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.read_only() {
            return false;
        }
        if matches!(action, ui::a11y::Action::Focus) {
            self.set_focus(true);
        }
        let field = self.num_inputs.iter().find_map(|slot| {
            (target == Some(ui::a11y::hash_key(&slot.field.semantic_target())))
                .then_some(slot.field)
        });
        let performed = if let Some(field) = field {
            if matches!(action, ui::a11y::Action::Focus) {
                self.line_spacing.close();
                self.style_pick.close();
                Widget::set_focus(&mut self.line_spacing, false);
                Widget::set_focus(&mut self.style_pick, false);
                self.focus_hosted(Some(field), false);
                true
            } else {
                let Some(slot) = self.num_inputs.iter_mut().find(|slot| slot.field == field) else {
                    return false;
                };
                Widget::perform_action(&mut slot.input, action, &[], state, &automerge::ROOT)
            }
        } else {
            if matches!(action, ui::a11y::Action::Focus) {
                self.focus_hosted(None, false);
                Widget::set_focus(&mut self.line_spacing, false);
                Widget::set_focus(&mut self.style_pick, false);
            }
            let menu = if target == Some(ui::a11y::hash_key(&"word-processor.styles:line-spacing"))
            {
                &mut self.line_spacing
            } else if target == Some(ui::a11y::hash_key(&"word-processor.styles:style")) {
                &mut self.style_pick
            } else {
                return false;
            };
            if matches!(action, ui::a11y::Action::Focus) {
                Widget::set_focus(menu, true);
                true
            } else {
                Widget::perform_action(menu, action, &[], state, &automerge::ROOT)
            }
        };
        if performed {
            self.drain_hosted_fields(state);
            self.lifecycle.dirty = true;
        }
        performed
    }

    /// Assemble the full accessibility node tree: a `Role::Document` root holding
    /// one node per block (headings with level, paragraphs/list items with text,
    /// images with alt text, tables as `Role::Table`/`Role::Cell`), followed by
    /// a `Role::Toolbar` group of formatting buttons. [`Widget::accessibility`]
    /// returns this tree's root so the host's per-widget path surfaces the document.
    pub fn accessibility_tree(&self) -> A11yTreeNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        if self.styles_panel_full_window() {
            return self.styles_page_accessibility_tree();
        }
        let mut children: Vec<A11yTreeNode> = Vec::new();
        let mut i = 0usize;
        while let Some(b) = self.blocks.get(i) {
            if b.kind == "cell" {
                // Coalesce a run of consecutive cells sharing a `table` id into a
                // single Table node with the cells as Cell children.
                let table_id = cell_table(b);
                let mut cells: Vec<A11yTreeNode> = Vec::new();
                while let Some(c) = self.blocks.get(i) {
                    if c.kind != "cell" || cell_table(c) != table_id {
                        break;
                    }
                    cells.push(A11yTreeNode::leaf(
                        AccessNode::new(Role::Cell).with_value(c.text.clone()),
                    ));
                    i += 1;
                }
                children.push(A11yTreeNode {
                    node: AccessNode::new(Role::Table),
                    children: cells,
                });
                continue;
            }
            if let Some(node) = Self::block_access_node(b) {
                children.push(A11yTreeNode::leaf(node));
            }
            i += 1;
        }

        if let Some(toolbar) = self.toolbar_access_node() {
            children.push(toolbar);
        }
        if let Some(review) = self.review_access_node() {
            children.push(review);
        }
        if let Some(image_placement) = self.image_placement_access_node() {
            children.push(image_placement);
        }
        if let Some(palette) = self.compact_swatch_palette.accessibility() {
            children.push(A11yTreeNode::leaf(palette));
        }
        if let Some(find) = self.find.accessibility() {
            children.push(A11yTreeNode::leaf(find.with_bounds(self.find_band)));
        }

        A11yTreeNode {
            node: AccessNode::new(Role::Document).with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.lifecycle.focused)
                    .multiline(true),
            ),
            children,
        }
    }
}
