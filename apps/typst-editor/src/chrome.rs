//! The editor's window chrome: its commands, its views and where the window
//! is are the window bar's slots ([`ui::window_toolbar`]), never a row of the
//! editor's own. The window is the document: its location is the file's name,
//! marked unsaved while the source differs from what was last written, its
//! views are the source and the preview, and Compile, Problems, Outline, the
//! preview's zoom, Save, Open and Export PDF are its actions. The History
//! control is the bar's own.
//!
//! What the slots do comes back as [`Command`]s, the same commands the
//! toolbar pushed, so nothing an action can do is reachable only by a control
//! the app draws. The only control left in the body is the Problems panel's
//! own close button, which closes that panel and not the window.

use std::cell::RefCell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, Widget};
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

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
    ToggleOutline,
    /// Narrow window: show the source editor pane.
    ShowEditor,
    /// Narrow window: show the rendered preview pane.
    ShowPreview,
}

type Queue = Rc<RefCell<Vec<Command>>>;

/// The slots' ids, which come back in [`ToolbarEvent::Action`].
const COMPILE: &str = "typst.compile";
const PROBLEMS: &str = "typst.problems";
const OUTLINE: &str = "typst.outline";
const ZOOM_IN: &str = "typst.zoom-in";
const ZOOM_OUT: &str = "typst.zoom-out";
const FIT: &str = "typst.fit";
const SAVE: &str = "typst.save";
const OPEN: &str = "typst.open";
const EXPORT_PDF: &str = "typst.export-pdf";
const EDITOR_PANE: &str = "typst.view.editor";
const PREVIEW_PANE: &str = "typst.view.preview";

/// The height of one control in `theme`, measured at the current text scale.
pub(crate) fn control_height(tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let metrics = &theme.tokens.metrics;
    let line = tr
        .measure_style("Ag", None, &theme.text(ui::tokens::TextRole::Label))
        .1;
    f64::from(metrics.min_pointer_target)
        .max(f64::from(line) + 2.0 * f64::from(theme.tokens.spacing.s2))
}

pub struct Chrome {
    queue: Queue,
    close_problems: Button,
    /// The window's document, as the bar shows it: its name, and whether the
    /// source is not what was last written.
    name: String,
    edited: bool,
    problems: usize,
    problems_shown: bool,
    outline_shown: bool,
    preview_shown: bool,
    /// The Problems panel's close button is laid out and in the tree.
    close: bool,
    structure_dirty: bool,
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

/// The Problems toggle's name: the count of problems, when there are any.
fn problems_name(problems: usize) -> String {
    if problems == 0 {
        ui::i18n::I18nManager::lookup_plain("typst-problems")
    } else {
        ui::i18n::I18nManager::format("typst-problems-count", &[("count", problems.into())])
    }
}

impl Chrome {
    pub fn new() -> Self {
        let queue: Queue = Rc::default();
        Self {
            close_problems: Button::icon_only(
                "typst.close-problems",
                Icon::X,
                &ui::i18n::I18nManager::lookup_plain("typst-close-problems"),
            )
            .on_click({
                let queue = Rc::clone(&queue);
                move || queue.borrow_mut().push(Command::ToggleProblems)
            }),
            queue,
            name: String::new(),
            edited: false,
            problems: 0,
            problems_shown: false,
            outline_shown: false,
            preview_shown: false,
            close: false,
            structure_dirty: true,
        }
    }

    /// Bind the panel's close button to the resource's declared operation.
    pub(crate) fn attach_actions(
        &mut self,
        actions: &ui::framework::Actions<crate::resource::Workspace>,
    ) {
        self.close_problems = actions.bind_effect_invocation(
            Button::icon_only(
                "typst.close-problems",
                Icon::X,
                &ui::i18n::I18nManager::lookup_plain("typst-close-problems"),
            ),
            &crate::actions::TypstEffect::Problems,
        );
        self.structure_dirty = true;
    }

    /// Everything the slots were asked to do since the last call.
    pub fn take(&mut self) -> Vec<Command> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    /// Make the bar say what the editor holds: the document's name and
    /// whether it is unsaved, the problem count and which panels are open,
    /// and the pane a narrow window shows.
    pub fn sync(
        &mut self,
        name: &str,
        edited: bool,
        problems: usize,
        problems_shown: bool,
        outline_shown: bool,
        preview_shown: bool,
    ) {
        self.name = name.to_owned();
        self.edited = edited;
        self.problems = problems;
        self.problems_shown = problems_shown;
        self.outline_shown = outline_shown;
        self.preview_shown = preview_shown;
    }

    /// The window bar's slots for what the editor holds now.
    fn toolbar(&self) -> WindowToolbar {
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let editor = ToolbarView {
            id: EDITOR_PANE.to_owned(),
            label: lookup("typst-view-editor"),
        };
        let preview = ToolbarView {
            id: PREVIEW_PANE.to_owned(),
            label: lookup("typst-view-preview"),
        };
        WindowToolbar {
            location: Some(ToolbarLocation {
                label: self.name.clone(),
                // The file's name is the location's value too: the bar edits
                // it in place, and a committed name renames the file.
                value: self.name.clone(),
                editable: true,
                name: lookup("typst-document-name"),
                go_name: lookup("typst-rename"),
                // A document with changes not yet written says so, the one
                // way every editor does.
                edited: self.edited,
                ..ToolbarLocation::default()
            }),
            views: Some(ToolbarViews {
                name: lookup("typst-view"),
                choices: vec![editor, preview],
                chosen: if self.preview_shown {
                    PREVIEW_PANE.to_owned()
                } else {
                    EDITOR_PANE.to_owned()
                },
            }),
            actions: vec![
                ToolbarAction::new(COMPILE, Icon::Play.name(), lookup("typst-compile")),
                ToolbarAction::new(
                    PROBLEMS,
                    Icon::TriangleAlert.name(),
                    problems_name(self.problems),
                )
                .on(self.problems_shown),
                ToolbarAction::new(OUTLINE, Icon::List.name(), lookup("typst-outline"))
                    .on(self.outline_shown),
                ToolbarAction::new(ZOOM_OUT, Icon::Minus.name(), lookup("typst-zoom-out")),
                ToolbarAction::new(ZOOM_IN, Icon::Plus.name(), lookup("typst-zoom-in")),
                ToolbarAction::new(FIT, Icon::Maximize.name(), lookup("typst-fit-width")),
                ToolbarAction::new(SAVE, Icon::Download.name(), lookup("typst-save")),
                ToolbarAction::new(OPEN, Icon::FolderOpen.name(), lookup("typst-open")),
                ToolbarAction::new(
                    EXPORT_PDF,
                    Icon::Download.name(),
                    lookup("typst-export-pdf"),
                )
                .in_menu(),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Declare the bar's slots, and run what the person did in them.
    ///
    /// Returns whether the editor has to look again (a command was asked
    /// for). An equal declaration is free, so this runs every pass.
    pub fn declare(&mut self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked |= self.handle(&event);
        }
        ui::window_toolbar::declare(Some(self.toolbar()));
        asked
    }

    /// What one thing done in the bar asks the editor to do.
    fn handle(&mut self, event: &ToolbarEvent) -> bool {
        let command = match event {
            ToolbarEvent::Action { id } => match id.as_str() {
                COMPILE => Some(Command::Compile),
                PROBLEMS => Some(Command::ToggleProblems),
                OUTLINE => Some(Command::ToggleOutline),
                ZOOM_IN => Some(Command::ZoomIn),
                ZOOM_OUT => Some(Command::ZoomOut),
                FIT => Some(Command::Fit),
                SAVE => Some(Command::Save),
                OPEN => Some(Command::Open),
                EXPORT_PDF => Some(Command::ExportPdf),
                EDITOR_PANE => Some(Command::ShowEditor),
                PREVIEW_PANE => Some(Command::ShowPreview),
                _ => None,
            },
            _ => None,
        };
        if let Some(command) = command {
            self.queue.borrow_mut().push(command);
        }
        command.is_some()
    }

    /// Whether the slots were rebuilt since the layout tree was.
    pub fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    /// The layout tree was rebuilt.
    pub fn layout_rebuilt(&mut self) {}

    /// Place the Problems panel's close button; `None` hides it.
    pub fn place_close(&mut self, rect: Option<Rect>) {
        let shown = rect.is_some();
        if shown != self.close {
            self.close = shown;
            self.structure_dirty = true;
        }
        self.close_problems
            .apply_layout_external(rect.unwrap_or(Rect::ZERO));
    }

    /// Whether a control holds an open list, which then owns the keyboard
    /// until it closes.
    pub fn captures_input(&self) -> bool {
        self.children().iter().any(|child| {
            child.wants_pointer_capture() || child.modal_capture().is_some() || child.traps_focus()
        })
    }

    /// Every control, in tree order: the layout tree, event routing and
    /// accessibility all follow it.
    pub fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(1);
        if self.close {
            children.push(&self.close_problems);
        }
        children
    }

    pub fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(1);
        if self.close {
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
