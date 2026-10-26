//! Canvas's controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the artwork's title as the window's location (editable in place, so the
//! person renames where they are), the artwork's menu, Undo, Redo, Add
//! picture, Export, the zoom controls and whether the tools show. The window
//! draws no header row of its own: the canvas keeps the room the row took,
//! as every other app's content does.
//!
//! What a slot stands for is what the control it replaces stood for: the same
//! view request, the same dialog, the same artwork action.

use ui::icons::Icon;
use ui::prelude::*;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use crate::model::Artwork;
use crate::tools::{Sheet, Tools, ViewRequest};

/// The artwork's menu button in the bar. Never sent: its items are.
pub(crate) const MENU: &str = "canvas.menu";
/// Undo the last step, and bring back what the last undo took back.
pub(crate) const UNDO: &str = "canvas.undo";
pub(crate) const REDO: &str = "canvas.redo";
/// Place a picture on the artwork.
pub(crate) const ADD_PICTURE: &str = "canvas.add-picture";
/// The export dialog.
pub(crate) const EXPORT: &str = "canvas.export";
/// The zoom controls, and the zoom the canvas is at in their name. Never
/// sent: its items are.
pub(crate) const ZOOM: &str = "canvas.zoom";
pub(crate) const ZOOM_IN: &str = "canvas.zoom-in";
pub(crate) const ZOOM_OUT: &str = "canvas.zoom-out";
pub(crate) const ZOOM_FIT: &str = "canvas.zoom-fit";
pub(crate) const ZOOM_ACTUAL: &str = "canvas.zoom-actual";
/// Show the tools, or put them away.
pub(crate) const TOOLS: &str = "canvas.tools";

/// How the artwork menu's commands are told from the bar's own actions: the
/// value the file desk carries out follows ([`Tools::menu`]).
const CARRY_OUT: &str = "canvas.carry-out.";

/// The artwork menu's commands, by the value the file desk carries out, and
/// the words each says.
const COMMANDS: [(&str, &str); 7] = [
    ("new", "canvas-menu-new"),
    ("open", "canvas-menu-open"),
    ("save", "canvas-menu-save"),
    ("save-as", "canvas-menu-save-as"),
    ("duplicate", "canvas-menu-duplicate"),
    ("export", "canvas-menu-export"),
    ("keys", "canvas-menu-keys"),
];

/// The window bar's slots for what the window shows now.
fn toolbar(tools: &Tools, title: &str, open: bool) -> WindowToolbar {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let commands = COMMANDS
        .into_iter()
        .map(|(value, id)| {
            ToolbarAction::new(format!("{CARRY_OUT}{value}"), Icon::Menu.name(), lookup(id))
        })
        .collect();
    let zoom = tools.zoom.with(|zoom| zoom * 100.0);
    let percent = if zoom < 10.0 {
        format!("{zoom:.1}%")
    } else {
        format!("{zoom:.0}%")
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: if title.is_empty() {
                lookup("canvas-untitled")
            } else {
                title.to_owned()
            },
            value: title.to_owned(),
            editable: true,
            name: lookup("canvas-label-artwork-title"),
            go_name: lookup("canvas-rename"),
            ..ToolbarLocation::default()
        }),
        actions: vec![
            ToolbarAction::new(MENU, Icon::Menu.name(), lookup("canvas-menu")).with_items(commands),
            ToolbarAction::new(UNDO, Icon::Undo2.name(), lookup("canvas-undo")),
            ToolbarAction::new(REDO, Icon::Redo2.name(), lookup("canvas-redo")),
            ToolbarAction::new(
                ADD_PICTURE,
                Icon::Image.name(),
                lookup("canvas-add-picture"),
            ),
            ToolbarAction::new(EXPORT, Icon::Download.name(), lookup("canvas-export-title")),
            // The zoom the canvas is at is said in the name, so the bar tells
            // the person where they are working without a readout of its own.
            ToolbarAction::new(
                ZOOM,
                Icon::Search.name(),
                ui::i18n::I18nManager::format("canvas-zoom-at", &[("percent", percent.into())]),
            )
            .with_items(vec![
                ToolbarAction::new(ZOOM_IN, Icon::Plus.name(), lookup("canvas-zoom-in")),
                ToolbarAction::new(ZOOM_OUT, Icon::Minus.name(), lookup("canvas-zoom-out")),
                ToolbarAction::new(ZOOM_FIT, Icon::Maximize2.name(), lookup("canvas-zoom-fit")),
                ToolbarAction::new(
                    ZOOM_ACTUAL,
                    Icon::Maximize.name(),
                    lookup("canvas-zoom-actual"),
                ),
            ])
            .in_menu(),
            // The tools' toggle stands last, where the shell puts an
            // inspector's.
            ToolbarAction::new(
                TOOLS,
                Icon::PanelRight.name(),
                lookup(if open {
                    "canvas-hide-tools"
                } else {
                    "canvas-show-tools"
                }),
            )
            .on(open),
        ],
        ..WindowToolbar::default()
    }
}

/// The window bar's slots for this window, and what the person did in them.
///
/// The canvas keeps no application struct of its own to declare from, so this
/// decorator over the window does: it declares on every pass (an equal
/// declaration is free) and carries out what a slot asked through the same
/// local state the control it replaces wrote, or through the artwork's own
/// action.
pub(crate) struct WindowBar {
    child: Element,
    tools: Tools,
    dispatch: ui::framework::Dispatcher<Artwork>,
}

impl WindowBar {
    pub(crate) fn new(
        child: Element,
        tools: &Tools,
        dispatch: ui::framework::Dispatcher<Artwork>,
    ) -> Self {
        Self {
            child,
            tools: tools.clone(),
            dispatch,
        }
    }

    /// Carry out one thing the person did in the bar.
    fn run(&self, event: &ToolbarEvent) {
        match event {
            // The location is the artwork's title: the bar edits it in place,
            // and the artwork's own action renames it as the title field did.
            ToolbarEvent::Commit { text } => {
                self.dispatch.submit(Artwork::SET_TITLE.with(text.clone()));
            }
            ToolbarEvent::Action { id } => self.run_action(id),
            _ => {}
        }
    }

    fn run_action(&self, id: &str) {
        let tools = &self.tools;
        if let Some(value) = id.strip_prefix(CARRY_OUT) {
            tools.menu.set(value.to_owned());
            return;
        }
        match id {
            UNDO => self.dispatch.submit(Artwork::UNDO.invoke()),
            REDO => self.dispatch.submit(Artwork::REDO.invoke()),
            ADD_PICTURE => super::header::pick_picture(tools),
            EXPORT => tools.sheet.set(Some(Sheet::Export)),
            ZOOM_IN => tools.view.set(Some(ViewRequest::ZoomIn)),
            ZOOM_OUT => tools.view.set(Some(ViewRequest::ZoomOut)),
            ZOOM_FIT => tools.view.set(Some(ViewRequest::Fit)),
            ZOOM_ACTUAL => tools.view.set(Some(ViewRequest::Pixel)),
            TOOLS => {
                let open = tools.sidebar.get();
                tools.sidebar.set(!open);
            }
            _ => {}
        }
    }

    fn declare(&self) {
        let title = self.tools.title.get();
        let open = self.tools.sidebar.get();
        ui::window_toolbar::declare(Some(toolbar(&self.tools, &title, open)));
    }
}

impl ui::widget::Decorator for WindowBar {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.child.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.child.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        for event in ui::window_toolbar::take_events() {
            self.run(&event);
        }
        self.declare();
        self.child.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            self.run(&event);
            asked = true;
        }
        // The zoom percentage is in the bar's own words, and it changes
        // without the tree being rebuilt.
        self.declare();
        self.child.update(state, path) || asked
    }

    fn needs_redraw(&self) -> bool {
        self.child.needs_redraw()
    }

    fn concrete_type_name(&self) -> &'static str {
        "CanvasWindowBar"
    }
}
