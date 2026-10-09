//! The editor's window chrome: the controls above the panes.
//!
//! Every control is a library widget hosted by the painted [`TypstApp`]
//! (`crate::app::TypstApp`): a [`Toolbar`] for the commands, a semantic
//! file-name heading, and a [`ToggleGroup`] for Edit/Preview at narrow widths.
//! The desktop owns document tabs. The app places the controls with
//! [`Chrome::place`] and reads what the person chose from [`Chrome::take`].

use std::cell::RefCell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toggle_group::{Segment, SegmentedFace, ToggleGroup};
use ui::widget::toolbar::{Orientation, Toolbar};
use ui::widget::{Button, ControlSize, Text, Widget};

/// What a control asked the editor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
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
    document: Text,
    document_name: Rc<RefCell<String>>,
    view: ToggleGroup,
    close_problems: Button,
    /// The window's History control, at the bar's trailing edge: the bar is
    /// the window's header row, so the window draws no History bar above it.
    history: ui::widget::HistoryOpener,
    history_rect: Rect,
    structure_dirty: bool,
    toolbar_rect: Rect,
    visible: Visible,
}

/// Which of the optional controls show in the current layout.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Visible {
    view: bool,
    close: bool,
}

fn command(queue: &Queue, command: Command) -> impl FnMut() + 'static {
    let queue = Rc::clone(queue);
    move || queue.borrow_mut().push(command)
}

/// The Problems toggle's label: the count of problems, when there are any.
fn problems_label(problems: usize) -> String {
    if problems == 0 {
        "Problems".to_string()
    } else {
        format!("Problems ({problems})")
    }
}

trait OptionalToolbarAction {
    fn with_optional_action(self, binding: Option<ui::tilo_actions::ActionBinding<()>>) -> Self;
}

impl OptionalToolbarAction for Toolbar {
    fn with_optional_action(self, binding: Option<ui::tilo_actions::ActionBinding<()>>) -> Self {
        match binding {
            Some(binding) => self.with_action(binding),
            None => self,
        }
    }
}

fn build_toolbar(
    queue: &Queue,
    problems: usize,
    shown: bool,
    actions: Option<&ui::framework::Actions<crate::resource::Workspace>>,
) -> Toolbar {
    use crate::actions::TypstEffect;
    let label = problems_label(problems);
    // Overflow folds the trailing commands first, so the order is the
    // priority: what the person needs at every width (compile, and whether the
    // source has problems) leads, the preview's zoom follows, and the file
    // commands, which have shortcuts, fold into the More menu first.
    Toolbar::new(Orientation::Horizontal)
        .push_button("Compile", command(queue, Command::Compile))
        .with_optional_action(actions.and_then(|actions| actions.effect(&TypstEffect::Compile)))
        .push_toggle(&label, shown, command(queue, Command::ToggleProblems))
        .with_optional_action(actions.and_then(|actions| actions.effect(&TypstEffect::Problems)))
        .push_separator()
        .push_icon(Icon::Minus, "Zoom out", command(queue, Command::ZoomOut))
        .with_optional_action(actions.and_then(|actions| actions.effect(&TypstEffect::ZoomOut)))
        .push_icon(Icon::Plus, "Zoom in", command(queue, Command::ZoomIn))
        .with_optional_action(actions.and_then(|actions| actions.effect(&TypstEffect::ZoomIn)))
        // Zoom out without zoom in reads as a broken control: the pair folds
        // into the More menu together.
        .joined()
        .push_button("Fit width", command(queue, Command::Fit))
        .with_optional_action(actions.and_then(|actions| actions.effect(&TypstEffect::FitWidth)))
        .push_spacer()
        .push_button("Save", command(queue, Command::Save))
        .with_optional_action(
            actions.and_then(|actions| actions.effect(&TypstEffect::Save { path: None })),
        )
        .push_button("Open", command(queue, Command::Open))
        .with_optional_action(
            actions.and_then(|actions| actions.effect(&TypstEffect::OpenFile { path: None })),
        )
        .push_button("Export PDF", command(queue, Command::ExportPdf))
        .with_optional_action(
            actions.and_then(|actions| actions.effect(&TypstEffect::ExportPdf { path: None })),
        )
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

impl Chrome {
    pub fn new() -> Self {
        let queue: Queue = Rc::default();
        let document_name = Rc::new(RefCell::new(String::new()));
        let source = Rc::clone(&document_name);
        let document = Text::dynamic(move || source.borrow().clone())
            .with_id("typst.document")
            .with_role(TextRole::Heading)
            .with_heading_level(1)
            .with_kind(ui::TextKind::Identifier);
        let view_queue = Rc::clone(&queue);
        let view = ToggleGroup::new(vec![Segment::new("Edit"), Segment::new("Preview")], 0)
            .with_id("typst.view")
            .on_view("typst.view", move |index| {
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
            toolbar: build_toolbar(&queue, 0, false, None),
            toolbar_for,
            document,
            document_name,
            view,
            close_problems: Button::icon_only("typst.close-problems", Icon::X, "Close problems")
                .on_click(command(&queue, Command::ToggleProblems)),
            history: ui::widget::HistoryOpener::new(),
            history_rect: Rect::ZERO,
            structure_dirty: true,
            toolbar_rect: Rect::ZERO,
            visible: Visible::default(),
            queue,
        }
    }

    /// Bind the actual toolbar commands to the resource's declared operations.
    pub(crate) fn attach_actions(
        &mut self,
        actions: &ui::framework::Actions<crate::resource::Workspace>,
    ) {
        self.toolbar = build_toolbar(
            &self.queue,
            self.toolbar_for.problems,
            self.toolbar_for.shown,
            Some(actions),
        );
        self.close_problems = actions.bind_effect_invocation(
            Button::icon_only("typst.close-problems", Icon::X, "Close problems"),
            &crate::actions::TypstEffect::Problems,
        );
        self.structure_dirty = true;
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
        let name = names.get(current).copied().unwrap_or_default();
        *self.document_name.borrow_mut() = name.to_string();
        let wanted = ToolbarFor {
            problems,
            shown: problems_shown,
        };
        if wanted != self.toolbar_for {
            self.toolbar.set_active(PROBLEMS_ITEM, problems_shown);
            self.toolbar
                .set_label(PROBLEMS_ITEM, &problems_label(problems));
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

    /// The layout tree was rebuilt: a History control the header replaced
    /// may now be dropped.
    pub fn layout_rebuilt(&mut self) {
        self.history.tree_rebuilt();
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
        _documents: usize,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let spacing = &theme.tokens.spacing;
        let (pad, gap) = (f64::from(spacing.s3), f64::from(spacing.s3));
        let control = Self::control_height(tr, theme);
        let y0 = bar.y0 + (bar.height() - control) / 2.0;
        let row = |x0: f64, x1: f64| Rect::new(x0, y0, x1.max(x0), y0 + control);

        self.show(Visible {
            view: compact,
            close: self.visible.close,
        });
        let natural = self.natural_document_width(tr, theme);
        let mut x = bar.x0 + pad;

        let mut right = bar.x1 - pad;
        // The window's History control holds the trailing corner.
        self.history_rect = if self.history.is_shown() {
            let rect = row(right - control, right);
            right = (right - control - gap).max(x);
            rect
        } else {
            Rect::ZERO
        };
        if let Some(button) = self.history.button_mut() {
            button.apply_layout_external(self.history_rect);
        }

        let mut view_rect = Rect::ZERO;
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
            picker_rect = row(x, x + natural.min((right - x - control - gap).max(0.0)));
            x = picker_rect.x1 + gap;
            toolbar_rect = row(x, right);
        }

        self.document.apply_layout_external(picker_rect);
        self.view.apply_layout_external(view_rect);
        self.toolbar_rect = toolbar_rect;
        self.toolbar.lay_out_in(toolbar_rect, tr, theme);
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
    fn natural_document_width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let width = f64::from(
            tr.measure_style(
                &self.document_name.borrow(),
                None,
                &theme.text(TextRole::Heading),
            )
            .0,
        );
        width.min(f64::from(theme.tokens.metrics.panel_width_list))
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
        self.toolbar.open_overflow();
    }

    pub fn toolbar_has_overflow(&self) -> bool {
        self.toolbar.has_overflow()
    }

    /// Every control, in tree order: the layout tree, event routing and
    /// accessibility all follow it.
    pub fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(7);
        children.push(&self.document);
        if self.visible.view {
            children.push(&self.view);
        }
        children.push(&self.toolbar);
        if self.visible.close {
            children.push(&self.close_problems);
        }
        if let Some(history) = self.history.button() {
            children.push(history);
        }
        children
    }

    pub fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(7);
        children.push(&mut self.document);
        if self.visible.view {
            children.push(&mut self.view);
        }
        children.push(&mut self.toolbar);
        if self.visible.close {
            children.push(&mut self.close_problems);
        }
        if let Some(history) = self.history.button_mut() {
            children.push(history);
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

    /// Show the History control the host offers (or none) at the bar's
    /// trailing edge; the bar is laid out and built again around it.
    pub fn set_history(&mut self, control: Option<ui::widget::HistoryControl>) {
        self.history.set_control(control);
        self.structure_dirty |= self.history.take_rebuilt();
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

    /// Where the toolbar's More chip is, when something folded into it.
    pub(crate) fn toolbar_chip(&self) -> Option<Rect> {
        self.toolbar_nodes()
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some("toolbar.overflow"))
            .and_then(|node| node.bounds)
            .map(ui::a11y::AccessBounds::to_rect)
    }
}
