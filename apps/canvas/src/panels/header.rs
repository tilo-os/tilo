//! The strip over the canvas: the artwork's title, set like the label
//! beside a picture on a wall, taking back and bringing back steps, how
//! near the artwork is, and the sidebar shown or put away.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::text_render::FontChoice;
use ui::widget::EditableLabel;

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
    // The title reads as a title until it is clicked; the edit lands as
    // one step when Enter or a click away ends it, not one per letter.
    let titled = cx.value(|art: &Artwork| art.title.clone());
    let title = cx.switch(&titled, |cx, title| {
        Box::new(
            cx.actions().bind(
                EditableLabel::new(title)
                    .edits_on_click()
                    .with_placeholder(ui::i18n::I18nManager::lookup_plain("canvas-untitled"))
                    .with_font_family(FontChoice::Serif)
                    .with_font_size(17.0)
                    .with_id("canvas-title")
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "canvas-artwork-title",
                    ))
                    .filling(),
                ArtworkResource::SET_TITLE,
            ),
        ) as Element
    });
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
                    .with_child(title),
            ))
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

/// The strip over the canvas while there is something to do with what is
/// selected: an area to fill, outline, mask the chosen layer to, clear,
/// invert or drop; shapes to combine into one. Nothing otherwise.
pub fn context_bar(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let selected = cx.value(|art: &Artwork| art.selection.is_some());
    let tools = tools.clone();
    cx.switch(&selected, move |cx, selected| {
        let area = *selected;
        let tools = tools.clone();
        cx.switch(&tools.selection.clone(), move |cx, picked| {
            let combine = picked.len() >= 2;
            if !area && !combine {
                return Box::new(Spacer::fixed(0.0)) as Element;
            }
            // Wrapping, so a narrow window keeps its panels in view.
            let mut row = Row::new()
                .with_padding(10.0, 4.0)
                .with_gap(2.0)
                .with_align_items(ui::taffy::AlignItems::Center);
            if area {
                row = row.push(area_buttons(cx, &tools));
            }
            if combine {
                row = row.push(combine_buttons(cx, &tools));
            }
            Box::new(super::pane::Pane::new(
                Box::new(row),
                super::pane::Edge::Bottom,
            ))
        })
    })
}

/// A text button invoking `action` with what it reads when pressed.
fn text_button(
    cx: &ViewCx<ArtworkResource>,
    id: &str,
    action: impl ui::tilo_resource::Binds<ArtworkResource, ()>,
) -> Element {
    let label = ui::i18n::I18nManager::lookup_plain(id);
    Box::new(cx.actions().bind(
        Button::new(id, &label, None).ghost().fit_content(28.0),
        action,
    ))
}

/// Fill, outline, mask, clear, invert, select nothing.
fn area_buttons(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let using = |what: &'static str| {
        let loaded = tools.loaded();
        text_button(
            cx,
            &format!("canvas-area-{what}"),
            ArtworkResource::USE_SELECTION
                .map(move |()| (what.to_owned(), loaded.layer(), Some(loaded.paint()))),
        )
    };
    let lookup = ui::i18n::I18nManager::lookup_plain;
    Box::new(
        Row::new()
            .with_gap(2.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Text::new(&lookup("canvas-area")).with_role(TextRole::Label),
            ))
            .push(using("fill"))
            .push(using("stroke"))
            .push(using("mask"))
            .push(using("clear"))
            .push(Box::new(
                cx.button(
                    &lookup("canvas-area-invert"),
                    ArtworkResource::SELECT_INVERT.with(true),
                )
                .ghost()
                .fit_content(28.0),
            ))
            .push(Box::new(
                cx.button(
                    &lookup("canvas-area-none"),
                    ArtworkResource::SELECT_MODE.with("none".into()),
                )
                .ghost()
                .fit_content(28.0),
            ))
            .push(Box::new(Spacer::fixed(12.0))),
    )
}

/// Unite, subtract, intersect or exclude the picked shapes.
fn combine_buttons(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut row = Row::new()
        .with_gap(2.0)
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(
            Text::new(&lookup("canvas-combine")).with_role(TextRole::Label),
        ));
    for op in ["union", "subtract", "intersect", "exclude"] {
        let picked = tools.selection.clone();
        row = row.push(text_button(
            cx,
            &format!("canvas-combine-{op}"),
            ArtworkResource::COMBINE.map(move |()| (picked.with(Clone::clone), op.to_owned())),
        ));
    }
    Box::new(row)
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
