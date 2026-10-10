//! Canvas: vector drawing and painting in real media, as fluent for an agent
//! as for a person.
//!
//! The artwork is layers of marks, each a vector object with a stable id. A
//! plain layer draws its marks as crisp vector art; an oil or watercolour
//! layer paints them with a simulation of that medium, re-simulated from the
//! marks on every machine. Everything a person does with the pointer is one
//! of the artwork's actions, and `look` lets a caller with no screen see the
//! result.

/// The application id.
pub const APP_ID: &str = "so.tilo.canvas";
/// The window and launcher title.
pub const APP_TITLE: &str = "Canvas";

pub mod actions;
pub mod adjust;
pub mod answer;
pub mod author;
pub mod background;
pub mod brushes;
pub mod content;
pub mod content_geometry;
pub mod detail;
pub mod document;
pub mod fonts;
pub mod gesture;
pub mod locales;
pub mod media;
pub mod menu;
pub mod model;
pub mod palette;
pub mod panels;
pub mod picture;
pub mod preview;
pub mod region;
pub mod render;
pub mod resource;
pub mod sample;
pub mod selection;
pub mod settle;
pub mod strokes;
pub mod subject;
pub mod surface;
pub mod svg;
pub mod thumb;
pub mod tools;
pub mod view_ops;
pub mod wand;

pub use model::Artwork;

use panels::pane::Group;
use ui::LocalState;
use ui::prelude::*;
use ui::widget::ControlSize;

pub struct App;

impl ResourceApp for App {
    type Resource = Artwork;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some("inspector-narrow") {
            (800, 600)
        } else {
            (1280, 800)
        }
    }

    fn view(cx: &mut ViewCx<Artwork>) -> Element {
        locales::install();
        let recents = cx.settings::<document::RecentArtworks>();
        let tools = tools::Tools {
            recents: recents.state(),
            ..tools::Tools::default()
        };
        let compact = LocalState::new(false);
        let dock = LocalState::new(Dock::Layers);
        let staged = dock.clone();
        cx.on_stage(move |scenario| {
            if scenario == Some("history") {
                staged.set(Dock::History);
            }
        });
        let work_tools = tools.clone();
        let work = cx.switch(&compact, move |cx, compact| {
            work_area(cx, &work_tools, *compact)
        });
        // The narrow capture shows the inspector as the drawer it is there.
        let inspector_open = tools.sidebar.clone();
        let stage_open = inspector_open.clone();
        cx.on_stage(move |scenario| {
            if scenario == Some("inspector-narrow") {
                stage_open.set(true);
            }
        });
        let window = Box::new(
            NavShell::new()
                .with_id("canvas-shell")
                .with_inspector_state(&inspector_open)
                .content(work)
                .content_header_leading(panels::header::title(cx, &tools, &compact))
                .content_header("", panels::header::header(cx, &tools, &compact))
                .inspector(Box::new(Group::new(sidebar(cx, &tools, &dock)))),
        );
        // Over a blank artwork, a welcome saying what can be done here and
        // by whom; then the dialogs.
        let mut layers = Stack::new().push(window);
        for sheet in sheets(cx, &tools) {
            layers = layers.push(sheet);
        }
        Box::new(panels::layout::ViewportMode::new(
            panels::files::FileDesk::new(
                Box::new(layers),
                tools,
                recents,
                cx.live().clone(),
                cx.dispatcher(),
            ),
            compact,
        ))
    }

    /// A saved artwork, handed over by Files or named on the command
    /// line; and pictures, which open as an artwork the size of the
    /// picture.
    const OPENS: &'static [&'static str] = &[
        document::MEDIA_TYPE,
        "image/png",
        "image/jpeg",
        "image/webp",
    ];
    const ARGUMENTS: usize = 1;

    /// A file named on the command line opens a record of its own, the
    /// same one each time: an artwork file is that record; a picture gets
    /// an artwork derived from it, edits and all.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        let path = std::path::Path::new(input.argument()?);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)?;
        if extension == document::EXTENSION {
            return Some(RecordId::identify_file(path));
        }
        matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp")
            .then(|| RecordId::derived("artwork", &RecordId::identify_file(path)))
    }

    /// The file, in the record it opened: an artwork file is that artwork,
    /// a picture a new artwork sized to it with the picture its first mark.
    /// A record made for the file starts as it; opened again, it is as it
    /// was left. A file handed to the window's artwork (no path) replaces
    /// it as one step, Undo away from what was there.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Artwork>> {
        let Some(file) = input.file() else {
            return Vec::new();
        };
        if !created && input.argument().is_some() {
            return Vec::new();
        }
        let open = if file.media_type() == document::MEDIA_TYPE {
            let Ok(artwork) = document::unpack(
                file.bytes(),
                &input
                    .content()
                    .cloned()
                    .unwrap_or_else(crate::content::store),
            ) else {
                return Vec::new();
            };
            Artwork::OPEN_ARTWORK.with(actions::OpenArtwork {
                file: String::from_utf8_lossy(&artwork.to_json()).into_owned(),
            })
        } else {
            match resource::open_picture(file.name(), file.bytes()) {
                Ok(open) => open,
                Err(_) => return Vec::new(),
            }
        };
        vec![open]
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "landscape",
            "close-up",
            "selection",
            "turned",
            "points",
            "history",
            "inspector-narrow",
        ]
    }

    /// The screenshots paint their samples through the same actions an
    /// agent would; a plain `--shots` shows the empty sheet a new artwork
    /// is.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Artwork>> {
        match name {
            Some("landscape" | "close-up" | "history" | "inspector-narrow") => sample::landscape(),
            Some("selection" | "turned" | "points") => sample::shapes(),
            _ => Vec::new(),
        }
    }

    fn label(art: &Artwork) -> String {
        if art.title.is_empty() {
            APP_TITLE.to_owned()
        } else {
            art.title.clone()
        }
    }
}

/// The dialogs over the window: starting a new artwork, exporting it, and
/// the keyboard shortcuts.
fn sheets(cx: &ViewCx<Artwork>, tools: &tools::Tools) -> Vec<Element> {
    use tools::{FileRequest, Sheet};
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let close = |tools: &tools::Tools| {
        let sheet = tools.sheet.clone();
        move || sheet.set(None)
    };
    let body = |content: Element| Container::new().with_no_padding().with_child(content);
    let new = {
        let dispatch = cx.dispatcher();
        let tools = tools.clone();
        let (cancel, dismiss) = (close(&tools), close(&tools));
        ui::widget::ModalDialog::new(
            lookup("canvas-command-new-artwork"),
            body(panels::start::body(cx, &tools)),
        )
        .with_test_id("canvas-new-dialog")
        .primary_action(lookup("canvas-create"), move || {
            panels::start::create(&dispatch, &tools);
        })
        .secondary_action(lookup("canvas-cancel"), cancel)
        .on_dismiss(dismiss)
        .with_max_width(680.0)
    };
    let export = {
        let tools = tools.clone();
        let (cancel, dismiss) = (close(&tools), close(&tools));
        ui::widget::ModalDialog::new(
            lookup("canvas-export-title"),
            body(panels::export::body(cx, &tools)),
        )
        .with_test_id("canvas-export-dialog")
        .primary_action(lookup("canvas-export-save"), move || {
            tools.sheet.set(None);
            tools
                .files
                .set(Some(FileRequest::Export(tools.export.settings())));
        })
        .secondary_action(lookup("canvas-cancel"), cancel)
        .on_dismiss(dismiss)
        .with_max_width(780.0)
    };
    let keys = ui::widget::ModalDialog::new(
        lookup("canvas-keys-title"),
        body(panels::shortcuts::overview()),
    )
    .with_test_id("canvas-keys-dialog")
    .primary_action(lookup("canvas-done"), close(tools))
    .on_dismiss(close(tools))
    .with_max_width(820.0);
    vec![
        Box::new(Shown::new(new, tools.sheet.clone(), Sheet::NewArtwork)),
        Box::new(Shown::new(export, tools.sheet.clone(), Sheet::Export)),
        Box::new(Shown::new(keys, tools.sheet.clone(), Sheet::Shortcuts)),
    ]
}

/// A dialog shown while the window's sheet is `which`. The dialog stays in
/// the tree, so what was typed in it outlives closing it.
struct Shown {
    dialog: ui::widget::ModalDialog<Container>,
    sheet: LocalState<Option<tools::Sheet>>,
    which: tools::Sheet,
}

impl Shown {
    fn new(
        dialog: ui::widget::ModalDialog<Container>,
        sheet: LocalState<Option<tools::Sheet>>,
        which: tools::Sheet,
    ) -> Self {
        Self {
            dialog,
            sheet,
            which,
        }
    }
}

impl ui::widget::Decorator for Shown {
    type Inner = ui::widget::ModalDialog<Container>;

    fn inner(&self) -> &Self::Inner {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.dialog
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        let open = self.sheet.with(|sheet| *sheet == Some(self.which));
        if open != self.dialog.is_open() {
            self.dialog.set_open(open);
        }
        self.dialog.sync(state, path);
    }

    /// A click or key right after the dialog was asked for already lands
    /// in it, not in what is beneath, even before the next frame.
    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let open = self.sheet.with(|sheet| *sheet == Some(self.which));
        if open != self.dialog.is_open() {
            self.dialog.set_open(open);
        }
        ui::widget::Widget::handle_event(&mut self.dialog, event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.sheet.with(|sheet| *sheet == Some(self.which)) != self.dialog.is_open()
            || self.dialog.needs_redraw()
    }

    fn concrete_type_name(&self) -> &'static str {
        "CanvasSheet"
    }
}

/// What the lower half of the sidebar shows.
/// What the lower half of the inspector shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Dock {
    #[default]
    Layers,
    Objects,
    History,
}

/// The tool rail's width.
pub(crate) const RAIL: f32 =
    ui::tokens::Metrics::BASE.min_pointer_target + 2.0 * ui::tokens::Spacing::BASE.s2; // a pointer target and its inset, laid out before a theme is known

/// Recommended tools follow the document; a person can override the suggestion.
fn rail(cx: &ViewCx<Artwork>, tools: &tools::Tools, compact: bool) -> Element {
    let inferred = cx.value(tools::Workspace::of);
    let tools = tools.clone();
    cx.switch(&inferred, move |cx, inferred| {
        let tools = tools.clone();
        let inferred = *inferred;
        cx.switch(&tools.workspace.clone(), move |cx, wanted| {
            let workspace = if *wanted == tools::Workspace::Auto {
                inferred
            } else {
                *wanted
            };
            let tools = tools.clone();
            cx.switch(&tools.all_tools.clone(), move |cx, _| {
                let tools = tools.clone();
                cx.switch(&tools.tool.clone(), move |_cx, _| {
                    rail_choices(&tools, workspace, compact)
                })
            })
        })
    })
}

/// On a phone the artwork keeps the full width and tools sit below it.
/// Every tool remains the same shared control and local view action.
fn work_area(cx: &ViewCx<Artwork>, tools: &tools::Tools, compact: bool) -> Element {
    let canvas = cx.surface(surface::CanvasSurface::new(tools.clone()));
    let canvas: Element = Box::new(preview::VersionPreview::new(
        canvas,
        tools.preview.clone(),
        cx.content(),
    ));
    let canvas = menu::over(canvas, tools, cx.live(), cx.dispatcher());
    let canvas: Element = Box::new(
        Stack::new()
            .push(canvas)
            .push(panels::header::context_bar(cx, tools))
            .push(panels::start::welcome(cx, tools, compact)),
    );
    if compact {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(0.0)
                .with_fill_remaining_height()
                .push(canvas)
                .push(rail(cx, tools, true)),
        )
    } else {
        Box::new(
            Row::new()
                .no_wrap()
                .with_gap(0.0)
                .with_fill_remaining_height()
                .push(rail(cx, tools, false))
                .push(canvas),
        )
    }
}

/// The tools, top to bottom, each with its key, and at the foot the paint
/// the brush is loaded with.
fn rail_choices(tools: &tools::Tools, workspace: tools::Workspace, compact: bool) -> Element {
    let choices = tools::Tool::ALL
        .iter()
        .filter(|tool| {
            tools.all_tools.get() || workspace.tools().contains(tool) || **tool == tools.tool.get()
        })
        .map(|tool| {
            let segment = ui::widget::toggle_group::Segment::new(
                ui::i18n::I18nManager::lookup_plain(tool.label()),
            )
            .with_icon(tool.icon())
            .with_hint(tool.shortcut().to_uppercase());
            (*tool, segment)
        })
        .collect();
    let rail = ui::widget::ToggleGroup::from_state(choices, &tools.tool).on_view("Tool", |_| {});
    if compact {
        return Box::new(
            Scrollable::new_horizontal()
                .fit_content_height()
                .with_child(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_padding_rhythm(
                            ui::tokens::Rhythm::Related,
                            ui::tokens::Rhythm::Related,
                        )
                        .push(panels::header::add_picture(tools, ControlSize::Large))
                        .push(Box::new(rail.control_size(ControlSize::Large)))
                        .push(Box::new(panels::well::Well::new(tools.clone()))),
                )),
        );
    }
    let rail = rail.vertical();
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(RAIL)
            .filling()
            .with_child(Box::new(
                Column::new()
                    .with_vertical_padding(8.0)
                    .with_horizontal_padding(6.0, 6.0)
                    .with_gap(0.0)
                    .push(Box::new(
                        Scrollable::new_vertical()
                            .fill_parent_height()
                            .with_scrollbar(false)
                            .with_child(Box::new(rail)),
                    ))
                    .push(Box::new(panels::well::Well::new(tools.clone()))),
            )),
    )
}

fn workspace_controls(tools: &tools::Tools) -> Element {
    use tools::Workspace;
    let choices = [
        (Workspace::Auto, "canvas-workspace-auto"),
        (Workspace::Vector, "canvas-workspace-vector"),
        (Workspace::Photo, "canvas-workspace-photo"),
        (Workspace::Pixel, "canvas-workspace-pixel"),
    ]
    .into_iter()
    .map(|(value, label)| {
        (
            value,
            ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(label)),
        )
    })
    .collect();
    let held = tools.clone();
    let choose = ui::widget::ToggleGroup::from_state(choices, &tools.workspace)
        .fill()
        .on_view("canvas-workspace", move |_| {
            let workspace = held.workspace.get();
            held.tool.set(if workspace == Workspace::Pixel {
                tools::Tool::Brush
            } else {
                tools::Tool::Select
            });
            held.choose_preset(if workspace == Workspace::Pixel {
                brushes::Preset::Pixel
            } else {
                brushes::Preset::Round
            });
            if workspace == Workspace::Photo {
                held.layer_adjust.set(true);
            }
        });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                Text::new("canvas-workspace").with_role(TextRole::Label),
            ))
            .push(Box::new(choose))
            .push(Box::new(
                ui::widget::ToggleGroup::from_state(
                    vec![
                        (
                            false,
                            ui::widget::toggle_group::Segment::new(
                                ui::i18n::I18nManager::lookup_plain("canvas-tools-focused"),
                            ),
                        ),
                        (
                            true,
                            ui::widget::toggle_group::Segment::new(
                                ui::i18n::I18nManager::lookup_plain("canvas-tools-all"),
                            ),
                        ),
                    ],
                    &tools.all_tools,
                )
                .on_view("canvas-tools-scope", |_| {}),
            )),
    )
}

/// The inspector above; under it the layers or the history.
fn sidebar(cx: &ViewCx<Artwork>, tools: &tools::Tools, dock: &LocalState<Dock>) -> Element {
    // A dense tool panel: one `s3` inset on every side.
    let inspector = Column::new()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .with_padding_rhythm(ui::tokens::Rhythm::Sibling, ui::tokens::Rhythm::Sibling)
        .push(workspace_controls(tools))
        .push(panels::inspector::panel(cx, tools));
    // The dock folds away to its tabs, giving the inspector the height.
    let open = LocalState::new(true);
    let docked = {
        let tools = tools.clone();
        let dock = dock.clone();
        cx.switch(&open, move |cx, open| {
            if !*open {
                return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
            }
            let tools = tools.clone();
            Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(cx.switch(&dock, move |cx, dock| match dock {
                        Dock::Layers => panels::layers::panel(cx, &tools),
                        Dock::Objects => panels::layers::objects_panel(cx, &tools),
                        Dock::History => panels::history::panel(cx, &tools),
                    })),
            ) as Element
        })
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .with_fill_remaining_height()
            .push(Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(Box::new(inspector)),
            ))
            .push(Box::new(ui::widget::Divider::horizontal()))
            .push(dock_tabs(cx, tools, dock, &open))
            .push(docked),
    )
}

/// The layers and history tabs, and beside the layers adding one.
fn dock_tabs(
    cx: &ViewCx<Artwork>,
    tools: &tools::Tools,
    dock: &LocalState<Dock>,
    open: &LocalState<bool>,
) -> Element {
    let tab = |which: Dock, id: &str| {
        (
            which,
            ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(id)),
        )
    };
    let tabs = ui::widget::ToggleGroup::from_state(
        vec![
            tab(Dock::Layers, "canvas-layers"),
            tab(Dock::Objects, "canvas-objects"),
            tab(Dock::History, "canvas-history"),
        ],
        dock,
    )
    .with_style(ui::widget::SegmentedStyle::Tabs)
    .fill()
    .with_id("canvas-dock");
    let tools = tools.clone();
    let action = cx.switch(dock, move |cx, dock| match dock {
        Dock::Layers => panels::layers::add_menu(cx, &tools),
        Dock::History => panels::history::checkpoint_button(cx),
        Dock::Objects => ui::widget::optional(None),
    });
    Box::new(
        Row::new()
            .no_wrap()
            .with_padding_rhythm(ui::tokens::Rhythm::Sibling, ui::tokens::Rhythm::Tight)
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(tabs)),
            ))
            .push(action)
            .push(fold(cx, open)),
    )
}

/// Folding the dock away to its tabs, or opening it again.
fn fold(cx: &ViewCx<Artwork>, open: &LocalState<bool>) -> Element {
    let flip = open.clone();
    cx.switch(open, move |_, shown| {
        let (icon, id) = if *shown {
            (ui::icons::Icon::ChevronDown, "canvas-dock-fold")
        } else {
            (ui::icons::Icon::ChevronUp, "canvas-dock-unfold")
        };
        let name = ui::i18n::I18nManager::lookup_plain(id);
        let flip = flip.clone();
        Box::new(
            Button::icon_only(id, icon, &name)
                .control_size(ControlSize::Small)
                .on_view(move || flip.set(!flip.with(|open| *open))),
        )
    })
}

ui::dev_reload_entry!(App);
