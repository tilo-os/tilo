//! The inspector: what the selected marks look like, or with nothing
//! selected, the brush and the artwork. Each value is edited through the
//! artwork's actions; the brush is the person's own and changes nothing
//! until they paint.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ColorSwatchPicker, Toggle};

use super::{caption, heading, icon_button, percent};
use crate::model::{Artwork, BLENDS, Mark, Shadow, Shape};
use crate::resource::ArtworkResource;
use crate::tools::Tools;

/// Inks for plain layers: document colours, the same in every theme.
const INKS: [u32; 14] = [
    0x1d_1d_1f, 0x5a_62_70, 0xff_ff_ff, 0xd6_40_3a, 0xf0_8a_24, 0xf6_c4_45, 0x5a_a0_46, 0x2f_8f_83,
    0x2b_6c_b0, 0x2b_3f_8f, 0x6b_4f_bf, 0xc2_41_7e, 0x8a_5a_3c, 0xe8_dc_c2,
]; // document colours

/// Grounds an artwork is commonly made on: papers, primed and toned
/// canvas.
const GROUNDS: [u32; 7] = [
    0xf4_ef_e3, 0xff_ff_ff, 0xec_e4_d0, 0xd9_cc_b0, 0xb8_a0_7e, 0x3a_3a_38, 0x1f_24_30,
]; // document colours

/// Showing a chosen swatch while building a picker.
trait Selecting {
    fn selecting(self, index: Option<usize>) -> Self;
}

impl Selecting for ColorSwatchPicker {
    fn selecting(mut self, index: Option<usize>) -> Self {
        self.set_selected(index);
        self
    }
}

/// A swatch cell's side.
const SWATCH: f64 = 28.0;

/// `#rrggbb` as `0xrrggbb`.
fn packed(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('#')?;
    u32::from_str_radix(digits.get(..6)?, 16).ok()
}

fn hex(rgb: u32) -> String {
    format!("#{rgb:06x}")
}

/// A pigment's name as words: `burnt-sienna` is "burnt sienna".
fn words(name: &str) -> String {
    name.replace('-', " ")
}

/// The inspector.
pub fn panel(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let tools = tools.clone();
    cx.switch(&tools.selection.clone(), move |cx, ids| {
        if ids.is_empty() {
            brush_and_artwork(cx, &tools)
        } else {
            selection(cx, ids.clone())
        }
    })
}

/// Groups of controls, kept apart so each reads as one thing.
fn spaced(groups: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(18.0);
    for group in groups {
        column = column.push(group);
    }
    Box::new(column)
}

/// A label and the control it names, close together.
fn tight(children: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0);
    for child in children {
        column = column.push(child);
    }
    Box::new(column)
}

fn section(children: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(8.0);
    for child in children {
        column = column.push(child);
    }
    Box::new(column)
}

/// A label on the left and its value on the right.
fn labelled(label: &str, value: Element) -> Element {
    Box::new(
        Row::new()
            .with_gap(8.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(super::filler())
            .push(value),
    )
}

/// The width of a field's label, so a form's controls line up.
const LABEL_WIDTH: f32 = 84.0;

/// A label on the left and a control filling the rest of the row.
fn field(label: &str, control: Element) -> Element {
    Box::new(
        Row::new()
            .with_gap(8.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(LABEL_WIDTH)
                    .with_child(Box::new(Text::new(label).with_role(TextRole::Label))),
            ))
            .push(grow(control)),
    )
}

/// A control taking the width a row leaves.
fn grow(control: Element) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_min_width(0.0)
            .with_child(control),
    )
}

fn brush_and_artwork(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let brush = {
        let tools = tools.clone();
        cx.switch(&tools.layer.clone(), move |cx, layer| {
            brush(cx, &tools, layer)
        })
    };
    section(vec![
        brush,
        Box::new(ui::widget::Divider::horizontal()),
        artwork(cx),
    ])
}

/// The brush on `layer`: the paints its medium offers, its size and load.
fn brush(cx: &ViewCx<ArtworkResource>, tools: &Tools, layer: &str) -> Element {
    let (medium, pigments, load) = cx.read(|art| {
        let Some(art) = art else {
            return (None, Vec::new(), None);
        };
        let medium = art
            .layer(layer)
            .filter(|layer| !layer.medium.is_empty())
            .and_then(|layer| crate::media::resolve(art, &layer.medium));
        let pigments = medium
            .as_ref()
            .map(|medium| medium.palette.clone())
            .unwrap_or_default();
        (medium, pigments, crate::tools::load_fraction(art, layer))
    });
    let title = medium.as_ref().map_or_else(
        || ui::i18n::I18nManager::lookup_plain("canvas-medium-plain-ink"),
        |medium| medium.label.clone(),
    );
    let mut parts: Vec<Element> = vec![Box::new(
        Row::new()
            .with_gap(8.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(heading("canvas-brush"))
            .push(super::filler())
            .push(caption(&title)),
    )];
    let palette = brush_palette(tools, &pigments);
    parts.push(Box::new(palette.with_cell(SWATCH).bare()));
    if medium.is_none() {
        parts.push(field(
            &ui::i18n::I18nManager::lookup_plain("canvas-colour"),
            Box::new(
                TextInput::from_state(&tools.colour)
                    .with_id("canvas-brush-colour")
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-colour")),
            ),
        ));
    }
    let size_label = {
        let tools = tools.clone();
        Text::dynamic(move || format!("{:.0}", tools.brush_size())).with_role(TextRole::Caption)
    };
    parts.push(tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-brush-size"),
            Box::new(size_label),
        ),
        Box::new(
            Slider::from_state(&tools.size)
                .with_id("canvas-brush-size")
                .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-brush-size")),
        ),
    ]));
    if let Some(load) = load {
        tools.load.set(load);
        let load_label = {
            let load = tools.load.clone();
            Text::dynamic(move || percent(load.with(|load| *load))).with_role(TextRole::Caption)
        };
        parts.push(tight(vec![
            labelled(
                &ui::i18n::I18nManager::lookup_plain("canvas-brush-load"),
                Box::new(load_label),
            ),
            Box::new(
                Slider::from_state(&tools.load)
                    .with_id("canvas-brush-load")
                    .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(
                        "canvas-brush-load",
                    )),
            ),
        ]));
    }
    section(parts)
}

/// The paints the brush can load: the medium's pigments, or inks on a
/// plain layer.
fn brush_palette(tools: &Tools, pigments: &[crate::media::PigmentSpec]) -> ColorSwatchPicker {
    if pigments.is_empty() {
        tools.pigment.set(None);
        let current = packed(&tools.hex());
        let chosen = tools.colour.clone();
        return ColorSwatchPicker::with_palette(INKS.to_vec(), 7)
            .with_accessible_labels(INKS.iter().map(|rgb| hex(*rgb)).collect())
            .on_change(move |index| {
                if let Some(rgb) = INKS.get(index) {
                    chosen.set(hex(*rgb));
                }
            })
            .selecting(INKS.iter().position(|rgb| Some(*rgb) == current));
    }
    // Keep the pigment when this palette has it too, else load the first.
    let loaded = tools
        .pigment
        .with(|pigment| pigment.as_ref().map(|(name, _)| name.clone()));
    let index = loaded
        .and_then(|name| pigments.iter().position(|pigment| pigment.name == name))
        .unwrap_or(0);
    if let Some(pigment) = pigments.get(index) {
        tools
            .pigment
            .set(Some((pigment.name.clone(), pigment.colour.clone())));
    }
    let loads: Vec<(String, String)> = pigments
        .iter()
        .map(|pigment| (pigment.name.clone(), pigment.colour.clone()))
        .collect();
    let colours: Vec<u32> = pigments
        .iter()
        .map(|pigment| packed(&pigment.colour).unwrap_or_default())
        .collect();
    let labels = pigments
        .iter()
        .map(|pigment| words(&pigment.name))
        .collect();
    let pigment = tools.pigment.clone();
    ColorSwatchPicker::with_palette(colours, 8)
        .with_accessible_labels(labels)
        .on_change(move |index| {
            if let Some(load) = loads.get(index) {
                pigment.set(Some(load.clone()));
            }
        })
        .selecting(Some(index))
}

/// The artwork's title, size and ground.
fn artwork(cx: &ViewCx<ArtworkResource>) -> Element {
    let title = cx
        .input(
            |art: &Artwork| art.title.clone(),
            ArtworkResource::SET_TITLE,
        )
        .with_placeholder(&ui::i18n::I18nManager::lookup_plain("canvas-untitled"));
    let side = |read: fn(&Artwork) -> f64, action, name: &str| {
        cx.control(
            |state| {
                NumberInput::from_state(state)
                    .with_range(16.0, crate::model::MAX_SIDE)
                    .with_step(10.0)
                    .with_precision(0)
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(name))
            },
            read,
            action,
        )
    };
    let width = side(
        |art| art.width,
        ArtworkResource::SET_WIDTH,
        "canvas-artwork-width",
    );
    let height = side(
        |art| art.height,
        ArtworkResource::SET_HEIGHT,
        "canvas-artwork-height",
    );
    let textures: Vec<(String, String)> = tilo_wet_media::Texture::ALL
        .iter()
        .map(|texture| {
            let name = texture.name().to_owned();
            let label = ui::i18n::I18nManager::lookup_plain(&format!("canvas-ground-{name}"));
            (name, label)
        })
        .collect();
    let texture = cx.control(
        |state| {
            let mut dropdown = Dropdown::from_state(Vec::new(), state)
                .with_id("canvas-ground-texture")
                .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-ground"));
            dropdown.set_value_options(textures);
            dropdown
        },
        |art: &Artwork| art.ground.texture.clone(),
        ArtworkResource::SET_TEXTURE,
    );
    let dispatch = cx.dispatcher();
    let ground_now = cx.read(|art| art.and_then(|art| packed(&art.ground.colour)));
    let grounds = ColorSwatchPicker::with_palette(GROUNDS.to_vec(), 7)
        .with_accessible_labels(GROUNDS.iter().map(|rgb| hex(*rgb)).collect())
        .with_cell(SWATCH)
        .bare()
        .selecting(GROUNDS.iter().position(|rgb| Some(*rgb) == ground_now))
        .on_change(move |index| {
            if let Some(rgb) = GROUNDS.get(index) {
                dispatch.invoke(ArtworkResource::SET_GROUND.with(hex(*rgb)));
            }
        });
    section(vec![
        heading("canvas-artwork"),
        Box::new(title),
        field(
            &ui::i18n::I18nManager::lookup_plain("canvas-artwork-width"),
            Box::new(width),
        ),
        field(
            &ui::i18n::I18nManager::lookup_plain("canvas-artwork-height"),
            Box::new(height),
        ),
        field(
            &ui::i18n::I18nManager::lookup_plain("canvas-ground"),
            Box::new(texture),
        ),
        Box::new(grounds),
    ])
}

/// The selected marks, read again whenever one of them changes so every
/// control starts from what is there now.
fn selection(cx: &ViewCx<ArtworkResource>, ids: Vec<String>) -> Element {
    let marks = cx.value(move |art: &Artwork| {
        ids.iter()
            .filter_map(|id| art.mark(id).cloned())
            .collect::<Vec<Mark>>()
    });
    cx.switch(&marks, |cx, marks| marks_inspector(cx, marks))
}

fn marks_inspector(cx: &ViewCx<ArtworkResource>, marks: &[Mark]) -> Element {
    let Some(first) = marks.first() else {
        return Box::new(Spacer::fixed(0.0));
    };
    let ids: Vec<String> = marks.iter().map(|mark| mark.id.clone()).collect();
    let style = &first.style;
    let summary = if let [mark] = marks {
        let kind =
            ui::i18n::I18nManager::lookup_plain(&format!("canvas-kind-{}", mark.shape.kind()));
        if mark.name.is_empty() {
            kind
        } else {
            format!("{kind} · {}", mark.name)
        }
    } else {
        ui::i18n::I18nManager::format("canvas-marks-selected", &[("count", marks.len().into())])
    };
    let palette = cx.read(|art| {
        art.map(|art| crate::media::palette_for(art, &first.layer))
            .unwrap_or_default()
    });
    let mut parts: Vec<Element> = vec![Box::new(
        Row::new()
            .with_gap(8.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(heading("canvas-selection"))
            .push(super::filler())
            .push(caption(&summary)),
    )];
    let lines_only = matches!(first.shape, Shape::Line { .. } | Shape::Stroke { .. });
    if !lines_only {
        let mut fill = vec![heading("canvas-fill")];
        if let Some(gradient) = &style.gradient {
            fill.push(caption(&ui::i18n::I18nManager::format(
                "canvas-gradient",
                &[("stops", gradient.stops.len().into())],
            )));
        }
        fill.push(paints(
            cx,
            &ids,
            style.fill.as_deref(),
            &palette,
            Paint::Fill,
        ));
        parts.push(section(fill));
    }
    parts.push(section(vec![
        heading("canvas-stroke"),
        paints(cx, &ids, style.stroke.as_deref(), &palette, Paint::Stroke),
        stroke_style(cx, &ids, first),
    ]));
    parts.push(appearance(cx, &ids, first));
    if let (Shape::Text { .. }, [mark]) = (&first.shape, marks) {
        parts.push(text_style(cx, mark));
    }
    parts.push(arrange(cx, &ids));
    spaced(parts)
}

#[derive(Clone, Copy)]
enum Paint {
    Fill,
    Stroke,
}

/// Swatches of the layer's paints (or inks) and the colour as text.
fn paints(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    current: Option<&str>,
    palette: &[tilo_wet_media::Pigment],
    paint: Paint,
) -> Element {
    let action = match paint {
        Paint::Fill => ArtworkResource::SET_FILL,
        Paint::Stroke => ArtworkResource::SET_STROKE,
    };
    // A painted layer's pigments by name, else inks by colour.
    let (values, colours, names): (Vec<String>, Vec<u32>, Vec<String>) =
        if palette.len() <= 15 && !palette.is_empty() {
            let values = palette
                .iter()
                .map(|pigment| format!("pigment:{}", pigment.name))
                .collect();
            let colours = palette
                .iter()
                .map(|pigment| packed(&pigment.colour).unwrap_or_default())
                .collect();
            let names = palette.iter().map(|pigment| words(&pigment.name)).collect();
            (values, colours, names)
        } else {
            (
                INKS.iter().map(|rgb| hex(*rgb)).collect(),
                INKS.to_vec(),
                INKS.iter().map(|rgb| hex(*rgb)).collect(),
            )
        };
    let selected = current.and_then(|current| values.iter().position(|value| value == current));
    let dispatch = cx.dispatcher();
    let targets = ids.to_vec();
    let chosen = values;
    let columns = if colours.len() > 14 { 8 } else { 7 };
    let swatches = ColorSwatchPicker::with_palette(colours, columns)
        .with_accessible_labels(names)
        .with_cell(SWATCH)
        .bare()
        .selecting(selected)
        .on_change(move |index| {
            if let Some(value) = chosen.get(index) {
                dispatch.invoke(action.with((targets.clone(), value.clone())));
            }
        });
    let text = LocalState::new(current.unwrap_or("none").to_owned());
    let field = cx.actions().bind(
        TextInput::from_state(&text)
            .with_id(match paint {
                Paint::Fill => "canvas-fill",
                Paint::Stroke => "canvas-stroke",
            })
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain(match paint {
                Paint::Fill => "canvas-fill",
                Paint::Stroke => "canvas-stroke",
            })),
        action.map({
            let ids = ids.to_vec();
            move |value: String| (ids.clone(), value)
        }),
    );
    section(vec![Box::new(swatches), Box::new(field)])
}

/// The outline's width, dashes, ends and corners.
fn stroke_style(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark) -> Element {
    let style = &mark.style;
    let width = LocalState::new(style.stroke_width);
    let width = cx.actions().bind(
        NumberInput::from_state(&width)
            .with_range(0.0, 400.0)
            .with_size(112.0, 32.0)
            .with_step(1.0)
            .with_precision(1)
            .with_id("canvas-stroke-width")
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-stroke-width")),
        ArtworkResource::SET_STROKE_WIDTH.map({
            let ids = ids.to_vec();
            move |value: f64| (ids.clone(), value)
        }),
    );
    let dash_now = match style.dash.as_slice() {
        [] => "solid",
        [on, ..] if *on <= 2.0 => "dotted",
        _ => "dashed",
    };
    let dash = choice(
        cx,
        "canvas-dash",
        &["solid", "dashed", "dotted"],
        dash_now,
        ArtworkResource::SET_DASH.map({
            let ids = ids.to_vec();
            move |value: String| {
                let pattern = match value.as_str() {
                    "dashed" => vec![12.0, 8.0],
                    "dotted" => vec![1.0, 6.0],
                    _ => Vec::new(),
                };
                (ids.clone(), pattern)
            }
        }),
    );
    let cap = choice(
        cx,
        "canvas-cap",
        &["round", "butt", "square"],
        style.cap.as_deref().unwrap_or("round"),
        ArtworkResource::SET_CAP.map({
            let ids = ids.to_vec();
            move |value: String| (ids.clone(), value)
        }),
    );
    let join = choice(
        cx,
        "canvas-join",
        &["round", "miter", "bevel"],
        style.join.as_deref().unwrap_or("round"),
        ArtworkResource::SET_JOIN.map({
            let ids = ids.to_vec();
            move |value: String| (ids.clone(), value)
        }),
    );
    section(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-stroke-width"),
            Box::new(width),
        ),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-dash"),
            grow(dash),
        ),
        Box::new(
            Row::new()
                .with_gap(6.0)
                .no_wrap()
                .push(grow(cap))
                .push(grow(join)),
        ),
    ])
}

/// A dropdown of `values` (named by `canvas-<value>`) bound to `action`.
fn choice(
    cx: &ViewCx<ArtworkResource>,
    id: &str,
    values: &[&str],
    current: &str,
    action: impl ui::tilo_resource::Binds<ArtworkResource, String>,
) -> Element {
    let state = LocalState::new(current.to_owned());
    let mut dropdown = Dropdown::from_state(Vec::new(), &state)
        .with_id(id)
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain(id));
    dropdown.set_value_options(
        values
            .iter()
            .map(|value| {
                (
                    (*value).to_owned(),
                    ui::i18n::I18nManager::lookup_plain(&format!("canvas-value-{value}")),
                )
            })
            .collect(),
    );
    Box::new(cx.actions().bind(dropdown, action))
}

/// Opacity, blend and shadow.
fn appearance(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark) -> Element {
    let style = &mark.style;
    let opacity = LocalState::new(style.opacity());
    let label = {
        let opacity = opacity.clone();
        Text::dynamic(move || percent(opacity.with(|value| *value))).with_role(TextRole::Caption)
    };
    let slider = cx.actions().bind(
        Slider::from_state(&opacity)
            .with_id("canvas-opacity")
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-opacity"))
            .commits_on_release(),
        ArtworkResource::SET_OPACITY.map({
            let ids = ids.to_vec();
            move |value: f64| (ids.clone(), (value * 100.0).round() / 100.0)
        }),
    );
    let blend = choice(
        cx,
        "canvas-blend",
        &BLENDS,
        style.blend.as_deref().unwrap_or("normal"),
        ArtworkResource::SET_BLEND.map({
            let ids = ids.to_vec();
            move |value: String| (ids.clone(), value)
        }),
    );
    let shadowed = LocalState::new(
        style
            .shadow
            .as_ref()
            .is_some_and(|shadow| shadow.blur > 0.0 || shadow.offset != [0.0, 0.0]),
    );
    let shadow = cx.actions().bind(
        Toggle::from_state(&shadowed)
            .with_id("canvas-shadow")
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-shadow")),
        ArtworkResource::SET_SHADOW.map({
            let ids = ids.to_vec();
            move |on: bool| {
                let shadow = if on {
                    Shadow {
                        offset: [6.0, 8.0],
                        blur: 14.0,
                        colour: None,
                    }
                } else {
                    Shadow {
                        offset: [0.0, 0.0],
                        blur: 0.0,
                        colour: None,
                    }
                };
                (ids.clone(), shadow)
            }
        }),
    );
    section(vec![
        tight(vec![
            labelled(
                &ui::i18n::I18nManager::lookup_plain("canvas-opacity"),
                Box::new(label),
            ),
            Box::new(slider),
        ]),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-blend"),
            grow(blend),
        ),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-shadow"),
            Box::new(shadow),
        ),
    ])
}

/// A text mark's family, size, weight and slant.
fn text_style(cx: &ViewCx<ArtworkResource>, mark: &Mark) -> Element {
    let Shape::Text {
        size,
        font,
        weight,
        italic,
        ..
    } = &mark.shape
    else {
        return Box::new(Spacer::fixed(0.0));
    };
    let ids = vec![mark.id.clone()];
    let restyled = |change: fn(&mut Shape, &str)| {
        let shape = mark.shape.clone();
        let ids = ids.clone();
        ArtworkResource::SET_SHAPE.map(move |value: String| {
            let mut shape = shape.clone();
            change(&mut shape, &value);
            (ids.clone(), shape)
        })
    };
    let family = choice(
        cx,
        "canvas-font",
        &["sans", "serif", "mono"],
        font.as_deref().unwrap_or("sans"),
        restyled(|shape, value| {
            if let Shape::Text { font, .. } = shape {
                *font = Some(value.to_owned());
            }
        }),
    );
    let size_state = LocalState::new(*size);
    let size_field = {
        let shape = mark.shape.clone();
        let ids = ids.clone();
        cx.actions().bind(
            NumberInput::from_state(&size_state)
                .with_range(4.0, 1000.0)
                .with_size(96.0, 32.0)
                .with_step(2.0)
                .with_precision(0)
                .with_id("canvas-text-size")
                .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-text-size")),
            ArtworkResource::SET_SHAPE.map(move |value: f64| {
                let mut shape = shape.clone();
                if let Shape::Text { size, .. } = &mut shape {
                    *size = value;
                }
                (ids.clone(), shape)
            }),
        )
    };
    let flag = |id: &str, on: bool, change: fn(&mut Shape, bool)| {
        let shape = mark.shape.clone();
        let ids = ids.clone();
        let state = LocalState::new(on);
        cx.actions().bind(
            Toggle::from_state(&state)
                .with_id(id)
                .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(id)),
            ArtworkResource::SET_SHAPE.map(move |value: bool| {
                let mut shape = shape.clone();
                change(&mut shape, value);
                (ids.clone(), shape)
            }),
        )
    };
    let bold = flag(
        "canvas-bold",
        weight.is_some_and(|weight| weight >= 600),
        |shape, on| {
            if let Shape::Text { weight, .. } = shape {
                *weight = on.then_some(700);
            }
        },
    );
    let slanted = flag("canvas-italic", *italic, |shape, on| {
        if let Shape::Text { italic, .. } = shape {
            *italic = on;
        }
    });
    section(vec![
        heading("canvas-text"),
        Box::new(
            Row::new()
                .with_gap(6.0)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(grow(family))
                .push(Box::new(size_field)),
        ),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-bold"),
            Box::new(bold),
        ),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-italic"),
            Box::new(slanted),
        ),
    ])
}

/// Bring to the front or send to the back, and remove.
fn arrange(cx: &ViewCx<ArtworkResource>, ids: &[String]) -> Element {
    let to = |icon, name: &str, to: &str| {
        Box::new(icon_button(
            cx,
            icon,
            &ui::i18n::I18nManager::lookup_plain(name),
            ArtworkResource::ARRANGE.with((ids.to_vec(), to.to_owned())),
        )) as Element
    };
    Box::new(
        Row::new()
            .with_gap(4.0)
            .no_wrap()
            .push(to(Icon::ArrowUp, "canvas-forward", "forward"))
            .push(to(Icon::ArrowDown, "canvas-backward", "backward"))
            .push(to(Icon::ChevronUp, "canvas-front", "front"))
            .push(to(Icon::ChevronDown, "canvas-back", "back"))
            .push(super::filler())
            .push(Box::new(icon_button(
                cx,
                Icon::Trash2,
                &ui::i18n::I18nManager::lookup_plain("canvas-remove-marks"),
                ArtworkResource::REMOVE_MARKS.with(ids.to_vec()),
            ))),
    )
}
