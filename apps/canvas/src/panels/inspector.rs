//! The inspector: what the selected marks look like, or with nothing
//! selected, the brush and the artwork. Each value is edited through the
//! artwork's actions; the brush is the person's own and changes nothing
//! until they paint.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::ControlSize;
use ui::widget::{ColorSwatch, ColorSwatchPicker, Toggle};

use super::{caption, heading, icon_button, percent};
use crate::brushes::Preset;
use crate::model::{Artwork, BLENDS, Mark, Shadow, Shape};
use crate::resource::ArtworkResource;
use crate::tools::{Erasing, Sampling, Tool, Tools};

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

/// A swatch cell's side: a small control's, so eight fit across the
/// inspector.
const SWATCH: f32 = ui::tokens::Metrics::BASE.bar_control_height;

/// `#rrggbb` as `0xrrggbb`.
fn packed(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('#')?;
    u32::from_str_radix(digits.get(..6)?, 16).ok()
}

fn hex(rgb: u32) -> String {
    format!("#{rgb:06x}")
}

/// A picker of the document colours `colours`, each named by `names`,
/// `columns` to a row.
pub(crate) fn swatches(colours: &[u32], names: Vec<String>, columns: usize) -> ColorSwatchPicker {
    let swatches = colours.iter().zip(names).map(|(rgb, name)| {
        let [_, r, g, b] = rgb.to_be_bytes();
        ColorSwatch::named(ui::vello::peniko::Color::from_rgb8(r, g, b), name)
    });
    ColorSwatchPicker::new(swatches, columns)
}

/// Reuse document paint through the shared picker; changing counts while
/// painting leaves this subtree in place.
fn artwork_palette(
    cx: &ViewCx<ArtworkResource>,
    id: String,
    current: impl Fn() -> String + 'static,
    choose: impl Fn(String) + Clone + 'static,
) -> Element {
    let colours = cx.value(crate::palette::swatches);
    cx.switch(&colours, move |_, colours| {
        let authored = !colours.is_empty();
        let colours = if authored {
            colours.clone()
        } else {
            INKS.iter().map(|rgb| hex(*rgb)).collect()
        };
        let loaded = current().to_lowercase();
        let selected = colours.iter().position(|colour| colour == &loaded);
        let values = colours.clone();
        let choose = choose.clone();
        let picker = ColorSwatchPicker::new(
            colours.iter().filter_map(|colour| {
                crate::render::colour(colour, &[])
                    .map(|paint| ColorSwatch::named(paint, colour.clone()))
            }),
            8,
        )
        .with_cell(f64::from(SWATCH))
        .selecting(selected)
        .on_change(move |index| {
            if let Some(colour) = values.get(index) {
                choose(colour.clone());
            }
        });
        let mut parts: Vec<Element> = Vec::new();
        if authored {
            parts.push(heading("canvas-artwork-colours"));
        }
        parts.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id(id.clone())
                .with_child(Box::new(picker)),
        ));
        section(parts)
    })
}

/// A pigment's name as words: `burnt-sienna` is "burnt sienna".
fn words(name: &str) -> String {
    name.replace('-', " ")
}

/// The inspector.
pub fn panel(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let tools = tools.clone();
    cx.switch(&tools.tool.clone(), move |cx, tool| {
        if matches!(tool, Tool::Marquee | Tool::Oval | Tool::Lasso | Tool::Wand) {
            return brush_and_artwork(cx, &tools);
        }
        let tools = tools.clone();
        cx.switch(&tools.selection.clone(), move |cx, ids| {
            if ids.is_empty() {
                brush_and_artwork(cx, &tools)
            } else {
                selection(cx, ids.clone(), &tools)
            }
        })
    })
}

/// Groups of controls, kept apart so each reads as one thing.
pub(super) fn spaced(groups: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(ui::tokens::Spacing::BASE.s4);
    for group in groups {
        column = column.push(group);
    }
    Box::new(column)
}

/// A label and the control it names, close together.
pub(super) fn tight(children: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0);
    for child in children {
        column = column.push(child);
    }
    Box::new(column)
}

pub(super) fn section(children: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(ui::tokens::Spacing::BASE.s2);
    for child in children {
        column = column.push(child);
    }
    Box::new(column)
}

/// A label on the left and its value on the right.
pub(super) fn labelled(label: &str, value: Element) -> Element {
    Box::new(
        Row::new()
            .with_gap(ui::tokens::Spacing::BASE.s2)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(Box::new(Spacer::new()))
            .push(value),
    )
}

/// A label over a control spanning the panel, as a form's field stacks
/// in a narrow column.
fn field(label: &str, control: Element) -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(ui::tokens::Spacing::BASE.s1)
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(grow(control)),
    )
}

/// A control taking the width a row leaves.
pub(super) fn grow(control: Element) -> Element {
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
        cx.switch(&tools.tool.clone(), move |cx, tool| {
            let tool = *tool;
            let tools = tools.clone();
            cx.switch(&tools.layer.clone(), move |cx, layer| {
                let settings = match tool {
                    Tool::Eraser => eraser(cx, &tools),
                    // Picking lays no paint: nothing to set.
                    Tool::Select => {
                        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
                    }
                    Tool::Marquee | Tool::Oval | Tool::Lasso | Tool::Wand => {
                        area_tool(cx, &tools, tool)
                    }
                    // Where the fill comes from first: it decides what
                    // the bucket fills.
                    Tool::Bucket => section(vec![
                        sampling(&tools.fill_sampling, "canvas-fill-sample"),
                        brush(cx, &tools, layer, tool),
                        tolerance(&tools),
                    ]),
                    _ => brush(cx, &tools, layer, tool),
                };
                section(vec![settings, Box::new(ui::widget::Divider::horizontal())])
            })
        })
    };
    let layer = {
        let tools = tools.clone();
        cx.switch(&tools.layer.clone(), move |cx, layer| {
            layer_adjust(cx, &tools, layer)
        })
    };
    section(vec![
        brush,
        artwork(cx),
        Box::new(ui::widget::Divider::horizontal()),
        layer,
    ])
}

/// The chosen layer's colour and tone, folded away until asked for.
fn layer_adjust(cx: &ViewCx<ArtworkResource>, tools: &Tools, layer: &str) -> Element {
    let chosen = layer.to_owned();
    let state = cx.value(move |art: &Artwork| {
        let layer = art
            .layer(&chosen)
            .or_else(|| art.layers.last())
            .map(|layer| layer.id.clone());
        layer.unwrap_or_default()
    });
    let tools = tools.clone();
    // Built again when another layer is chosen, not at each edit: the
    // controls follow the layer's adjustments in place.
    cx.switch(&state, move |cx, id| {
        let open = tools.layer_adjust.clone();
        let tools = tools.clone();
        let id = id.clone();
        cx.switch(&open.clone(), move |cx, shown| {
            tools.live.borrow_mut().clear();
            let flip = open.clone();
            let (icon, name) = if *shown {
                (Icon::ChevronUp, "canvas-layer-adjust-hide")
            } else {
                (Icon::ChevronDown, "canvas-layer-adjust-show")
            };
            let name = ui::i18n::I18nManager::lookup_plain(name);
            let header = Row::new()
                .with_gap(ui::tokens::Spacing::BASE.s2)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(heading("canvas-layer-adjust"))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Button::icon_only("canvas-layer-adjust-fold", icon, &name)
                        .control_size(ControlSize::Small)
                        .on_view(move || flip.set(!flip.with(|on| *on))),
                ));
            let mut parts: Vec<Element> = vec![Box::new(header)];
            if *shown {
                parts.push(super::picture::adjustments(
                    cx,
                    &super::picture::Target::Layer(id.clone()),
                    &tools,
                ));
            }
            section(parts)
        })
    })
}

/// The brush on `layer`: the paints its medium offers, its size and load.
///
/// For the other tools that lay paint, only what they use of it: the
/// colour, and for lines and open paths the width.
fn brush(cx: &ViewCx<ArtworkResource>, tools: &Tools, layer: &str, tool: Tool) -> Element {
    let brushing = tool == Tool::Brush;
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
    let mut parts: Vec<Element> = vec![tool_heading(tools, medium.as_ref(), tool)];
    if brushing && medium.is_none() {
        parts.push(preset_picker(tools));
    }
    let palette = brush_palette(tools, &pigments);
    if medium.is_some() {
        parts.push(Box::new(palette.with_cell(f64::from(SWATCH))));
    } else {
        let chosen = tools.colour.clone();
        let loaded = tools.clone();
        parts.push(artwork_palette(
            cx,
            "canvas-artwork-colours".into(),
            move || loaded.hex(),
            move |value| chosen.set(value),
        ));
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
    if !matches!(tool, Tool::Brush | Tool::Line | Tool::Pen) {
        return section(parts);
    }
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
    if !brushing {
        return section(parts);
    }
    if medium.is_none() {
        parts.push(fraction(
            "canvas-brush-opacity",
            &tools.opacity,
            "canvas-brush-opacity",
        ));
        let dynamics = tools.clone();
        parts.push(cx.switch(&tools.preset.clone(), move |_, preset| {
            dynamics_sliders(&dynamics, *preset)
        }));
    }
    if let Some(load) = load {
        parts.push(load_slider(tools, load));
    }
    section(parts)
}

/// What the tool settings are of: the brush, and the medium painting the
/// layer or the preset it is loaded with; or the tool by name.
fn tool_heading(tools: &Tools, medium: Option<&crate::media::MediumSpec>, tool: Tool) -> Element {
    if tool != Tool::Brush {
        return heading(tool.label());
    }
    let title: Element = if let Some(medium) = medium {
        caption(&medium.label)
    } else {
        let preset = tools.preset.clone();
        Box::new(
            Text::dynamic(move || {
                ui::i18n::I18nManager::lookup_plain(&preset.with(|preset| preset.label()))
            })
            .with_role(TextRole::Caption)
            .dimmed(),
        )
    };
    Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(heading("canvas-brush"))
            .push(Box::new(Spacer::new()))
            .push(title),
    )
}

/// How much paint a painted layer's brush carries, starting at `load`.
fn load_slider(tools: &Tools, load: f64) -> Element {
    tools.load.set(load);
    let load_label = {
        let load = tools.load.clone();
        Text::dynamic(move || percent(load.with(|load| *load))).with_role(TextRole::Caption)
    };
    tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-brush-load"),
            Box::new(load_label),
        ),
        Box::new(
            Slider::from_state(&tools.load)
                .with_id("canvas-brush-load")
                .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-brush-load")),
        ),
    ])
}

/// The presets the brush can lay a plain layer's strokes with, drawn in
/// its paint.
fn preset_picker(tools: &Tools) -> Element {
    Box::new(super::brushes::picker(
        &super::brushes::PAINTING,
        tools.preset.with(|preset| *preset),
        {
            let tools = tools.clone();
            move || tools.hex()
        },
        {
            let tools = tools.clone();
            move |preset| tools.choose_preset(preset)
        },
    ))
}

/// A labelled slider over `0..=1` of the person's own tool settings.
fn fraction(label: &str, state: &LocalState<f64>, id: &str) -> Element {
    let shown = {
        let state = state.clone();
        Text::dynamic(move || percent(state.with(|value| *value))).with_role(TextRole::Caption)
    };
    let name = ui::i18n::I18nManager::lookup_plain(label);
    tight(vec![
        labelled(&name, Box::new(shown)),
        Box::new(
            Slider::from_state(state)
                .with_id(id)
                .with_accessible_name(&name),
        ),
    ])
}

/// The dynamics `preset` takes from the person: flow for brushes that lay
/// paint in touches, hardness for those with a soft edge, and for every
/// brush smoothing and taper.
fn dynamics_sliders(tools: &Tools, preset: Preset) -> Element {
    let mut rows = Vec::new();
    if matches!(
        preset,
        Preset::Pencil | Preset::Marker | Preset::Airbrush | Preset::Charcoal
    ) {
        rows.push(fraction(
            "canvas-brush-flow",
            &tools.flow,
            "canvas-brush-flow",
        ));
    }
    if preset.dabbed(&crate::brushes::Dynamics::of(
        preset,
        &std::collections::BTreeMap::new(),
    )) {
        rows.push(fraction(
            "canvas-brush-hardness",
            &tools.hardness,
            "canvas-brush-hardness",
        ));
    }
    rows.push(fraction(
        "canvas-brush-smoothing",
        &tools.smoothing,
        "canvas-brush-smoothing",
    ));
    rows.push(fraction(
        "canvas-brush-taper",
        &tools.taper,
        "canvas-brush-taper",
    ));
    section(rows)
}

/// Where the magic wand or the bucket find colour: in every layer as the
/// artwork shows, or in the chosen layer alone.
fn sampling(choice: &LocalState<Sampling>, heading_id: &str) -> Element {
    let segment = |key: &str| {
        ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(key))
    };
    let choices = vec![
        (Sampling::All, segment("canvas-sample-all")),
        (Sampling::Layer, segment("canvas-sample-layer")),
    ];
    section(vec![
        heading(heading_id),
        Box::new(
            ui::widget::ToggleGroup::from_state(choices, choice).with_id(
                if heading_id == "canvas-sample" {
                    "canvas-sampling"
                } else {
                    "canvas-fill-sampling"
                },
            ),
        ),
    ])
}

/// A selecting tool: how it adds to and takes from the selection, and for
/// the magic wand how alike the colours it takes in are.
fn area_tool(cx: &ViewCx<ArtworkResource>, tools: &Tools, tool: Tool) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut parts: Vec<Element> = vec![heading(tool.label())];
    // The photo's subject in one click, found on this device: off the
    // window's thread, so the window keeps drawing; the canvas says it is
    // under way and applies the area as one step.
    let button = Button::new("canvas-select-subject", &lookup("canvas-select-subject"))
        .with_icon(Icon::Sparkles)
        .tooltip(lookup("canvas-select-subject-hint"));
    let effect = crate::resource::ViewOp::FindArea(crate::resource::FindArea {
        subject: Some(true),
        ..Default::default()
    });
    parts.push(Box::new(
        cx.actions().bind_effect_invocation(button, &effect),
    ));
    if tool == Tool::Wand {
        parts.push(tolerance(tools));
        parts.push(sampling(&tools.sampling, "canvas-sample"));
    }
    parts.push(Box::new(
        Text::new(&lookup(if tool == Tool::Lasso {
            "canvas-lasso-hint"
        } else if tool == Tool::Wand {
            "canvas-wand-hint"
        } else {
            "canvas-area-hint"
        }))
        .with_role(TextRole::Caption)
        .dimmed(),
    ));
    section(parts)
}

/// How unlike a colour the magic wand and the bucket still take in.
fn tolerance(tools: &Tools) -> Element {
    fraction("canvas-tolerance", &tools.tolerance, "canvas-tolerance")
}

/// The eraser: whether it takes paint or whole strokes, its size, and for
/// paint how much it takes and how crisp its edge is.
fn eraser(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let choices = vec![
        (
            Erasing::Paint,
            ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(
                "canvas-erase-paint",
            )),
        ),
        (
            Erasing::Strokes,
            ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(
                "canvas-erase-strokes",
            )),
        ),
    ];
    let mode = ui::widget::ToggleGroup::from_state(choices, &tools.erasing)
        .fill()
        .with_id("canvas-erasing");
    let size_label = {
        let tools = tools.clone();
        Text::dynamic(move || format!("{:.0}", tools.brush_size())).with_role(TextRole::Caption)
    };
    let size = tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-brush-size"),
            Box::new(size_label),
        ),
        Box::new(
            Slider::from_state(&tools.size)
                .with_id("canvas-brush-size")
                .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-brush-size")),
        ),
    ]);
    let paint_only = {
        let tools = tools.clone();
        cx.switch(&tools.erasing.clone(), move |_, erasing| {
            if *erasing == Erasing::Strokes {
                return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
            }
            section(vec![
                fraction(
                    "canvas-eraser-strength",
                    &tools.eraser_strength,
                    "canvas-eraser-strength",
                ),
                fraction(
                    "canvas-eraser-hardness",
                    &tools.eraser_hardness,
                    "canvas-eraser-hardness",
                ),
            ])
        })
    };
    section(vec![
        heading("canvas-eraser"),
        Box::new(mode),
        size,
        paint_only,
    ])
}

/// The paints the brush can load: the medium's pigments, or inks on a
/// plain layer.
fn brush_palette(tools: &Tools, pigments: &[crate::media::PigmentSpec]) -> ColorSwatchPicker {
    if pigments.is_empty() {
        tools.pigment.set(None);
        let current = packed(&tools.hex());
        let chosen = tools.colour.clone();
        return swatches(&INKS, INKS.iter().map(|rgb| hex(*rgb)).collect(), 7)
            .on_change(move |index| {
                if let Some(rgb) = INKS.get(index) {
                    chosen.set(hex(*rgb));
                }
            })
            .selecting(INKS.iter().position(|rgb| Some(*rgb) == current));
    }
    // Keep the pigment when this palette has it too, else load the one
    // nearest the brush's colour: the paint the hand was using, not the
    // palette's first (a white that does not show on a light ground).
    let loaded = tools
        .pigment
        .with(|pigment| pigment.as_ref().map(|(name, _)| name.clone()));
    let index = loaded
        .and_then(|name| pigments.iter().position(|pigment| pigment.name == name))
        .or_else(|| nearest(tools.colour(), pigments))
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
    swatches(&colours, labels, 8)
        .on_change(move |index| {
            if let Some(load) = loads.get(index) {
                pigment.set(Some(load.clone()));
            }
        })
        .selecting(Some(index))
}

/// The pigment whose masstone is nearest `colour`.
fn nearest(
    colour: ui::vello::peniko::Color,
    pigments: &[crate::media::PigmentSpec],
) -> Option<usize> {
    let want = colour.to_rgba8();
    let distance = |pigment: &crate::media::PigmentSpec| {
        let Some(got) = crate::render::colour(&pigment.colour, &[]) else {
            return u32::MAX;
        };
        let got = got.to_rgba8();
        [(want.r, got.r), (want.g, got.g), (want.b, got.b)]
            .into_iter()
            .map(|(a, b)| u32::from(a.abs_diff(b)))
            .fold(0u32, |sum, d| sum.saturating_add(d.saturating_mul(d)))
    };
    pigments
        .iter()
        .enumerate()
        .min_by_key(|(_, pigment)| distance(pigment))
        .map(|(index, _)| index)
}

/// The artwork's title, size and ground.
fn artwork(cx: &ViewCx<ArtworkResource>) -> Element {
    let side = |read: fn(&Artwork) -> f64, action, name: &str| {
        cx.control_with(
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
    let texture = cx.control_with(
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
    let grounds = swatches(&GROUNDS, GROUNDS.iter().map(|rgb| hex(*rgb)).collect(), 7)
        .with_cell(f64::from(SWATCH))
        .selecting(GROUNDS.iter().position(|rgb| Some(*rgb) == ground_now))
        .on_change(move |index| {
            if let Some(rgb) = GROUNDS.get(index) {
                dispatch.invoke(ArtworkResource::SET_GROUND.with(hex(*rgb)));
            }
        });
    section(vec![
        heading("canvas-artwork"),
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
/// The selection's heading, a picture's controls, then the rest. The rest
/// is built again whenever a selected mark changes; a picture's controls
/// follow the artwork in place, built again only when the selection is, so
/// a drag begun as another slider's edit lands is not lost.
fn selection(cx: &ViewCx<ArtworkResource>, ids: Vec<String>, tools: &Tools) -> Element {
    let summary = {
        let ids = ids.clone();
        cx.value(move |art: &Artwork| {
            let marks: Vec<Mark> = ids.iter().filter_map(|id| art.mark(id).cloned()).collect();
            (!marks.is_empty()).then(|| summary(&marks, marks.iter().all(Mark::erases)))
        })
    };
    let heading_row = cx.switch(&summary, |_, summary| {
        let Some(summary) = summary else {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
        };
        Box::new(
            Row::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(heading("canvas-selection"))
                .push(Box::new(Spacer::new()))
                .push(caption(&summary.0)),
        )
    });
    let picture = {
        let ids = ids.clone();
        cx.value(move |art: &Artwork| {
            ids.first()
                .and_then(|id| art.mark(id))
                .is_some_and(|mark| matches!(mark.shape, Shape::Image { .. }))
        })
    };
    let picture_part = {
        let ids = ids.clone();
        let tools = tools.clone();
        cx.switch(&picture, move |cx, picture| {
            if *picture {
                super::picture::panel(cx, &ids, &tools)
            } else {
                Box::new(Spacer::token(ui::tokens::SpacingStep::S0))
            }
        })
    };
    let marks = cx.value(move |art: &Artwork| {
        ids.iter()
            .filter_map(|id| art.mark(id).cloned())
            .collect::<Vec<Mark>>()
    });
    let tools = tools.clone();
    let rest = cx.switch(&marks, move |cx, marks| marks_inspector(cx, marks, &tools));
    spaced(vec![heading_row, picture_part, rest])
}

/// What is picked, in a word: the kind of one mark (an eraser named so),
/// or how many marks; and the one mark's name, when it has one.
fn summary(marks: &[Mark], erasers: bool) -> (String, Option<String>) {
    let [mark] = marks else {
        let count = ui::i18n::I18nManager::format(
            "canvas-marks-selected",
            &[("count", marks.len().into())],
        );
        return (count, None);
    };
    let kind = if erasers {
        ui::i18n::I18nManager::lookup_plain("canvas-eraser")
    } else {
        ui::i18n::I18nManager::lookup_plain(&format!("canvas-kind-{}", mark.shape.kind()))
    };
    (
        kind,
        Some(mark.name.clone()).filter(|name| !name.is_empty()),
    )
}

fn selection_heading(summary: &str) -> Element {
    Box::new(
        Row::new()
            .with_gap(ui::tokens::Spacing::BASE.s2)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(heading("canvas-selection"))
            .push(Box::new(Spacer::new()))
            .push(caption(summary)),
    )
}

fn marks_inspector(cx: &ViewCx<ArtworkResource>, marks: &[Mark], tools: &Tools) -> Element {
    let stop = &tools.stop;
    let Some(first) = marks.first() else {
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
    };
    let ids: Vec<String> = marks.iter().map(|mark| mark.id.clone()).collect();
    let style = &first.style;
    // An eraser has no colour to choose: it takes paint off.
    let erasers = marks.iter().all(Mark::erases);
    let (summary, name) = summary(marks, erasers);
    let palette = cx.read(|art| {
        art.map(|art| crate::media::palette_for(art, &first.layer))
            .unwrap_or_default()
    });
    let mut parts: Vec<Element> = vec![selection_heading(&summary)];
    // The mark's own name under the heading, the kind beside it.
    if let Some(name) = name {
        parts.push(Box::new(Text::new(&name).with_role(TextRole::Body)));
    }
    let picture = matches!(first.shape, Shape::Image { .. });
    if picture {
        parts.push(super::picture::panel(cx, &ids, tools));
    }
    let lines_only = matches!(
        first.shape,
        Shape::Line { .. } | Shape::Stroke { .. } | Shape::Image { .. }
    );
    if !lines_only {
        let mut fill = vec![
            heading("canvas-fill"),
            super::gradient::editor(cx, &ids, first, stop),
        ];
        match &style.gradient {
            // The swatches recolour the chosen stop, built again when
            // another stop is chosen so they show its colour.
            Some(gradient) => {
                let gradient = gradient.clone();
                let ids = ids.clone();
                let palette = palette.clone();
                fill.push(cx.switch(stop, move |cx, index| {
                    let index = (*index).min(gradient.stops.len().saturating_sub(1));
                    let current = gradient.stops.get(index).map(|stop| stop.1.clone());
                    paints(
                        cx,
                        &ids,
                        current.as_deref(),
                        &palette,
                        &Paint::Stop(gradient.clone(), index),
                    )
                }));
            }
            None => fill.push(paints(
                cx,
                &ids,
                style.fill.as_deref(),
                &palette,
                &Paint::Fill,
            )),
        }
        parts.push(section(fill));
    }
    let strokes = marks
        .iter()
        .all(|mark| matches!(mark.shape, Shape::Stroke { .. }));
    if strokes {
        let plain = cx.read(|art| {
            art.and_then(|art| art.layer(&first.layer))
                .is_none_or(|layer| layer.medium.is_empty())
        });
        // A brush stroke's paint is the brush's, not an outline round it.
        let mut stroke = vec![heading(if erasers {
            "canvas-tool-eraser"
        } else {
            "canvas-brush"
        })];
        if !erasers {
            stroke.push(paints(
                cx,
                &ids,
                style.stroke.as_deref(),
                &palette,
                &Paint::Stroke,
            ));
        }
        stroke.push(stroke_brush(cx, &ids, first, plain));
        parts.push(section(stroke));
    } else {
        parts.push(section(vec![
            heading("canvas-stroke"),
            paints(cx, &ids, style.stroke.as_deref(), &palette, &Paint::Stroke),
            stroke_style(cx, &ids, first),
        ]));
    }
    parts.push(appearance(cx, &ids, first));
    if let (Shape::Text { .. }, [mark]) = (&first.shape, marks) {
        parts.push(text_style(cx, mark));
    }
    parts.push(arrange(cx, &ids));
    spaced(parts)
}

/// What a row of swatches paints.
#[derive(Clone)]
enum Paint {
    Fill,
    Stroke,
    /// One stop of the fill's gradient, by index.
    Stop(crate::model::Gradient, usize),
}

impl Paint {
    fn id(&self) -> &'static str {
        match self {
            Self::Fill => "canvas-fill",
            Self::Stroke => "canvas-stroke",
            Self::Stop(..) => "canvas-gradient-stop",
        }
    }

    /// `value` painted by the marks `ids`, as one invocation.
    fn invoke(&self, ids: &[String], value: String) -> Invoke<ArtworkResource> {
        match self {
            Self::Fill => ArtworkResource::SET_FILL.with((ids.to_vec(), value)),
            Self::Stroke => ArtworkResource::SET_STROKE.with((ids.to_vec(), value)),
            Self::Stop(gradient, index) => {
                let mut gradient = gradient.clone();
                if let Some(stop) = gradient.stops.get_mut(*index) {
                    stop.1 = value;
                }
                ArtworkResource::SET_GRADIENT.with((ids.to_vec(), gradient))
            }
        }
    }
}

/// Swatches of the layer's paints (or inks) and the colour as text.
fn paints(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    current: Option<&str>,
    palette: &[tilo_wet_media::Pigment],
    paint: &Paint,
) -> Element {
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
    let picked = paint.clone();
    let swatches = self::swatches(&colours, names, columns)
        .with_cell(f64::from(SWATCH))
        .selecting(selected)
        .on_change(move |index| {
            if let Some(value) = chosen.get(index) {
                dispatch.invoke(picked.invoke(&targets, value.clone()));
            }
        });
    let text = LocalState::new(current.unwrap_or("none").to_owned());
    let input = TextInput::from_state(&text)
        .with_id(paint.id())
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain(paint.id()));
    let field: Element = match paint {
        Paint::Fill => Box::new(cx.actions().bind(
            input,
            ArtworkResource::SET_FILL.map({
                let ids = ids.to_vec();
                move |value: String| (ids.clone(), value)
            }),
        )),
        Paint::Stroke => Box::new(cx.actions().bind(
            input,
            ArtworkResource::SET_STROKE.map({
                let ids = ids.to_vec();
                move |value: String| (ids.clone(), value)
            }),
        )),
        Paint::Stop(gradient, index) => Box::new(cx.actions().bind(
            input,
            ArtworkResource::SET_GRADIENT.map({
                let ids = ids.to_vec();
                let gradient = gradient.clone();
                let index = *index;
                move |value: String| {
                    let mut gradient = gradient.clone();
                    if let Some(stop) = gradient.stops.get_mut(index) {
                        stop.1 = value;
                    }
                    (ids.clone(), gradient)
                }
            }),
        )),
    };
    let mut parts: Vec<Element> = Vec::new();
    if palette.is_empty() {
        let dispatch = cx.dispatcher();
        let ids = ids.to_vec();
        let paint = paint.clone();
        let current = current.unwrap_or("none").to_owned();
        parts.push(artwork_palette(
            cx,
            format!("{}-artwork-colours", paint.id()),
            move || current.clone(),
            move |value| {
                dispatch.invoke(paint.invoke(&ids, value));
            },
        ));
    } else {
        parts.push(Box::new(swatches));
    }
    parts.push(field);
    section(parts)
}

/// Brush strokes' preset, width and dynamics, and their paths: smoothed,
/// simplified, joined.
fn stroke_brush(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark, plain: bool) -> Element {
    let Shape::Stroke { size, .. } = &mark.shape else {
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
    };
    let mut rows: Vec<Element> = Vec::new();
    let preset = Preset::of(&mark.style);
    if plain && !preset.erases() {
        let dispatch = cx.dispatcher();
        let targets = ids.to_vec();
        let ink = mark.style.stroke.clone().unwrap_or_default();
        rows.push(Box::new(super::brushes::picker(
            &super::brushes::PAINTING,
            preset,
            move || ink.clone(),
            move |preset| {
                dispatch.invoke(
                    ArtworkResource::SET_BRUSH.with((targets.clone(), preset.name().to_owned())),
                );
            },
        )));
    }
    let width = LocalState::new(*size);
    let width = cx.actions().bind(
        NumberInput::from_state(&width)
            .with_range(0.5, 400.0)
            .with_step(1.0)
            .with_precision(1)
            .with_id("canvas-stroke-size")
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-brush-size")),
        ArtworkResource::SET_SIZE.map({
            let ids = ids.to_vec();
            move |value: f64| (ids.clone(), value)
        }),
    );
    rows.push(labelled(
        &ui::i18n::I18nManager::lookup_plain("canvas-brush-size"),
        Box::new(width),
    ));
    if plain {
        let dynamics = crate::brushes::Dynamics::of_mark(mark);
        let mut tuned = vec![
            ("flow", dynamics.flow),
            ("smoothing", dynamics.smoothing),
            ("taper", dynamics.taper),
        ];
        if preset.dabbed(&dynamics) {
            tuned.insert(1, ("hardness", dynamics.hardness));
        }
        for (name, value) in tuned {
            rows.push(dynamic(cx, ids, name, value));
        }
    }
    rows.push(path_tools(cx, ids, *size));
    section(rows)
}

/// A slider over one of strokes' dynamics, applied when let go.
fn dynamic(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    name: &'static str,
    value: f64,
) -> Element {
    let state = LocalState::new(value);
    let label = format!("canvas-brush-{name}");
    let shown = {
        let state = state.clone();
        Text::dynamic(move || percent(state.with(|value| *value))).with_role(TextRole::Caption)
    };
    let slider = cx.actions().bind(
        Slider::from_state(&state)
            .with_id(&format!("canvas-stroke-{name}"))
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(&label))
            .commits_on_release(),
        ArtworkResource::SET_PARAMS.map({
            let ids = ids.to_vec();
            move |value: f64| {
                (
                    ids.clone(),
                    std::collections::BTreeMap::from([(
                        name.to_owned(),
                        (value * 100.0).round() / 100.0,
                    )]),
                )
            }
        }),
    );
    tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain(&label),
            Box::new(shown),
        ),
        Box::new(slider),
    ])
}

/// Smooth or simplify strokes' paths, or join them into one.
fn path_tools(cx: &ViewCx<ArtworkResource>, ids: &[String], size: f64) -> Element {
    let lookup = |id: &str| ui::i18n::I18nManager::lookup_plain(id);
    let mut row = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .no_wrap()
        .push(Box::new(cx.button(
            lookup("canvas-stroke-smooth"),
            ArtworkResource::SMOOTH.with((ids.to_vec(), 0.5)),
        )))
        .push(Box::new(cx.button(
            lookup("canvas-stroke-simplify"),
            ArtworkResource::SIMPLIFY.with((ids.to_vec(), (size * 0.15).clamp(0.5, 6.0))),
        )));
    if ids.len() > 1 {
        row = row.push(Box::new(cx.button(
            lookup("canvas-stroke-join"),
            ArtworkResource::MERGE.with((ids.to_vec(), true)),
        )));
    }
    section(vec![heading("canvas-stroke-path"), Box::new(row)])
}

/// The outline's width, dashes, ends and corners.
fn stroke_style(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark) -> Element {
    let style = &mark.style;
    let width = LocalState::new(style.stroke_width);
    let width = cx.actions().bind(
        NumberInput::from_state(&width)
            .with_range(0.0, 400.0)
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
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-cap"),
            grow(cap),
        ),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-join"),
            grow(join),
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
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
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
                .with_gap(ui::tokens::Spacing::BASE.s2)
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
            .with_gap(ui::tokens::Spacing::BASE.s1)
            .no_wrap()
            .push(to(Icon::ArrowUp, "canvas-forward", "forward"))
            .push(to(Icon::ArrowDown, "canvas-backward", "backward"))
            .push(to(Icon::ChevronUp, "canvas-front", "front"))
            .push(to(Icon::ChevronDown, "canvas-back", "back"))
            .push(Box::new(Spacer::new()))
            .push(Box::new(icon_button(
                cx,
                Icon::Trash2,
                &ui::i18n::I18nManager::lookup_plain("canvas-remove-marks"),
                ArtworkResource::REMOVE_MARKS.with(ids.to_vec()),
            ))),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixtures")]

    use super::*;

    /// A brush moved onto a painted layer loads the pigment nearest the
    /// colour it held, not the palette's first.
    #[test]
    fn a_painted_layer_loads_the_pigment_nearest_the_brush() {
        let oil = crate::media::builtin()
            .into_iter()
            .find(|medium| medium.engine == "oil" && !medium.palette.is_empty())
            .unwrap();
        let blue = ui::vello::peniko::Color::from_rgb8(0x2b, 0x3f, 0x8f);
        let index = nearest(blue, &oil.palette).unwrap();
        let chosen = &oil.palette[index].name;
        assert!(
            chosen.contains("ultramarine") || chosen.contains("blue"),
            "{chosen}"
        );
        assert_ne!(index, 0, "not simply the first pigment");
    }
}
