//! The inspector's part for a picture: cropping it on the canvas, fitting
//! the artwork to it, flipping and straightening it, and its colour and
//! tone adjustments. Each slider shows its value on the canvas while it is
//! dragged and becomes one edit, one step of history, when let go.

use std::rc::Rc;

use ui::prelude::*;
use ui::widget::ButtonRole;
use ui::widget::Toggle;

use super::heading;
use super::inspector::{grow, labelled, section, tight};
use crate::model::{Adjust, Artwork, Shape};
use crate::tools::{Live, Tools};

/// One adjustment a slider sets: its name, range and how it reads.
struct Step {
    /// The Fluent id of its name, and its control's id.
    id: &'static str,
    low: f64,
    high: f64,
    read: fn(&Adjust) -> Option<f64>,
    write: fn(&mut Adjust, f64),
    /// Decimals the value is kept to.
    places: i32,
}

static LIGHT: [Step; 4] = [
    Step {
        id: "canvas-adjust-exposure",
        low: -3.0,
        high: 3.0,
        read: |a| a.exposure,
        write: |a, v| a.exposure = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-contrast",
        low: -1.0,
        high: 1.0,
        read: |a| a.contrast,
        write: |a, v| a.contrast = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-highlights",
        low: -1.0,
        high: 1.0,
        read: |a| a.highlights,
        write: |a, v| a.highlights = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-shadows",
        low: -1.0,
        high: 1.0,
        read: |a| a.shadows,
        write: |a, v| a.shadows = Some(v),
        places: 2,
    },
];

static COLOUR: [Step; 5] = [
    Step {
        id: "canvas-adjust-temperature",
        low: -1.0,
        high: 1.0,
        read: |a| a.temperature,
        write: |a, v| a.temperature = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-tint",
        low: -1.0,
        high: 1.0,
        read: |a| a.tint,
        write: |a, v| a.tint = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-saturation",
        low: -1.0,
        high: 1.0,
        read: |a| a.saturation,
        write: |a, v| a.saturation = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-vibrance",
        low: -1.0,
        high: 1.0,
        read: |a| a.vibrance,
        write: |a, v| a.vibrance = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-hue",
        low: -180.0,
        high: 180.0,
        read: |a| a.hue,
        write: |a, v| a.hue = Some(v),
        places: 0,
    },
];

static EFFECTS: [Step; 3] = [
    Step {
        id: "canvas-adjust-blur",
        low: 0.0,
        high: 40.0,
        read: |a| a.blur,
        write: |a, v| a.blur = Some(v),
        places: 1,
    },
    Step {
        id: "canvas-adjust-sharpen",
        low: 0.0,
        high: 1.0,
        read: |a| a.sharpen,
        write: |a, v| a.sharpen = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-vignette",
        low: -1.0,
        high: 1.0,
        read: |a| a.vignette,
        write: |a, v| a.vignette = Some(v),
        places: 2,
    },
];

impl Step {
    /// The value at `travel` (0-1) along the slider.
    fn at(&self, travel: f64) -> f64 {
        let value = self.low + travel.clamp(0.0, 1.0) * (self.high - self.low);
        let scale = 10_f64.powi(self.places);
        (value * scale).round() / scale
    }

    /// Where `value` sits along the slider.
    fn travel(&self, value: f64) -> f64 {
        ((value - self.low) / (self.high - self.low)).clamp(0.0, 1.0)
    }

    fn label(&self, value: f64) -> String {
        let places = usize::try_from(self.places).unwrap_or(0);
        if self.low < 0.0 && value > 0.0 {
            format!("+{value:.places$}")
        } else {
            format!("{value:.places$}")
        }
    }
}

/// What an adjustment control changes: the marks selected, or a layer.
#[derive(Clone)]
pub enum Target {
    Marks(Vec<String>),
    Layer(String),
}

impl Target {
    /// `edit` of the target's adjustments, as one invocation.
    fn invoke(&self, edit: Adjust) -> Invoke<Artwork> {
        match self {
            Self::Marks(ids) => Artwork::SET_ADJUST.with((ids.clone(), edit)),
            Self::Layer(id) => Artwork::SET_LAYER_ADJUST.with((id.clone(), Box::new(edit))),
        }
    }

    /// `control` bound to editing the target's adjustments with the
    /// edit `edit` makes of the control's value.
    fn bind<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        cx: &ViewCx<Artwork>,
        control: C,
        edit: impl Fn(I) -> Adjust + Send + Sync + 'static,
    ) -> C {
        match self {
            Self::Marks(ids) => {
                let ids = ids.clone();
                cx.actions().bind(
                    control,
                    Artwork::SET_ADJUST.map(move |value: I| (ids.clone(), edit(value))),
                )
            }
            Self::Layer(id) => {
                let id = id.clone();
                cx.actions().bind(
                    control,
                    Artwork::SET_LAYER_ADJUST
                        .map(move |value: I| (id.clone(), Box::new(edit(value)))),
                )
            }
        }
    }

    /// The target's adjustments as `art` has them: its first mark's, or
    /// its layer's.
    fn adjust_in(&self, art: &Artwork) -> Adjust {
        let adjust = match self {
            Self::Marks(ids) => ids
                .first()
                .and_then(|id| art.mark(id))
                .and_then(|mark| mark.style.adjust.clone()),
            Self::Layer(id) => art.layer(id).and_then(|layer| layer.adjust.clone()),
        };
        adjust.unwrap_or_default()
    }

    /// The target's adjustments in `art`, with `change` made, as the
    /// canvas shows them while a control is dragged.
    fn preview(&self, art: &mut Artwork, change: &dyn Fn(&mut Adjust)) {
        let changed = |adjust: &mut Option<Adjust>| {
            let mut edited = adjust.clone().unwrap_or_default();
            change(&mut edited);
            *adjust = edited.normalized();
        };
        match self {
            Self::Marks(ids) => {
                for mark in art.marks.iter_mut().filter(|mark| ids.contains(&mark.id)) {
                    changed(&mut mark.style.adjust);
                }
            }
            Self::Layer(id) => {
                for layer in art.layers.iter_mut().filter(|layer| layer.id == *id) {
                    changed(&mut layer.adjust);
                }
            }
        }
    }
}

/// The picture's part of the inspector, for the marks `ids` (a picture
/// first). Its controls follow the artwork in place rather than being
/// built again at each edit, so a drag begun as an edit lands is kept.
pub fn panel(cx: &ViewCx<Artwork>, ids: &[String], tools: &Tools) -> Element {
    tools.live.borrow_mut().clear();
    let first = ids.first().cloned().unwrap_or_default();
    let pixelated =
        cx.value(move |art: &Artwork| art.mark(&first).is_some_and(|mark| mark.style.pixelated));
    let sampling = cx.actions().bind(
        Toggle::from_state(&pixelated)
            .with_id("canvas-picture-pixelated")
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(
                "canvas-picture-pixelated",
            )),
        Artwork::SET_PIXELATED.map({
            let ids = ids.to_vec();
            move |on: bool| (ids.clone(), on)
        }),
    );
    section(vec![
        heading("canvas-picture"),
        labelled(
            &ui::i18n::I18nManager::lookup_plain("canvas-picture-pixelated"),
            Box::new(sampling),
        ),
        framing(cx, ids, tools),
        straighten(cx, ids, tools),
        adjustments(cx, &Target::Marks(ids.to_vec()), tools),
    ])
}

/// Colour and tone of `target`: light, colour and effects, and starting
/// over.
pub fn adjustments(cx: &ViewCx<Artwork>, target: &Target, tools: &Tools) -> Element {
    let reset = cx
        .button("canvas-adjust-reset", target.invoke(Adjust::default()))
        .role(ButtonRole::Plain);
    let title = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(heading("canvas-adjust"))
        .push(Box::new(Spacer::new()))
        .push(Box::new(reset));
    let mut parts: Vec<Element> = vec![Box::new(title)];
    for (group, steps) in [
        ("canvas-adjust-light", &LIGHT[..]),
        ("canvas-adjust-colour", &COLOUR[..]),
        ("canvas-adjust-effects", &EFFECTS[..]),
    ] {
        parts.push(Box::new(
            Text::new(group).with_role(TextRole::Caption).dimmed(),
        ));
        for step in steps {
            parts.push(slider(cx, target, step, tools));
        }
        if group == "canvas-adjust-colour" {
            parts.push(flag(
                cx,
                target,
                "canvas-adjust-grayscale",
                |a| a.grayscale,
                |a, on| a.grayscale = Some(on),
            ));
            parts.push(flag(
                cx,
                target,
                "canvas-adjust-invert",
                |a| a.invert,
                |a, on| a.invert = Some(on),
            ));
        }
    }
    section(parts)
}

/// Crop on the canvas, fit the artwork to the picture, flip it.
fn framing(cx: &ViewCx<Artwork>, ids: &[String], tools: &Tools) -> Element {
    let first = ids.first().cloned().unwrap_or_default();
    let crop = {
        let crop = tools.crop.clone();
        cx.switch(&tools.crop, move |_, cropping| {
            let id = if *cropping {
                "canvas-crop-done"
            } else {
                "canvas-crop"
            };
            let name = ui::i18n::I18nManager::lookup_plain(id);
            let flip = crop.clone();
            let button = Button::new(id, &name)
                .tooltip(ui::i18n::I18nManager::lookup_plain("canvas-crop-hint"))
                .on_view(move || flip.set(!flip.with(|on| *on)));
            Box::new(if *cropping {
                button.role(ButtonRole::Primary)
            } else {
                button
            }) as Element
        })
    };
    let fit = cx
        .button("canvas-fit-artwork", Artwork::FIT_ARTWORK.with(first))
        .tooltip(ui::i18n::I18nManager::lookup_plain(
            "canvas-fit-artwork-hint",
        ));
    let flip = |id: &str, way: &str| {
        Box::new(
            cx.button(id, Artwork::FLIP.with((ids.to_vec(), way.to_owned())))
                .role(ButtonRole::Plain),
        ) as Element
    };
    let row = |children: Vec<Element>| {
        let mut row = Row::new()
            .with_gap(ui::tokens::Spacing::BASE.s2)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center);
        for child in children {
            row = row.push(grow(child));
        }
        Box::new(row) as Element
    };
    section(vec![
        row(vec![crop, Box::new(fit)]),
        row(vec![
            flip("canvas-flip-horizontal", "horizontal"),
            flip("canvas-flip-vertical", "vertical"),
        ]),
    ])
}

/// The picture's turn inside its frame, -45° to 45°: dragged (a tenth as
/// fast with Shift held) or typed to a tenth of a degree.
fn straighten(cx: &ViewCx<Artwork>, ids: &[String], tools: &Tools) -> Element {
    const ID: &str = "canvas-straighten";
    let first = ids.first().cloned().unwrap_or_default();
    let angle_in = move |art: &Artwork| match art.mark(&first).map(|mark| &mark.shape) {
        Some(Shape::Image { straighten, .. }) => straighten.unwrap_or(0.0),
        _ => 0.0,
    };
    let to_value = |travel: f64| {
        let value = -45.0 + travel.clamp(0.0, 1.0) * 90.0;
        (value * 10.0).round() / 10.0
    };
    let travel_in = {
        let angle_in = angle_in.clone();
        move |art: &Artwork| ((angle_in(art) + 45.0) / 90.0).clamp(0.0, 1.0)
    };
    let state = cx.value(travel_in.clone());
    let turned = ids.to_vec();
    tools.live.borrow_mut().push(Live {
        state: state.clone(),
        committed: cx.value(travel_in),
        apply: Rc::new(move |art: &mut Artwork, travel| {
            for mark in art
                .marks
                .iter_mut()
                .filter(|mark| turned.contains(&mark.id))
            {
                if let Some(mut placement) = crate::picture::Placement::of(&mark.shape) {
                    placement.straighten = to_value(travel);
                    placement.apply(&mut mark.shape);
                }
            }
        }),
    });
    let typed = cx.actions().bind(
        NumberInput::from_state(&cx.value(angle_in))
            .with_range(-45.0, 45.0)
            .control_size(ui::widget::ControlSize::Regular)
            .with_step(0.1)
            .with_precision(1)
            .with_id("canvas-straighten-degrees")
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain(ID)),
        Artwork::SET_STRAIGHTEN.map({
            let ids = ids.to_vec();
            move |value: f64| (ids.clone(), (value * 10.0).round() / 10.0)
        }),
    );
    let slider = cx.actions().bind(
        Slider::from_state(&state)
            .with_id(ID)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(ID))
            .commits_on_release(),
        Artwork::SET_STRAIGHTEN.map({
            let ids = ids.to_vec();
            move |travel: f64| (ids.clone(), to_value(travel))
        }),
    );
    tight(vec![
        labelled(&ui::i18n::I18nManager::lookup_plain(ID), Box::new(typed)),
        Box::new(slider),
    ])
}

/// A slider for one adjustment of `target`, following the artwork.
fn slider(cx: &ViewCx<Artwork>, target: &Target, step: &'static Step, tools: &Tools) -> Element {
    let travel_in = {
        let target = target.clone();
        move |art: &Artwork| step.travel((step.read)(&target.adjust_in(art)).unwrap_or(0.0))
    };
    let state = cx.value(travel_in.clone());
    let previewed = target.clone();
    tools.live.borrow_mut().push(Live {
        state: state.clone(),
        committed: cx.value(travel_in),
        apply: Rc::new(move |art: &mut Artwork, travel| {
            previewed.preview(art, &|adjust| (step.write)(adjust, step.at(travel)));
        }),
    });
    let shown = {
        let state = state.clone();
        Text::dynamic(move || step.label(step.at(state.with(|t| *t)))).with_role(TextRole::Caption)
    };
    let control = target.bind(
        cx,
        Slider::from_state(&state)
            .with_id(step.id)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(step.id))
            .commits_on_release(),
        move |travel: f64| {
            let mut edit = Adjust::default();
            (step.write)(&mut edit, step.at(travel));
            edit
        },
    );
    tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain(step.id),
            Box::new(shown),
        ),
        Box::new(control),
    ])
}

/// A switch for an adjustment of `target` that is on or off, following
/// the artwork.
fn flag(
    cx: &ViewCx<Artwork>,
    target: &Target,
    id: &str,
    read: fn(&Adjust) -> Option<bool>,
    write: fn(&mut Adjust, bool),
) -> Element {
    let state = {
        let target = target.clone();
        cx.value(move |art: &Artwork| read(&target.adjust_in(art)) == Some(true))
    };
    let toggle = target.bind(
        cx,
        Toggle::from_state(&state)
            .with_id(id)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(id)),
        move |on: bool| {
            let mut edit = Adjust::default();
            write(&mut edit, on);
            edit
        },
    );
    labelled(&ui::i18n::I18nManager::lookup_plain(id), Box::new(toggle))
}
