//! The strip over the canvas: the artwork's title, set like the label
//! beside a picture on a wall, taking back and bringing back steps, how
//! near the artwork is, and the sidebar shown or put away.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::text_render::FontChoice;

use super::icon_button;
use crate::model::Artwork;
use crate::resource::ArtworkResource;
use crate::tools::{Tools, ViewRequest};

/// The least room the title keeps, so it reads as a title, not a field.
const TITLE: f32 = 96.0; // about ten letters of the title face: a text width, not spacing
/// The zoom readout's width, so the buttons beside it hold still as it
/// changes.
const READOUT: f32 = 52.0; // "6400%" in the caption face: a text width, not spacing

/// The header.
pub fn header(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let title = cx
        .input(
            |art: &Artwork| art.title.clone(),
            ArtworkResource::SET_TITLE,
        )
        .with_plain_chrome()
        .with_font_family(FontChoice::Serif)
        .with_font_size(17.0)
        .with_placeholder(&ui::i18n::I18nManager::lookup_plain("canvas-untitled"))
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-artwork-title"));
    let lookup = ui::i18n::I18nManager::lookup_plain;
    Box::new(
        Row::new()
            .no_wrap()
            .with_padding(10.0, 6.0)
            .with_gap(2.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            // The title takes the room the controls leave, and holds them
            // at the far end.
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(TITLE)
                    .with_child(Box::new(title)),
            ))
            .push(add_picture(tools))
            .push(export(tools))
            .push(Box::new(Spacer::fixed(6.0)))
            .push(Box::new(icon_button(
                cx,
                Icon::Undo2,
                &lookup("canvas-undo"),
                ArtworkResource::UNDO.invoke(),
            )))
            .push(Box::new(icon_button(
                cx,
                Icon::Redo2,
                &lookup("canvas-redo"),
                ArtworkResource::REDO.invoke(),
            )))
            .push(Box::new(Spacer::fixed(10.0)))
            .push(zoom(tools))
            .push(Box::new(Spacer::fixed(10.0)))
            .push(Box::new(toggle(
                &tools.sidebar,
                "canvas-sidebar",
                Icon::PanelRight,
            ))),
    )
}

/// Choose a picture to add: on an empty sheet it becomes the artwork, the
/// sheet taking its size; otherwise it is placed on the artwork. The
/// canvas takes the answer when the chooser gives it.
fn add_picture(tools: &Tools) -> Element {
    let picking = tools.picking.clone();
    let name = ui::i18n::I18nManager::lookup_plain("canvas-add-picture");
    let title = ui::i18n::I18nManager::lookup_plain("canvas-add-picture-title");
    let confirm = ui::i18n::I18nManager::lookup_plain("canvas-add-picture-confirm");
    let kinds = ui::i18n::I18nManager::lookup_plain("canvas-pictures");
    Box::new(
        Button::new("canvas-add-picture", "", None)
            .with_icon(Icon::Image)
            .ghost()
            .with_accessible_name(&name)
            .tooltip(name.clone())
            .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
            .on_view(move || {
                let request = ui::portal::PickRequest::open(&title, &confirm)
                    .filter(
                        ui::portal::FileFilter::new(&kinds)
                            .mimetype("image/png")
                            .mimetype("image/jpeg")
                            .mimetype("image/webp"),
                    )
                    .send_contents();
                *picking.borrow_mut() = Some(request);
            }),
    )
}

/// Export the artwork as a picture, saved where the person chooses: the
/// same rendering as the `export` action.
fn export(tools: &Tools) -> Element {
    let mut menu = Dropdown::from_state(Vec::new(), &tools.export)
        .as_menu(Icon::Download)
        .with_id("canvas-export")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-export"));
    menu.set_value_options(
        ["png", "jpeg", "webp"]
            .into_iter()
            .map(|format| {
                (
                    format.to_owned(),
                    ui::i18n::I18nManager::lookup_plain(&format!("canvas-export-{format}")),
                )
            })
            .collect(),
    );
    Box::new(menu)
}

/// Zoom out, how near the artwork is now, zoom in, and fit it to the
/// window.
fn zoom(tools: &Tools) -> Element {
    let readout = {
        let zoom = tools.zoom.clone();
        Text::dynamic(move || {
            let percent = zoom.with(|zoom| zoom * 100.0);
            if percent < 10.0 {
                format!("{percent:.1}%")
            } else {
                format!("{percent:.0}%")
            }
        })
        .with_role(TextRole::Caption)
        .with_font_feature(ui::text_render::OpenTypeTag(*b"tnum"), 1)
    };
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(0.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(view_button(
                tools,
                "canvas-zoom-out",
                Icon::Minus,
                ViewRequest::ZoomOut,
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(READOUT)
                    .with_child(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_justify(ui::taffy::JustifyContent::Center)
                            .push(Box::new(readout)),
                    )),
            ))
            .push(view_button(
                tools,
                "canvas-zoom-in",
                Icon::Plus,
                ViewRequest::ZoomIn,
            ))
            .push(view_button(
                tools,
                "canvas-zoom-fit",
                Icon::Maximize2,
                ViewRequest::Fit,
            )),
    )
}

/// A button asking the canvas for a change of view: a view action, so an
/// agent zooms the way a person does.
fn view_button(tools: &Tools, id: &str, icon: Icon, request: ViewRequest) -> Element {
    let view = tools.view.clone();
    let name = ui::i18n::I18nManager::lookup_plain(id);
    Box::new(
        Button::new(id, "", None)
            .with_icon(icon)
            .ghost()
            .with_accessible_name(&name)
            .tooltip(name.clone())
            .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
            .on_view(move || view.set(Some(request))),
    )
}

/// An icon button that shows or hides what `state` says, as a view action.
fn toggle(state: &LocalState<bool>, id: &str, icon: Icon) -> Button {
    let flip = state.clone();
    let name = ui::i18n::I18nManager::lookup_plain(id);
    Button::new(id, "", None)
        .with_icon(icon)
        .ghost()
        .with_accessible_name(&name)
        .tooltip(name.clone())
        .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
        .on_view(move || flip.set(!flip.with(|on| *on)))
}
