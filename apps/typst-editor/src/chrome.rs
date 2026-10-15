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
use ui::widget::{Button, ButtonRole, Chip, ChipLead, ChipStyle, ControlSize, Text, Tone, Widget};

/// What a control asked the editor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    ToggleOutline,
    Bold,
    Italic,
    Math,
    Cite,
    Section,
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

/// Compiler status shown only in the bottom status controls.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Diagnostics {
    errors: usize,
    warnings: usize,
    elapsed_ms: u128,
    compiling: bool,
}

pub struct Chrome {
    queue: Queue,
    toolbar: Toolbar,
    diagnostics: Diagnostics,
    bottom_status: Toolbar,
    sidebar: Button,
    compact: bool,
    footer_density: FooterDensity,
    actions: Option<ui::framework::Actions<crate::resource::Workspace>>,
    retired_toolbars: Vec<Toolbar>,
    document: Text,
    document_name: Rc<RefCell<String>>,
    view: ToggleGroup,
    close_problems: Button,
    /// The window's History control, at the bar's trailing edge: the bar is
    /// the window's header row, so the window draws no History bar above it.
    history: ui::widget::HistoryOpener,
    history_rect: Rect,
    structure_dirty: bool,
    reopen_overflow: bool,
    visible: Visible,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FooterDensity {
    Full,
    Compact,
}

/// Which of the optional controls show in the current layout.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Visible {
    view: bool,
    close: bool,
    bottom_status: bool,
}

fn command(queue: &Queue, command: Command) -> impl FnMut() + 'static {
    let queue = Rc::clone(queue);
    move || queue.borrow_mut().push(command)
}

fn sidebar_label(shown: bool) -> &'static str {
    if shown {
        "Hide sidebar"
    } else {
        "Show sidebar"
    }
}

fn diagnostic_label(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn status_chip(
    queue: &Queue,
    id: &str,
    label: &str,
    tone: Tone,
    icon: Icon,
    actions: Option<&ui::framework::Actions<crate::resource::Workspace>>,
) -> Chip {
    let chip = Chip::new(label)
        .with_id(id)
        .with_lead(ChipLead::Glyph(icon))
        .style(ChipStyle::Tinted(tone))
        .tooltip("Toggle problems")
        .on_click(command(queue, Command::ToggleProblems));
    match actions.and_then(|actions| actions.effect(&crate::actions::TypstEffect::Problems)) {
        Some(binding) => chip.on_action(binding),
        None => chip,
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
    status: Diagnostics,
    export: bool,
    compact: bool,
    actions: Option<&ui::framework::Actions<crate::resource::Workspace>>,
) -> Toolbar {
    let mut toolbar = Toolbar::new(Orientation::Horizontal);
    if export {
        // Only the primary output belongs in the header. Diagnostics and
        // compile timing have one home, in the persistent bottom status bar.
        return toolbar
            .push_spacer()
            .push_button("Export PDF", command(queue, Command::ExportPdf))
            .with_optional_action(actions.and_then(|actions| {
                actions.effect(&crate::actions::TypstEffect::ExportPdf { path: None })
            }));
    }
    let status_id = |name: &str| format!("typst.status.{name}");
    // Hosted controls never fold into More: the diagnostics stay visible even
    // when secondary commands overflow in a narrow window.
    if status.errors + status.warnings == 0 {
        toolbar = toolbar.push_element(Box::new(
            status_chip(
                queue,
                &status_id("no-problems"),
                if compact { "" } else { "No problems" },
                Tone::Success,
                Icon::Check,
                actions,
            )
            .with_accessible_name("No problems"),
        ));
    } else if !export && compact && status.errors > 0 && status.warnings > 0 {
        let label = format!("{} ⚠ {}", status.errors, status.warnings);
        let accessible = format!(
            "{} · {}",
            diagnostic_label(status.errors, "error", "errors"),
            diagnostic_label(status.warnings, "warning", "warnings")
        );
        toolbar = toolbar.push_element(Box::new(
            status_chip(
                queue,
                &status_id("problems"),
                &label,
                Tone::Danger,
                Icon::CircleAlert,
                actions,
            )
            .with_accessible_name(accessible),
        ));
    } else {
        for (count, singular, plural, tone, icon, id) in [
            (
                status.errors,
                "error",
                "errors",
                Tone::Danger,
                Icon::CircleAlert,
                "errors",
            ),
            (
                status.warnings,
                "warning",
                "warnings",
                Tone::Warning,
                Icon::TriangleAlert,
                "warnings",
            ),
        ] {
            if count > 0 {
                let name = diagnostic_label(count, singular, plural);
                let label = if compact {
                    count.to_string()
                } else {
                    name.clone()
                };
                toolbar = toolbar.push_element(Box::new(
                    status_chip(queue, &status_id(id), &label, tone, icon, actions)
                        .with_accessible_name(name),
                ));
            }
        }
    }
    if !export && compact {
        return toolbar;
    }
    let timing = if status.compiling {
        "Compiling…".to_owned()
    } else if status.errors == 0 {
        format!("Compiled in {} ms", status.elapsed_ms)
    } else {
        "Compilation failed".to_owned()
    };
    // Timing is secondary metadata: unlike the diagnostics pill it can move
    // into More on narrow headers, and still opens the same Problems panel.
    toolbar = toolbar
        .push_button(&timing, command(queue, Command::ToggleProblems))
        .with_button_role(ButtonRole::Plain)
        .with_priority(-1)
        .with_optional_action(
            actions.and_then(|actions| actions.effect(&crate::actions::TypstEffect::Problems)),
        );
    toolbar
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
        let diagnostics = Diagnostics::default();
        Self {
            toolbar: build_toolbar(&queue, diagnostics, true, false, None),
            bottom_status: build_toolbar(&queue, diagnostics, false, false, None),
            diagnostics,
            sidebar: Button::icon_only("typst.sidebar", Icon::PanelLeft, "Show sidebar")
                .toggled(false)
                .on_click(command(&queue, Command::ToggleOutline)),
            compact: false,
            footer_density: FooterDensity::Full,
            actions: None,
            retired_toolbars: Vec::new(),
            document,
            document_name,
            view,
            close_problems: Button::icon_only("typst.close-problems", Icon::X, "Close problems")
                .on_click(command(&queue, Command::ToggleProblems)),
            history: ui::widget::HistoryOpener::new(),
            history_rect: Rect::ZERO,
            structure_dirty: true,
            reopen_overflow: false,
            visible: Visible::default(),
            queue,
        }
    }

    /// Bind the actual toolbar commands to the resource's declared operations.
    pub(crate) fn attach_actions(
        &mut self,
        actions: &ui::framework::Actions<crate::resource::Workspace>,
    ) {
        self.actions = Some(actions.clone());
        self.rebuild_status();
        let shown = self.sidebar.is_toggled();
        self.sidebar = actions.bind_effect_invocation(
            Button::icon_only("typst.sidebar", Icon::PanelLeft, sidebar_label(shown))
                .toggled(shown),
            &crate::actions::TypstEffect::Outline,
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
        _problems_shown: bool,
        outline_shown: bool,
        preview_shown: bool,
    ) {
        let name = names.get(current).copied().unwrap_or_default();
        *self.document_name.borrow_mut() = name.to_string();
        // Preserve callers that only know a total; detailed compiler status
        // supplied through `set_diagnostics` takes precedence when it matches.
        if problems != self.diagnostics.errors + self.diagnostics.warnings {
            self.set_diagnostics(
                problems,
                0,
                self.diagnostics.elapsed_ms,
                self.diagnostics.compiling,
            );
        }
        self.sidebar.set_toggled(outline_shown);
        self.sidebar
            .set_accessible_name(sidebar_label(outline_shown));
        self.sidebar
            .set_tooltip(Some(sidebar_label(outline_shown).to_owned()));
        self.view.set_selected(usize::from(preview_shown));
    }

    /// Supply the compiler's separate severities, last elapsed time and busy
    /// state before `sync`. Both status surfaces invoke the existing Problems
    /// action; the parent remains the owner of panel visibility.
    pub fn set_diagnostics(
        &mut self,
        errors: usize,
        warnings: usize,
        elapsed_ms: u128,
        compiling: bool,
    ) {
        let wanted = Diagnostics {
            errors,
            warnings,
            elapsed_ms,
            compiling,
        };
        if wanted != self.diagnostics {
            self.diagnostics = wanted;
            self.rebuild_status();
        }
    }

    fn rebuild_status(&mut self) {
        self.reopen_overflow |= self.toolbar.is_overflow_open();
        for (toolbar, export) in [(&mut self.toolbar, true), (&mut self.bottom_status, false)] {
            let replacement = build_toolbar(
                &self.queue,
                self.diagnostics,
                export,
                if export {
                    self.compact
                } else {
                    self.footer_density == FooterDensity::Compact
                },
                self.actions.as_ref(),
            );
            // Keep widget pointers alive until the host replaces its layout tree.
            self.retired_toolbars
                .push(std::mem::replace(toolbar, replacement));
        }
        self.structure_dirty = true;
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
        self.retired_toolbars.clear();
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
            ..self.visible
        });
        if compact != self.compact {
            self.compact = compact;
            self.rebuild_status();
        }
        // Reserve the primary output control before fitting the filename.
        let overflow_open = self.toolbar.is_overflow_open() || self.reopen_overflow;
        self.toolbar.lay_out_in(self.toolbar.get_rect(), tr, theme);
        let toolbar_min = self
            .toolbar
            .children()
            .into_iter()
            .map(|child| child.get_rect().width() + gap)
            .sum::<f64>()
            + control;
        let natural = self.natural_document_width(tr, theme);
        let mut x = bar.x0 + pad;
        self.sidebar.apply_layout_external(row(x, x + control));
        x += control + gap;

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
        let picker_rect = if compact {
            let view_w = SegmentedFace::intrinsic_width(
                &["Edit", "Preview"],
                ControlSize::Regular,
                tr,
                theme,
            )
            .min((right - x - toolbar_min - gap).max(0.0));
            // The filename gives way to the switch, status and More chip.
            let picker_w = natural.min((right - x - view_w - toolbar_min - 2.0 * gap).max(0.0));
            let picker_rect = row(x, x + picker_w);
            x += picker_w + if picker_w > 0.0 { gap } else { 0.0 };
            view_rect = row(x, x + view_w);
            x += view_w + gap;
            picker_rect
        } else {
            let picker_rect = row(x, x + natural.min((right - x - toolbar_min - gap).max(0.0)));
            x = picker_rect.x1 + gap;
            picker_rect
        };
        let toolbar_rect = row(x, right);

        self.document.apply_layout_external(picker_rect);
        self.view.apply_layout_external(view_rect);
        self.toolbar.lay_out_in(toolbar_rect, tr, theme);
        if overflow_open {
            self.toolbar.open_overflow();
        }
        self.reopen_overflow = false;
    }

    /// Place the bottom diagnostics controls in a reserved part of the status
    /// bar, after calling `place`. The parent must omit its old compile-status
    /// text from this region. `None` removes the controls from focus and
    /// accessibility traversal.
    pub fn place_status(&mut self, rect: Option<Rect>, tr: &mut TextRenderer, theme: &Theme) {
        self.show(Visible {
            bottom_status: rect.is_some(),
            ..self.visible
        });
        let compact = rect
            .is_some_and(|rect| rect.width() < f64::from(theme.tokens.metrics.panel_width_list));
        let density = if compact {
            FooterDensity::Compact
        } else {
            FooterDensity::Full
        };
        if density != self.footer_density {
            self.footer_density = density;
            self.rebuild_status();
        }
        self.bottom_status
            .lay_out_in(rect.unwrap_or(Rect::ZERO), tr, theme);
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

    /// Open the picker's list: a screenshot scenario showing it.
    pub fn open_picker(&mut self) {
        self.toolbar.open_overflow();
    }

    /// Every control, in tree order: the layout tree, event routing and
    /// accessibility all follow it.
    pub fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(7);
        children.push(&self.sidebar);
        children.push(&self.document);
        if self.visible.view {
            children.push(&self.view);
        }
        children.push(&self.toolbar);
        if self.visible.bottom_status {
            children.push(&self.bottom_status);
        }
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
        children.push(&mut self.sidebar);
        children.push(&mut self.document);
        if self.visible.view {
            children.push(&mut self.view);
        }
        children.push(&mut self.toolbar);
        if self.visible.bottom_status {
            children.push(&mut self.bottom_status);
        }
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
        Self::nodes(&self.toolbar)
    }

    fn bottom_status_names(&self) -> Vec<String> {
        Self::nodes(&self.bottom_status)
            .into_iter()
            .filter_map(|node| node.name)
            .collect()
    }

    fn nodes(toolbar: &Toolbar) -> Vec<ui::a11y::AccessNode> {
        fn flatten(node: ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
            for child in node.children.clone() {
                flatten(child, out);
            }
            out.push(node);
        }
        fn visit(widget: &dyn Widget, out: &mut Vec<ui::a11y::AccessNode>) {
            if let Some(root) = widget.accessibility() {
                flatten(root, out);
            }
            for child in widget.children() {
                visit(child, out);
            }
        }
        let mut out = Vec::new();
        visit(toolbar, &mut out);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_and_timing_only_appear_in_the_footer() {
        let mut chrome = Chrome::new();
        let mut text = TextRenderer::new();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for (errors, warnings) in [(0, 0), (2, 1), (0, 3), (1, 0), (0, 0)] {
                chrome.set_diagnostics(errors, warnings, 37, false);
                chrome.sync(&["note.typ"], 0, errors + warnings, false, false, false);
                chrome.place(
                    Rect::new(0.0, 0.0, 1440.0, 60.0),
                    false,
                    1,
                    &mut text,
                    &theme,
                );
                assert_eq!(chrome.toolbar_names(), vec!["Export PDF"]);
                chrome.place_status(
                    Some(Rect::new(0.0, 100.0, 1400.0, 150.0)),
                    &mut text,
                    &theme,
                );
                let names = chrome.bottom_status_names();
                assert_eq!(
                    names.iter().any(|name| name == "No problems"),
                    errors + warnings == 0
                );
                assert_eq!(
                    names
                        .iter()
                        .any(|name| name.ends_with("error") || name.ends_with("errors")),
                    errors > 0
                );
                assert_eq!(
                    names
                        .iter()
                        .any(|name| name.ends_with("warning") || name.ends_with("warnings")),
                    warnings > 0
                );
                let timing = if errors == 0 {
                    "Compiled in 37 ms"
                } else {
                    "Compilation failed"
                };
                assert!(names.iter().any(|name| name == timing), "{names:?}");
                for node in Chrome::nodes(&chrome.bottom_status)
                    .into_iter()
                    .filter(|node| {
                        node.name.as_deref() == Some("No problems")
                            || node.name.as_ref().is_some_and(|name| {
                                name.ends_with("error")
                                    || name.ends_with("errors")
                                    || name.ends_with("warning")
                                    || name.ends_with("warnings")
                            })
                    })
                {
                    assert_eq!(node.role, ui::a11y::Role::Button);
                    assert!(
                        node.bounds
                            .is_some_and(|bounds| bounds.to_rect().area() > 0.0)
                    );
                }
                chrome.layout_rebuilt();
            }
        }
    }

    fn activate_status(widget: &mut dyn Widget, id: &str) -> bool {
        if widget.test_id() == Some(id) {
            let mut doc = automerge::AutoCommit::new();
            return widget.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| activate_status(child, id))
    }

    #[test]
    fn footer_status_uses_the_existing_problems_command() {
        let mut chrome = Chrome::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        chrome.place_status(
            Some(Rect::new(0.0, 100.0, 1000.0, 140.0)),
            &mut text,
            &theme,
        );
        for (errors, warnings, ids) in [
            (0, 0, vec!["typst.no-problems"]),
            (1, 2, vec!["typst.errors", "typst.warnings"]),
        ] {
            chrome.set_diagnostics(errors, warnings, 19, false);
            chrome.place(
                Rect::new(0.0, 0.0, 1440.0, 60.0),
                false,
                1,
                &mut text,
                &theme,
            );
            chrome.place_status(
                Some(Rect::new(0.0, 100.0, 1000.0, 140.0)),
                &mut text,
                &theme,
            );
            for id in ids {
                assert!(!activate_status(&mut chrome.toolbar, id));
                assert!(chrome.take().is_empty());
                let bottom_id = id.replace("typst.", "typst.status.");
                assert!(activate_status(&mut chrome.bottom_status, &bottom_id));
                assert_eq!(chrome.take(), vec![Command::ToggleProblems]);
            }
        }
        assert_eq!(chrome.children().len(), 4);
        chrome.place_status(None, &mut text, &theme);
        assert_eq!(chrome.children().len(), 3);
    }

    #[test]
    fn narrow_headers_only_offer_export_and_view_controls() {
        let mut chrome = Chrome::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let bar = Rect::new(0.0, 0.0, 390.0, 60.0);
        for (errors, warnings) in [(0, 0), (2, 1)] {
            chrome.set_diagnostics(errors, warnings, 37, false);
            chrome.sync(
                &["motion-notes.typ"],
                0,
                errors + warnings,
                false,
                false,
                false,
            );
            chrome.place(bar, true, 1, &mut text, &theme);
            for child in chrome.toolbar.children() {
                let rect = child.get_rect();
                assert!(
                    rect.area() > 0.0 && rect.x0 >= 0.0 && rect.x1 <= bar.x1,
                    "{rect:?}"
                );
                assert!(rect.x0 >= chrome.view.get_rect().x1, "{rect:?}");
            }
            let names = chrome.toolbar_names();
            assert!(!names.iter().any(|name| name.contains("error")
                || name.contains("warning")
                || name == "No problems"));
        }
    }

    #[test]
    fn narrow_footer_keeps_both_severities_visible_without_an_overflow_chip() {
        for scale in [1.0, 2.0] {
            let mut chrome = Chrome::new();
            let mut text = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(scale);
            chrome.set_diagnostics(1, 2, 19, false);
            let bar = Rect::new(0.0, 100.0, 180.0, 160.0);
            chrome.place_status(Some(bar), &mut text, &theme);
            assert!(!chrome.bottom_status.has_overflow());
            let pills = chrome.bottom_status.children();
            assert_eq!(pills.len(), 1);
            let pill = pills[0];
            assert_eq!(
                pill.accessibility().unwrap().name.as_deref(),
                Some("1 error · 2 warnings")
            );
            assert!(pill.get_rect().x0 >= bar.x0 && pill.get_rect().x1 <= bar.x1);
        }
    }

    #[test]
    fn busy_status_does_not_claim_a_completed_compile() {
        let mut chrome = Chrome::new();
        let mut text = TextRenderer::new();
        chrome.set_diagnostics(0, 0, 37, true);
        chrome.place(
            Rect::new(0.0, 0.0, 1440.0, 60.0),
            false,
            1,
            &mut text,
            &Theme::dark(),
        );
        chrome.place_status(
            Some(Rect::new(0.0, 100.0, 1400.0, 150.0)),
            &mut text,
            &Theme::dark(),
        );
        let names = chrome.bottom_status_names();
        assert!(names.iter().any(|name| name == "Compiling…"));
        assert!(!names.iter().any(|name| name.starts_with("Compiled in")));
    }

    #[test]
    fn outline_face_tracks_visibility_without_rebuilding_bound_commands() {
        let mut chrome = Chrome::new();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        for shown in [false, true, false] {
            chrome.sync(&["note.typ"], 0, 0, false, shown, false);
            chrome.place(
                Rect::new(0.0, 0.0, 1440.0, 60.0),
                false,
                1,
                &mut text,
                &theme,
            );
            let outline = chrome.sidebar.accessibility().unwrap();
            assert_eq!(outline.name.as_deref(), Some(sidebar_label(shown)));
            assert_eq!(outline.role, ui::a11y::Role::ToggleButton);
            assert_eq!(outline.state.checked, Some(shown));
            assert!(chrome.sidebar.get_rect().x1 < chrome.document.get_rect().x0);
            assert_eq!(chrome.children()[0].test_id(), Some("typst.sidebar"));
            assert!(activate_status(&mut chrome.sidebar, "typst.sidebar"));
            assert_eq!(chrome.take(), vec![Command::ToggleOutline]);
            let names = chrome.toolbar_names();
            assert!(names.iter().any(|name| name == "Export PDF"));
            for absent in [
                "Bold",
                "Italic",
                "Math",
                "Cite",
                "Section",
                "Zoom in",
                "Zoom out",
                "Fit width",
                "Save",
                "Open",
                "Problems",
            ] {
                assert!(
                    !names.iter().any(|name| name == absent),
                    "{absent}: {names:?}"
                );
            }
        }
    }
}
