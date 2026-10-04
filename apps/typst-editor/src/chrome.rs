//! The editor's window chrome: the controls above the panes.
//!
//! Every control is a library widget hosted by the painted [`TypstApp`]
//! (`crate::app::TypstApp`): a [`Toolbar`] for the commands (it folds what
//! does not fit into its own More menu), a [`Dropdown`] listing the open
//! documents with previous and next buttons beside it, and in a narrow window
//! a [`ToggleGroup`] that picks which pane shows. The app places them with
//! [`Chrome::place`] and reads what the person chose from [`Chrome::take`].

use std::cell::RefCell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::LocalState;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toggle_group::{Segment, SegmentedFace, ToggleGroup};
use ui::widget::toolbar::{Orientation, Toolbar};
use ui::widget::{Button, ControlSize, Dropdown, Widget};

/// What a control asked the editor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PrevDoc,
    NextDoc,
    /// Open the document at this index of the list.
    SelectDoc(usize),
    Compile,
    Save,
    Open,
    ExportPdf,
    ZoomOut,
    ZoomIn,
    Fit,
    ToggleProblems,
    /// Narrow window: show the source editor pane.
    ShowEditor,
    /// Narrow window: show the rendered preview pane.
    ShowPreview,
}

type Queue = Rc<RefCell<Vec<Command>>>;

/// The position of the Problems toggle among the toolbar's items.
const PROBLEMS_ITEM: usize = 1;
/// The position of the zoom-out command; zoom in follows it. The two are one
/// pair: [`Chrome::place`] never lets overflow leave only the first showing.
const ZOOM_OUT_ITEM: usize = 3;

/// What the toolbar was built for; it is rebuilt when the Problems label or
/// the toggle's state would differ.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ToolbarFor {
    problems: usize,
    shown: bool,
}

pub struct Chrome {
    queue: Queue,
    toolbar: Toolbar,
    toolbar_for: ToolbarFor,
    /// Toolbars replaced since the last layout pass. The layout tree holds
    /// raw pointers to widgets, so a replaced one is kept alive until the
    /// next pass rebuilds the tree without it.
    retired: Vec<Toolbar>,
    previous: Button,
    next: Button,
    picker: Dropdown,
    /// The documents the picker lists, and the one chosen.
    picker_for: (Vec<String>, usize),
    view: ToggleGroup,
    close_problems: Button,
    /// The window's History button, placed in this bar so the window draws
    /// no History bar of its own.
    structure_dirty: bool,
    toolbar_rect: Rect,
    visible: Visible,
}

/// Which of the optional controls show in the current layout.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Visible {
    arrows: bool,
    view: bool,
    close: bool,
}

fn command(queue: &Queue, command: Command) -> impl FnMut() + 'static {
    let queue = Rc::clone(queue);
    move || queue.borrow_mut().push(command)
}

fn build_toolbar(queue: &Queue, problems: usize, shown: bool) -> Toolbar {
    let label = if problems == 0 {
        "Problems".to_string()
    } else {
        format!("Problems ({problems})")
    };
    // Overflow folds the trailing commands first, so the order is the
    // priority: what the person needs at every width (compile, and whether the
    // source has problems) leads, the preview's zoom follows, and the file
    // commands, which have shortcuts, fold into the More menu first.
    Toolbar::new(Orientation::Horizontal)
        .push_button("Compile", command(queue, Command::Compile))
        .push_toggle(&label, shown, command(queue, Command::ToggleProblems))
        .push_separator()
        .push_icon(Icon::Minus, "Zoom out", command(queue, Command::ZoomOut))
        .push_icon(Icon::Plus, "Zoom in", command(queue, Command::ZoomIn))
        .push_button("Fit width", command(queue, Command::Fit))
        .push_spacer()
        .push_button("Save", command(queue, Command::Save))
        .push_button("Open", command(queue, Command::Open))
        .push_button("Export PDF", command(queue, Command::ExportPdf))
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

impl Chrome {
    pub fn new() -> Self {
        let queue: Queue = Rc::default();
        let picker_state = LocalState::new(String::new());
        let picker_queue = Rc::clone(&queue);
        let picker = Dropdown::from_state(Vec::new(), &picker_state)
            .with_id("typst.document")
            .with_accessible_name("Document")
            .on_change(move |value| {
                if let Ok(index) = value.parse::<usize>() {
                    picker_queue.borrow_mut().push(Command::SelectDoc(index));
                }
            });
        let view_queue = Rc::clone(&queue);
        let view = ToggleGroup::new(vec![Segment::new("Edit"), Segment::new("Preview")], 0)
            .with_id("typst.view")
            .on_change(move |index| {
                let command = if index == 0 {
                    Command::ShowEditor
                } else {
                    Command::ShowPreview
                };
                view_queue.borrow_mut().push(command);
            });
        let toolbar_for = ToolbarFor {
            problems: 0,
            shown: false,
        };
        Self {
            toolbar: build_toolbar(&queue, 0, false),
            toolbar_for,
            retired: Vec::new(),
            previous: Button::icon_only(
                "typst.previous-document",
                Icon::ChevronLeft,
                "Previous document",
            )
            .on_click(command(&queue, Command::PrevDoc)),
            next: Button::icon_only("typst.next-document", Icon::ChevronRight, "Next document")
                .on_click(command(&queue, Command::NextDoc)),
            picker,
            picker_for: (Vec::new(), 0),
            view,
            close_problems: Button::icon_only("typst.close-problems", Icon::X, "Close problems")
                .on_click(command(&queue, Command::ToggleProblems)),
            structure_dirty: false,
            toolbar_rect: Rect::ZERO,
            visible: Visible::default(),
            queue,
        }
    }

    /// Everything the controls were asked to do since the last call.
    pub fn take(&mut self) -> Vec<Command> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    /// Make the controls say what the editor holds: the documents, the
    /// problem count and whether the panel is open, and the pane a narrow
    /// window shows.
    pub fn sync(
        &mut self,
        names: &[&str],
        current: usize,
        problems: usize,
        problems_shown: bool,
        preview_shown: bool,
    ) {
        if !self
            .picker_for
            .0
            .iter()
            .map(String::as_str)
            .eq(names.iter().copied())
        {
            self.picker.set_value_options(
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| (index.to_string(), (*name).to_string()))
                    .collect(),
            );
            self.picker_for.0 = names.iter().map(|name| (*name).to_string()).collect();
        }
        if self.picker_for.1 != current || self.picker.selected().parse() != Ok(current) {
            self.picker.set_selected(current.to_string());
            self.picker_for.1 = current;
        }
        let wanted = ToolbarFor {
            problems,
            shown: problems_shown,
        };
        if wanted != self.toolbar_for {
            if wanted.problems == self.toolbar_for.problems {
                self.toolbar.set_active(PROBLEMS_ITEM, problems_shown);
            } else {
                let rebuilt = build_toolbar(&self.queue, problems, problems_shown);
                self.retired
                    .push(std::mem::replace(&mut self.toolbar, rebuilt));
                self.structure_dirty = true;
            }
            self.toolbar_for = wanted;
        }
        self.view.set_selected(usize::from(preview_shown));
    }

    /// Whether the controls were rebuilt since the layout tree was.
    pub fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    /// The layout tree was rebuilt: replaced controls may now be dropped.
    pub fn layout_rebuilt(&mut self) {
        self.retired.clear();
    }

    /// The height of one control in the bar: the toolbar's own measure, so
    /// the band grows with the text size.
    pub fn control_height(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let metrics = &theme.tokens.metrics;
        let line = tr.measure_style("Ag", None, &theme.text(TextRole::Label)).1;
        f64::from(metrics.min_pointer_target)
            .max(f64::from(line) + 2.0 * f64::from(theme.tokens.spacing.s2))
    }

    /// Place every control in `bar`, the band reserved for them.
    pub fn place(
        &mut self,
        bar: Rect,
        compact: bool,
        documents: usize,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let spacing = &theme.tokens.spacing;
        let (pad, gap, tight) = (
            f64::from(spacing.s3),
            f64::from(spacing.s3),
            f64::from(spacing.s1),
        );
        let control = Self::control_height(tr, theme);
        let y0 = bar.y0 + (bar.height() - control) / 2.0;
        let row = |x0: f64, x1: f64| Rect::new(x0, y0, x1.max(x0), y0 + control);

        self.show(Visible {
            arrows: !compact && documents > 1,
            view: compact,
            close: self.visible.close,
        });
        let natural = self.natural_picker_width(tr, theme);
        let mut x = bar.x0 + pad;

        let right = bar.x1 - pad;

        let hidden = Rect::ZERO;
        let (mut prev_rect, mut next_rect, mut view_rect) = (hidden, hidden, hidden);
        let picker_rect;
        let toolbar_rect;
        if compact {
            let view_w = SegmentedFace::intrinsic_width(
                &["Edit", "Preview"],
                ControlSize::Regular,
                tr,
                theme,
            );
            // The overflow chip is the least the toolbar needs; the picker
            // gives way to the switch and the chip, never the other way round.
            let toolbar_min = control;
            let picker_w = natural.min((right - x - view_w - toolbar_min - 2.0 * gap).max(0.0));
            picker_rect = row(x, x + picker_w);
            x += picker_w + gap;
            view_rect = row(x, x + view_w);
            x += view_w + gap;
            toolbar_rect = row(x, right);
        } else {
            if self.visible.arrows {
                prev_rect = row(x, x + control);
                x += control + tight;
            }
            picker_rect = row(x, x + natural);
            x += natural;
            if self.visible.arrows {
                x += tight;
                next_rect = row(x, x + control);
                x += control;
            }
            x += gap;
            toolbar_rect = row(x, right);
        }

        self.previous.apply_layout_external(prev_rect);
        self.next.apply_layout_external(next_rect);
        self.picker.apply_layout_external(picker_rect);
        self.view.apply_layout_external(view_rect);
        self.toolbar_rect = toolbar_rect;
        self.toolbar.lay_out_in(toolbar_rect, tr, theme);
        // Zoom out without zoom in reads as a broken control: when overflow
        // would leave only the first of the pair showing, take back exactly
        // the room that command used so both fold into the More menu.
        if self.toolbar.visible_count() == ZOOM_OUT_ITEM + 1 {
            let fold = Rect::new(
                (toolbar_rect.x0 + control + gap).min(toolbar_rect.x1),
                toolbar_rect.y0,
                toolbar_rect.x1,
                toolbar_rect.y1,
            );
            self.toolbar.lay_out_in(fold, tr, theme);
        }
    }

    /// Place the Problems panel's close button; `None` hides it.
    pub fn place_close(&mut self, rect: Option<Rect>) {
        self.show(Visible {
            close: rect.is_some(),
            ..self.visible
        });
        self.close_problems
            .apply_layout_external(rect.unwrap_or(Rect::ZERO));
    }

    /// Say which optional controls the layout shows. A control that is not
    /// shown is not in the tree, so it is never a focus stop or an
    /// accessibility node; a change asks for a new layout pass.
    fn show(&mut self, visible: Visible) {
        if visible != self.visible {
            self.visible = visible;
            self.structure_dirty = true;
        }
    }

    /// The picker's natural width: its longest document name, its chevron
    /// and its insets, never wider than a list column.
    fn natural_picker_width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let style = theme.text(TextRole::Label);
        let widest = self
            .picker_for
            .0
            .iter()
            .map(|name| f64::from(tr.measure_style(name, None, &style).0))
            .fold(0.0, f64::max);
        let spacing = &theme.tokens.spacing;
        let chrome = 2.0 * f64::from(spacing.s3)
            + f64::from(theme.tokens.metrics.icon_md)
            + f64::from(theme.tokens.metrics.icon_label_gap);
        let floor = f64::from(theme.tokens.metrics.min_pointer_target) * 3.0;
        let cap = f64::from(theme.tokens.metrics.panel_width_list);
        (widest + chrome).clamp(floor, cap)
    }

    /// Whether a control holds an open list (the picker, the toolbar's More
    /// menu), which then owns the keyboard until it closes.
    pub fn captures_input(&self) -> bool {
        self.children().iter().any(|child| {
            child.wants_pointer_capture() || child.modal_capture().is_some() || child.traps_focus()
        })
    }

    pub fn toolbar_rect(&self) -> Rect {
        self.toolbar_rect
    }

    /// Open the picker's list: a screenshot scenario showing it.
    pub fn open_picker(&mut self) {
        self.picker.open_menu();
    }

    pub fn toolbar_has_overflow(&self) -> bool {
        self.toolbar.has_overflow()
    }

    /// Every control, in tree order: the layout tree, event routing and
    /// accessibility all follow it.
    pub fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(7);
        if self.visible.arrows {
            children.push(&self.previous);
        }
        children.push(&self.picker);
        if self.visible.arrows {
            children.push(&self.next);
        }
        if self.visible.view {
            children.push(&self.view);
        }
        children.push(&self.toolbar);
        if self.visible.close {
            children.push(&self.close_problems);
        }
        children
    }

    pub fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(7);
        if self.visible.arrows {
            children.push(&mut self.previous);
        }
        children.push(&mut self.picker);
        if self.visible.arrows {
            children.push(&mut self.next);
        }
        if self.visible.view {
            children.push(&mut self.view);
        }
        children.push(&mut self.toolbar);
        if self.visible.close {
            children.push(&mut self.close_problems);
        }
        children
    }

    /// Paint the controls that show in the current layout.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw(scene, transform, tr, theme, presence);
        }
    }

    pub fn needs_redraw(&self) -> bool {
        self.children().iter().any(|child| child.needs_redraw())
    }

    pub fn mark_drawn(&mut self) {
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }
}

#[cfg(test)]
impl Chrome {
    /// The toolbar's accessibility nodes, flattened.
    fn toolbar_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        fn flatten(node: ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
            for child in node.children.clone() {
                flatten(child, out);
            }
            out.push(node);
        }
        let mut out = Vec::new();
        if let Some(root) = self.toolbar.accessibility() {
            flatten(root, &mut out);
        }
        out
    }

    /// Every name the toolbar offers: its shown commands, the More chip, and
    /// the rows of the More menu while it is open.
    pub(crate) fn toolbar_names(&self) -> Vec<String> {
        self.toolbar_nodes()
            .into_iter()
            .filter_map(|node| node.name)
            .collect()
    }

    /// The name and action target of each command the toolbar shows (not the
    /// More chip), in order.
    pub(crate) fn toolbar_targets(&self) -> Vec<(String, String)> {
        let Some(root) = self.toolbar.accessibility() else {
            return Vec::new();
        };
        root.children
            .into_iter()
            .filter(|node| node.role != ui::a11y::Role::Menu)
            .filter_map(|node| Some((node.name?, node.action_target?)))
            .filter(|(_, target)| target != "toolbar.overflow")
            .collect()
    }

    /// Where the toolbar's More chip is, when something folded into it.
    pub(crate) fn toolbar_chip(&self) -> Option<Rect> {
        self.toolbar_nodes()
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some("toolbar.overflow"))
            .and_then(|node| node.bounds)
            .map(ui::a11y::AccessBounds::to_rect)
    }

    /// Activate a toolbar item the way assistive technology does.
    pub(crate) fn activate_toolbar(
        &mut self,
        target: &str,
        doc: &mut automerge::AutoCommit,
    ) -> bool {
        self.toolbar.perform_semantic_action(
            target,
            &ui::a11y::Action::Activate,
            doc,
            &automerge::ROOT,
        )
    }
}
